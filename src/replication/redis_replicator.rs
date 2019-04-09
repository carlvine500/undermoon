use super::replicator::{
    MasterMeta, MasterReplicator, ReplicaMeta, ReplicaReplicator, ReplicatorError,
};
use atomic_option::AtomicOption;
use common::utils::revolve_first_address;
use futures::sync::oneshot;
use futures::{future, stream, Future, Stream};
use futures_timer::Delay;
use protocol::{RedisClient, RedisClientError, Resp, SimpleRedisClient};
use std::iter;
use std::str;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub struct RedisMasterReplicator {
    meta: MasterMeta,
}

impl RedisMasterReplicator {
    pub fn new(meta: MasterMeta) -> Self {
        Self { meta }
    }
}

impl MasterReplicator for RedisMasterReplicator {
    fn start(&self) -> Box<Future<Item = (), Error = ReplicatorError> + Send> {
        Box::new(future::ok(()))
    }

    fn stop(&self) -> Box<Future<Item = (), Error = ReplicatorError> + Send> {
        Box::new(future::ok(()))
    }

    fn start_migrating(&self) -> Box<Future<Item = (), Error = ReplicatorError> + Send> {
        // TODO: implement migration
        Box::new(future::ok(()))
    }

    fn commit_migrating(&self) -> Box<Future<Item = (), Error = ReplicatorError> + Send> {
        // TODO: implement migration
        Box::new(future::ok(()))
    }

    fn get_meta(&self) -> &MasterMeta {
        &self.meta
    }
}

pub struct RedisReplicaReplicator {
    meta: ReplicaMeta,
    stop_channel: AtomicOption<oneshot::Sender<()>>,
}

impl RedisReplicaReplicator {
    pub fn new(meta: ReplicaMeta) -> Self {
        Self {
            meta,
            stop_channel: AtomicOption::empty(),
        }
    }

    fn send_stop_signal(&self) -> Result<(), ReplicatorError> {
        if let Some(sender) = self.stop_channel.take(Ordering::SeqCst) {
            sender.send(()).map_err(|()| {
                error!("failed to send stop signal");
                ReplicatorError::Canceled
            })
        } else {
            Err(ReplicatorError::AlreadyEnded)
        }
    }
}

impl ReplicaReplicator for RedisReplicaReplicator {
    fn start(&self) -> Box<Future<Item = (), Error = ReplicatorError> + Send> {
        let (sender, receiver) = oneshot::channel();
        if self
            .stop_channel
            .try_store(Box::new(sender), Ordering::SeqCst)
            .is_some()
        {
            return Box::new(future::err(ReplicatorError::AlreadyStarted));
        }

        let address = match revolve_first_address(&self.meta.master_node_address) {
            Some(address) => address,
            None => return Box::new(future::err(ReplicatorError::InvalidAddress)),
        };
        let host = address.ip().to_string();
        let port = address.port().to_string();

        let client = SimpleRedisClient::new();
        let interval = Duration::new(5, 0);
        let cmd = vec!["SLAVEOF".to_string(), host, port];
        let send_fut = keep_sending_cmd(
            client,
            self.meta.replica_node_address.clone(),
            cmd,
            interval,
        );

        let meta = self.meta.clone();

        Box::new(
            receiver
                .map_err(|_| ReplicatorError::Canceled)
                .select(send_fut.map_err(ReplicatorError::RedisError))
                .then(move |_| {
                    warn!("RedisReplicaReplicator {:?} stopped", meta);
                    future::ok(())
                }),
        )
    }

    fn stop(&self) -> Box<Future<Item = (), Error = ReplicatorError> + Send> {
        Box::new(future::result(self.send_stop_signal()))
    }

    fn start_importing(&self) -> Box<Future<Item = (), Error = ReplicatorError> + Send> {
        // TODO: implement migration
        Box::new(future::ok(()))
    }

    fn commit_importing(&self) -> Box<Future<Item = (), Error = ReplicatorError> + Send> {
        // TODO: implement migration
        Box::new(future::ok(()))
    }

    fn get_meta(&self) -> &ReplicaMeta {
        &self.meta
    }
}

// Make sure the future will end.
impl Drop for RedisReplicaReplicator {
    fn drop(&mut self) {
        self.send_stop_signal().unwrap_or(())
    }
}

fn keep_sending_cmd<C: RedisClient>(
    client: C,
    address: String,
    cmd: Vec<String>,
    interval: Duration,
) -> impl Future<Item = (), Error = RedisClientError> {
    let infinite_stream = stream::iter_ok(iter::repeat(()));
    infinite_stream
        .fold(client, move |client, ()| {
            let byte_cmd = cmd.iter().map(|s| s.clone().into_bytes()).collect();
            let delay = Delay::new(interval).map_err(RedisClientError::Io);
            let exec_fut = client
                .execute(address.clone(), byte_cmd)
                .map_err(|e| {
                    error!("failed to send: {}", e);
                    e
                })
                .map(|response| {
                    if let Resp::Error(err) = response {
                        let err_str = str::from_utf8(&err)
                            .map(|s| s.to_string())
                            .unwrap_or_else(|_| format!("{:?}", err));
                        error!("error reply: {}", err_str);
                    }
                });
            exec_fut.join(delay).map(move |_| client)
        })
        .map(|_| ())
}