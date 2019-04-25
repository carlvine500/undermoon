use super::cluster::{SlotRange, SlotRangeTag};
use super::utils::{has_flags, CmdParseError};
use protocol::{Array, BulkStr, Resp};
use std::collections::HashMap;
use std::iter::Peekable;
use std::str;

const MIGRATING_TAG: &str = "MIGRATING";
const IMPORTING_TAG: &str = "IMPORTING";

#[derive(Debug, Clone, PartialEq)]
pub struct DBMapFlags {
    pub force: bool,
}

impl DBMapFlags {
    pub fn to_arg(&self) -> String {
        if self.force {
            "FORCE".to_string()
        } else {
            "NOFLAG".to_string()
        }
    }

    pub fn from_arg(flags_str: &str) -> Self {
        let force = has_flags(flags_str, ',', "FORCE");
        DBMapFlags { force }
    }
}

#[derive(Debug)]
pub struct HostDBMap {
    epoch: u64,
    flags: DBMapFlags,
    db_map: HashMap<String, HashMap<String, Vec<SlotRange>>>,
}

macro_rules! try_parse {
    ($expression:expr) => {{
        match $expression {
            Ok(v) => (v),
            Err(_) => return Err(CmdParseError {}),
        }
    }};
}

macro_rules! try_get {
    ($expression:expr) => {{
        match $expression {
            Some(v) => (v),
            None => return Err(CmdParseError {}),
        }
    }};
}

impl HostDBMap {
    pub fn new(
        epoch: u64,
        flags: DBMapFlags,
        db_map: HashMap<String, HashMap<String, Vec<SlotRange>>>,
    ) -> Self {
        Self {
            epoch,
            flags,
            db_map,
        }
    }

    pub fn get_epoch(&self) -> u64 {
        self.epoch
    }

    pub fn get_flags(&self) -> DBMapFlags {
        self.flags.clone()
    }

    pub fn into_map(self) -> HashMap<String, HashMap<String, Vec<SlotRange>>> {
        self.db_map
    }

    pub fn db_map_to_args(&self) -> Vec<String> {
        let mut args = vec![];
        for (db_name, node_map) in &self.db_map {
            for (node, slot_ranges) in node_map {
                for slot_range in slot_ranges {
                    args.push(db_name.clone());
                    args.push(node.clone());
                    match &slot_range.tag {
                        SlotRangeTag::Migrating(ref dst) => {
                            args.push("migrating".to_string());
                            args.push(dst.clone());
                        }
                        SlotRangeTag::Importing(ref src) => {
                            args.push("importing".to_string());
                            args.push(src.clone());
                        }
                        SlotRangeTag::None => (),
                    };
                    args.push(format!("{}-{}", slot_range.start, slot_range.end));
                }
            }
        }
        args
    }

    pub fn from_resp(resp: &Resp) -> Result<Self, CmdParseError> {
        let arr = match resp {
            Resp::Arr(Array::Arr(ref arr)) => arr,
            _ => return Err(CmdParseError {}),
        };

        // Skip the "UMCTL SET_DB|SET_REMOTE"
        let it = arr.iter().skip(2).flat_map(|resp| match resp {
            Resp::Bulk(BulkStr::Str(safe_str)) => match str::from_utf8(safe_str) {
                Ok(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        });
        let mut it = it.peekable();

        Self::parse(&mut it)
    }

    fn parse<It>(it: &mut Peekable<It>) -> Result<Self, CmdParseError>
    where
        It: Iterator<Item = String>,
    {
        let epoch_str = try_get!(it.next());
        let epoch = try_parse!(epoch_str.parse::<u64>());

        let flags = DBMapFlags::from_arg(&try_get!(it.next()));

        let mut db_map = HashMap::new();

        while let Some(_) = it.peek() {
            let (dbname, address, slot_range) = try_parse!(Self::parse_db(it));
            let db = db_map.entry(dbname).or_insert_with(HashMap::new);
            let slots = db.entry(address).or_insert_with(Vec::new);
            slots.push(slot_range);
        }

        Ok(Self {
            epoch,
            flags,
            db_map,
        })
    }

    fn parse_db<It>(it: &mut It) -> Result<(String, String, SlotRange), CmdParseError>
    where
        It: Iterator<Item = String>,
    {
        let dbname = try_get!(it.next());
        let addr = try_get!(it.next());
        let slot_range = try_parse!(Self::parse_tagged_slot_range(it));
        Ok((dbname, addr, slot_range))
    }

    fn parse_tagged_slot_range<It>(it: &mut It) -> Result<SlotRange, CmdParseError>
    where
        It: Iterator<Item = String>,
    {
        let slot_range = try_get!(it.next());
        let slot_range_tag = slot_range.to_uppercase();

        if slot_range_tag == MIGRATING_TAG {
            let dst = try_get!(it.next());
            let mut slot_range = try_parse!(Self::parse_slot_range(try_get!(it.next())));
            slot_range.tag = SlotRangeTag::Migrating(dst);
            Ok(slot_range)
        } else if slot_range_tag == IMPORTING_TAG {
            let src = try_get!(it.next());
            let mut slot_range = try_parse!(Self::parse_slot_range(try_get!(it.next())));
            slot_range.tag = SlotRangeTag::Importing(src);
            Ok(slot_range)
        } else {
            Self::parse_slot_range(slot_range)
        }
    }

    fn parse_slot_range(s: String) -> Result<SlotRange, CmdParseError> {
        let mut slot_range = s.split('-');
        let start_str = try_get!(slot_range.next());
        let end_str = try_get!(slot_range.next());
        let start = try_parse!(start_str.parse::<usize>());
        let end = try_parse!(end_str.parse::<usize>());
        Ok(SlotRange {
            start,
            end,
            tag: SlotRangeTag::None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_db() {
        let mut arguments = vec!["233", "force", "dbname", "127.0.0.1:6379", "0-1000"]
            .into_iter()
            .map(|s| s.to_string())
            .peekable();
        let r = HostDBMap::parse(&mut arguments);
        assert!(r.is_ok());
        let host_db_map = r.expect("test_single_db");
        assert_eq!(host_db_map.epoch, 233);
        assert_eq!(host_db_map.flags, DBMapFlags { force: true });
        assert_eq!(host_db_map.db_map.len(), 1);
    }

    #[test]
    fn test_multiple_slots() {
        let mut arguments = vec![
            "233",
            "noflag",
            "dbname",
            "127.0.0.1:6379",
            "0-1000",
            "dbname",
            "127.0.0.1:6379",
            "1001-2000",
        ]
        .into_iter()
        .map(|s| s.to_string())
        .peekable();
        let r = HostDBMap::parse(&mut arguments);
        assert!(r.is_ok());
        let host_db_map = r.expect("test_multiple_slots");
        assert_eq!(host_db_map.epoch, 233);
        assert_eq!(host_db_map.flags, DBMapFlags { force: false });
        assert_eq!(host_db_map.db_map.len(), 1);
        assert_eq!(
            host_db_map
                .db_map
                .get("dbname")
                .expect("test_multiple_slots")
                .len(),
            1
        );
        assert_eq!(
            host_db_map
                .db_map
                .get("dbname")
                .expect("test_multiple_slots")
                .get("127.0.0.1:6379")
                .expect("test_multiple_slots")
                .len(),
            2
        );
    }

    #[test]
    fn test_multiple_nodes() {
        let mut arguments = vec![
            "233",
            "noflag",
            "dbname",
            "127.0.0.1:7000",
            "0-1000",
            "dbname",
            "127.0.0.1:7001",
            "1001-2000",
        ]
        .into_iter()
        .map(|s| s.to_string())
        .peekable();
        let r = HostDBMap::parse(&mut arguments);
        assert!(r.is_ok());
        let host_db_map = r.expect("test_multiple_nodes");
        assert_eq!(host_db_map.epoch, 233);
        assert_eq!(host_db_map.flags, DBMapFlags { force: false });
        assert_eq!(host_db_map.db_map.len(), 1);
        assert_eq!(
            host_db_map
                .db_map
                .get("dbname")
                .expect("test_multiple_nodes")
                .len(),
            2
        );
        assert_eq!(
            host_db_map
                .db_map
                .get("dbname")
                .expect("test_multiple_nodes")
                .get("127.0.0.1:7000")
                .expect("test_multiple_nodes")
                .len(),
            1
        );
        assert_eq!(
            host_db_map
                .db_map
                .get("dbname")
                .expect("test_multiple_nodes")
                .get("127.0.0.1:7001")
                .expect("test_multiple_nodes")
                .len(),
            1
        );
    }

    #[test]
    fn test_multiple_db() {
        let mut arguments = vec![
            "233",
            "noflag",
            "dbname",
            "127.0.0.1:7000",
            "0-1000",
            "dbname",
            "127.0.0.1:7001",
            "1001-2000",
            "another_db",
            "127.0.0.1:7002",
            "0-2000",
        ]
        .into_iter()
        .map(|s| s.to_string())
        .peekable();
        let r = HostDBMap::parse(&mut arguments);
        assert!(r.is_ok());
        let host_db_map = r.expect("test_multiple_db");
        assert_eq!(host_db_map.epoch, 233);
        assert_eq!(host_db_map.flags, DBMapFlags { force: false });
        assert_eq!(host_db_map.db_map.len(), 2);
        assert_eq!(
            host_db_map
                .db_map
                .get("dbname")
                .expect("test_multiple_db")
                .len(),
            2
        );
        assert_eq!(
            host_db_map
                .db_map
                .get("dbname")
                .expect("test_multiple_db")
                .get("127.0.0.1:7000")
                .expect("test_multiple_db")
                .len(),
            1
        );
        assert_eq!(
            host_db_map
                .db_map
                .get("dbname")
                .expect("test_multiple_db")
                .get("127.0.0.1:7001")
                .expect("test_multiple_db")
                .len(),
            1
        );
        assert_eq!(
            host_db_map
                .db_map
                .get("another_db")
                .expect("test_multiple_nodes")
                .len(),
            1
        );
        assert_eq!(
            host_db_map
                .db_map
                .get("another_db")
                .expect("test_multiple_db")
                .get("127.0.0.1:7002")
                .expect("test_multiple_db")
                .len(),
            1
        );
    }

    #[test]
    fn test_to_map() {
        let arguments = vec![
            "233",
            "noflag",
            "dbname",
            "127.0.0.1:7000",
            "0-1000",
            "dbname",
            "127.0.0.1:7001",
            "1001-2000",
            "another_db",
            "127.0.0.1:7002",
            "0-2000",
        ];
        let mut it = arguments
            .clone()
            .into_iter()
            .map(|s| s.to_string())
            .peekable();
        let r = HostDBMap::parse(&mut it);
        let host_db_map = r.expect("test_to_map");

        let db_map = HostDBMap::new(host_db_map.epoch, host_db_map.flags, host_db_map.db_map);
        let mut args = db_map.db_map_to_args();
        let mut db_args: Vec<String> = arguments
            .into_iter()
            .skip(2)
            .map(|s| s.to_string())
            .collect();
        args.sort();
        db_args.sort();
        assert_eq!(args, db_args);
    }
}
