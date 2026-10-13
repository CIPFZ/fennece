//! Server ↔ client over a real local socket (named pipe on Windows).

// Test-only helpers outside `#[test]` functions.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use fnc_index::{IndexStore, SharedIndex};
use fnc_ipc::backend::{AdminCommand, Host, HostError, IndexBackend, Peer};
use fnc_ipc::proto::{self, Column, Mods, Query, SearchSpec, Status};
use fnc_ipc::{Client, ClientError, ServerConfig};

fn fixture() -> SharedIndex {
    let mut efu = String::from("Filename,Size,Date Modified,Date Created,Attributes\n\"C:\\Data\",,,,16\n");
    for i in 0..2500 {
        efu.push_str(&format!("\"C:\\Data\\file{i:04}.iso\",{},133000000000000000,,32\n", i * 10));
    }
    efu.push_str("\"C:\\Data\\readme.txt\",5,,,32\n");
    let mut s = IndexStore::default();
    fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "fixture.efu").unwrap();
    s.rebuild_name_order();
    SharedIndex::new(s)
}

#[derive(Default)]
struct TestHost {
    rebuilds: AtomicU32,
}

impl Host for TestHost {
    fn command(&self, cmd: AdminCommand) -> Result<(), HostError> {
        match cmd {
            AdminCommand::Rebuild => {
                self.rebuilds.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            _ => Err(HostError::Unsupported),
        }
    }

    fn authorize(&self, cmd: AdminCommand, _peer: &Peer) -> bool {
        cmd != AdminCommand::Exit
    }
}

fn instance(tag: &str) -> String {
    format!("test-{tag}-{}", std::process::id())
}

#[test]
fn end_to_end() {
    let inst = instance("e2e");
    let host = Arc::new(TestHost::default());
    let backend = Arc::new(IndexBackend::new(fixture(), host.clone()));
    let server = fnc_ipc::spawn(&inst, backend, ServerConfig::default()).unwrap();

    let mut c = Client::connect(&inst, Duration::from_secs(5)).unwrap();
    assert_eq!(c.version().unwrap().protocol, proto::PROTOCOL_VERSION);
    assert!(c.is_db_loaded().unwrap());
    let q = Query {
        search: "*.iso".into(),
        sort: vec![(Column::Size, false)],
        limit: 5,
        columns: vec![Column::FullPath, Column::Size],
        ..Query::default()
    };
    let r = c.query(&q).unwrap();
    assert_eq!(r.total, 2500);
    assert_eq!(r.len(), 5);
    assert_eq!(r.columns.full_path.as_ref().unwrap()[0], "C:\\Data\\file2499.iso");
    assert_eq!(r.columns.size.as_ref().unwrap()[0], 24990);
    let next = c.query_continue(r.query_id, 5, 2).unwrap();
    assert_eq!(next.columns.full_path.as_ref().unwrap(), &["C:\\Data\\file2494.iso", "C:\\Data\\file2493.iso"]);
    let all = c.query_all(&Query { limit: u64::MAX, ..q.clone() }).unwrap();
    assert_eq!(all.len(), 2500);

    let spec = SearchSpec { search: "readme".into(), mods: Mods::default() };
    assert_eq!(c.result_count(&spec).unwrap(), 1);
    assert_eq!(c.total_size(&spec).unwrap(), 5);
    assert_eq!(c.inc_run_count("C:\\Data\\readme.txt").unwrap(), 1);
    assert_eq!(c.run_count("c:\\data\\README.TXT").unwrap(), 1);
    assert!(matches!(c.run_count("C:\\nope"), Err(ClientError::Server { status: Status::NotFound, .. })));
    c.rebuild().unwrap();
    assert_eq!(host.rebuilds.load(Ordering::SeqCst), 1);
    assert!(matches!(c.exit(), Err(ClientError::Server { status: Status::Denied, .. })));
    assert!(matches!(c.save_db(), Err(ClientError::Server { status: Status::Unsupported, .. })));
    assert_eq!(c.volumes().unwrap().len(), 1);
    assert!(c.filters().unwrap().is_empty());
    let st = c.index_status().unwrap();
    assert_eq!(st.entries, 2503);

    // A second client works concurrently.
    let mut c2 = Client::connect(&inst, Duration::from_secs(5)).unwrap();
    assert_eq!(c2.result_count(&SearchSpec { search: "file".into(), mods: Mods::default() }).unwrap(), 2500);
    drop(c);
    drop(c2);
    server.stop().unwrap();
    assert!(Client::connect(&inst, Duration::from_millis(100)).is_err());
}

#[test]
fn malformed_frames_close_the_connection() {
    use interprocess::local_socket::prelude::*;
    use interprocess::local_socket::{GenericNamespaced, Stream};
    let inst = instance("bad");
    let backend = Arc::new(IndexBackend::new(fixture(), Arc::new(TestHost::default())));
    let _server = fnc_ipc::spawn(&inst, backend, ServerConfig::default()).unwrap();
    let name = proto::pipe_name(&inst);
    let mut s = Stream::connect(name.as_str().to_ns_name::<GenericNamespaced>().unwrap()).unwrap();
    proto::write_frame(&mut s, &[0xc1, 0x00], 16).unwrap();
    let resp = proto::read_frame(&mut s, proto::MAX_RESPONSE_FRAME).unwrap().unwrap();
    let resp = proto::Response::decode(&resp).unwrap();
    assert_eq!(resp.status, Status::BadRequest);
    assert!(proto::read_frame(&mut s, proto::MAX_RESPONSE_FRAME).unwrap_or(None).is_none());
    // Oversized length prefix: dropped without a response.
    let mut s = Stream::connect(name.as_str().to_ns_name::<GenericNamespaced>().unwrap()).unwrap();
    use std::io::Write;
    s.write_all(&u32::MAX.to_le_bytes()).unwrap();
    assert!(proto::read_frame(&mut s, proto::MAX_RESPONSE_FRAME).unwrap_or(None).is_none());
}

#[test]
fn connection_limit() {
    let inst = instance("limit");
    let backend = Arc::new(IndexBackend::new(fixture(), Arc::new(TestHost::default())));
    let _server =
        fnc_ipc::spawn(&inst, backend, ServerConfig { max_connections: 1, ..ServerConfig::default() }).unwrap();
    let mut a = Client::connect(&inst, Duration::from_secs(5)).unwrap();
    assert!(a.is_db_loaded().unwrap());
    let mut b = Client::connect(&inst, Duration::from_secs(5)).unwrap();
    match b.is_db_loaded() {
        Err(ClientError::Server { status: Status::Busy, .. }) | Err(ClientError::Io(_)) => {}
        other => panic!("expected busy, got {other:?}"),
    }
    assert!(a.is_db_loaded().unwrap());
}
