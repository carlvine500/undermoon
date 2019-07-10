extern crate tokio;
extern crate undermoon;
#[macro_use]
extern crate log;
extern crate config;
extern crate env_logger;

use std::env;
use std::sync::Arc;
use std::time::Duration;
use undermoon::protocol::PooledRedisClientFactory;
use undermoon::proxy::executor::SharedForwardHandler;
use undermoon::proxy::service::{ServerProxyConfig, ServerProxyService};
use undermoon::proxy::slowlog::SlowRequestLogger;

fn gen_conf() -> ServerProxyConfig {
    let conf_file_path = env::args()
        .nth(1)
        .unwrap_or_else(|| "server-proxy.toml".to_string());

    let mut s = config::Config::new();
    s.merge(config::File::with_name(&conf_file_path))
        .map(|_| ())
        .unwrap_or_else(|e| warn!("failed to read config file: {:?}", e));
    // e.g. UNDERMOON_ADDRESS='127.0.0.1:5299'
    s.merge(config::Environment::with_prefix("undermoon"))
        .map(|_| ())
        .unwrap_or_else(|e| warn!("failed to read address from env vars {:?}", e));

    ServerProxyConfig {
        address: s
            .get::<String>("address")
            .unwrap_or_else(|_| "127.0.0.1:5299".to_string()),
        auto_select_db: s.get::<bool>("auto_select_db").unwrap_or_else(|_| false),
        slowlog_len: s.get::<usize>("slowlog_len").unwrap_or_else(|_| 1024),
    }
}

fn main() {
    env_logger::init();

    let config = gen_conf();

    let timeout = Duration::new(1, 0);
    let pool_size = 1;
    let client_factory = PooledRedisClientFactory::new(pool_size, timeout);

    let slow_request_logger = Arc::new(SlowRequestLogger::new(config.slowlog_len));
    let forward_handler = SharedForwardHandler::new(
        config.clone(),
        Arc::new(client_factory),
        slow_request_logger.clone(),
    );
    let server = ServerProxyService::new(config, forward_handler, slow_request_logger);

    tokio::run(server.run());
}
