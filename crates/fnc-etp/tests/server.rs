//! Loopback tests: a real server on 127.0.0.1 over a small index (plain FTP commands,
//! passive and active data connections, the Everything extension, the client).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use fnc_core::FileTime;
use fnc_etp::namespace::ftp_path;
use fnc_etp::proto::{Reply, ReplyParser, parse_host_port, parse_query_reply};
use fnc_etp::{EtpClient, EtpConfig, EtpHandle, SearchState, Target};
use fnc_index::{IndexStore, Meta, SharedIndex, VolumeInfo, VolumeKind};
use fnc_ipc::backend::{DefaultHost, IndexBackend};
use fnc_ipc::proto::Column;

struct Fixture {
    _dir: tempfile::TempDir,
    /// Index path of the root folder.
    root: String,
    backend: Arc<IndexBackend>,
}

fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    std::fs::create_dir(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("docs").join("abc report.txt"), b"hello download").unwrap();
    std::fs::write(dir.path().join("secret.txt"), b"not indexed").unwrap();
    let mut s = IndexStore::default();
    let vol = s.add_volume(VolumeInfo::new(VolumeKind::Folder, root.clone()), Some(1)).unwrap();
    let mut b = s.bulk_load(vol);
    let dm = Some(FileTime(133_000_000_000_000_000));
    b.push(2, 1, &u16s("docs"), &Meta { attributes: Some(16), date_modified: dm, ..Meta::default() }).unwrap();
    b.push(
        3,
        2,
        &u16s("abc report.txt"),
        &Meta { attributes: Some(32), size: Some(14), date_modified: dm, ..Meta::default() },
    )
    .unwrap();
    // In the index but not on disk.
    b.push(4, 2, &u16s("ghost abc.txt"), &Meta { attributes: Some(32), size: Some(3), ..Meta::default() }).unwrap();
    for i in 0..40u64 {
        b.push(
            100 + i,
            1,
            &u16s(&format!("page{i:02}.dat")),
            &Meta { attributes: Some(32), size: Some(i), ..Meta::default() },
        )
        .unwrap();
    }
    b.finish().unwrap();
    s.rebuild_name_order();
    let backend = Arc::new(IndexBackend::new(SharedIndex::new(s), Arc::new(DefaultHost)));
    Fixture { _dir: dir, root, backend }
}

fn server(f: &Fixture, cfg: EtpConfig) -> EtpHandle {
    let cfg = EtpConfig {
        port: 0,
        username: "u".into(),
        password: fnc_core::secret::hash("p"),
        login_delay: Duration::from_millis(10),
        ..cfg
    };
    fnc_etp::start(cfg, f.backend.clone()).unwrap()
}

/// A raw control connection.
struct Ftp {
    r: BufReader<TcpStream>,
    w: TcpStream,
}

impl Ftp {
    fn connect(addr: SocketAddr) -> (Self, Reply) {
        let s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut f = Ftp { r: BufReader::new(s.try_clone().unwrap()), w: s };
        let hello = f.reply();
        (f, hello)
    }

    fn reply(&mut self) -> Reply {
        let mut p = ReplyParser::default();
        loop {
            let mut line = String::new();
            assert!(self.r.read_line(&mut line).unwrap() > 0, "connection closed");
            if let Some(r) = p.push(line.trim_end_matches(['\r', '\n'])).unwrap() {
                return r;
            }
        }
    }

    fn cmd(&mut self, line: &str) -> Reply {
        self.w.write_all(format!("{line}\r\n").as_bytes()).unwrap();
        self.reply()
    }

    fn login(addr: SocketAddr) -> Self {
        let (mut f, _) = Ftp::connect(addr);
        assert_eq!(f.cmd("USER u").code, 331);
        assert_eq!(f.cmd("PASS p").code, 230);
        f
    }

    /// PASV + `line`, returns (data, final reply).
    fn data(&mut self, line: &str) -> (Vec<u8>, Reply) {
        let r = self.cmd("PASV");
        assert_eq!(r.code, 227, "{r:?}");
        let a = parse_host_port(r.text()).unwrap();
        let mut d = TcpStream::connect(a).unwrap();
        let r = self.cmd(line);
        if r.code != 150 {
            return (Vec::new(), r);
        }
        let mut buf = Vec::new();
        d.read_to_end(&mut buf).unwrap();
        (buf, self.reply())
    }

    fn text(&mut self, line: &str) -> (String, u16) {
        let (d, r) = self.data(line);
        (String::from_utf8(d).unwrap(), r.code)
    }
}

#[test]
fn login_is_required_and_checked() {
    let f = fixture();
    let h = server(&f, EtpConfig::default());
    let (mut c, hello) = Ftp::connect(h.local_addr());
    assert_eq!((hello.code, hello.text()), (220, "Welcome to Fennec ETP/FTP"));
    assert_eq!(c.cmd("LIST").code, 530);
    assert_eq!(c.cmd("EVERYTHING QUERY").code, 530);
    let feat = c.cmd("FEAT");
    assert_eq!(feat.code, 211);
    assert!(feat.lines.iter().any(|l| l.trim() == "EVERYTHING"), "{feat:?}");
    // Windows ftp.exe sends this before logging in.
    assert_eq!(c.cmd("OPTS UTF8 ON").code, 200);
    assert_eq!(c.cmd("USER u").code, 331);
    assert_eq!(c.cmd("PASS wrong").code, 530);
    // A wrong user name also asks for a password (and then fails).
    assert_eq!(c.cmd("USER x").code, 331);
    assert_eq!(c.cmd("PASS p").code, 530);
    // The third failure closes the connection.
    assert_eq!(c.cmd("USER u").code, 331);
    assert_eq!(c.cmd("PASS nope").code, 421);
    let (mut c, _) = Ftp::connect(h.local_addr());
    assert_eq!(c.cmd("USER u").code, 331);
    assert_eq!(c.cmd("PASS p").code, 230);
    assert_eq!(c.cmd("PASS p").code, 503);
    assert_eq!(c.cmd("SYST").text(), "UNIX emulated by Fennec");
    assert_eq!(c.cmd("QUIT").code, 221);
    // No password: refuses to start.
    assert!(matches!(
        fnc_etp::start(EtpConfig { port: 0, ..EtpConfig::default() }, f.backend.clone()),
        Err(fnc_etp::EtpError::NoPassword)
    ));
}

#[test]
fn browsing_comes_from_the_index() {
    let f = fixture();
    let h = server(&f, EtpConfig::default());
    let mut c = Ftp::login(h.local_addr());
    let root = ftp_path(&f.root);
    assert_eq!(c.cmd("PWD").text(), "\"/\" is current directory.");
    // `/` lists the first component of the indexed root.
    let first = root.split('/').find(|p| !p.is_empty()).unwrap().to_string();
    let (list, code) = c.text("NLST");
    assert_eq!((list.as_str(), code), (format!("{first}\r\n").as_str(), 226));
    let r = c.cmd(&format!("CWD {root}"));
    assert_eq!(r.code, 250, "{r:?}");
    assert_eq!(c.cmd("PWD").text(), format!("\"{root}\" is current directory."));
    let (list, code) = c.text("LIST -la");
    assert_eq!(code, 226);
    let lines: Vec<&str> = list.lines().collect();
    assert_eq!(lines.len(), 41, "{list}");
    assert!(lines[0].starts_with("dr--r--r-- 1 ftp ftp             0 ") && lines[0].ends_with(" docs"), "{list}");
    assert!(lines[1].starts_with("-r--r--r-- 1 ftp ftp             0 ") && lines[1].ends_with(" page00.dat"));
    // The file that is on disk but not in the index is not listed.
    assert!(!list.contains("secret.txt"));
    let (mlsd, _) = c.text("MLSD docs");
    assert!(mlsd.contains("type=file;modify=20220618042640;size=14; abc report.txt\r\n"), "{mlsd}");
    assert!(mlsd.contains("ghost abc.txt"));
    let r = c.cmd("MLST docs/abc report.txt");
    assert_eq!(r.code, 250);
    assert!(r.lines[1].starts_with(" type=file;modify=20220618042640;size=14; /"), "{r:?}");
    assert_eq!(c.cmd("SIZE docs/abc report.txt").text(), "14");
    assert_eq!(c.cmd("MDTM docs/abc report.txt").text(), "20220618042640.000");
    assert_eq!(c.cmd("SIZE docs").code, 550);
    assert_eq!(c.cmd("CWD docs").code, 250);
    assert_eq!(c.cmd("CWD nothing").code, 550);
    assert_eq!(c.cmd("CDUP").code, 250);
    assert_eq!(c.cmd("PWD").text(), format!("\"{root}\" is current directory."));
    // Not in the index → not reachable, even with `..`.
    // As many `..` as the root is deep (temporary folders are nested deeper on CI runners).
    let up = vec![".."; root.split('/').filter(|p| !p.is_empty()).count() + 3].join("/");
    assert_eq!(c.cmd(&format!("CWD {up}")).code, 250);
    assert_eq!(c.cmd("PWD").text(), "\"/\" is current directory.");
    assert_eq!(c.data(&format!("LIST {root}/secret.txt")).1.code, 550);
    assert_eq!(c.cmd("MKD x").code, 550, "read-only");
    assert_eq!(c.cmd("TYPE I").code, 200);
    assert_eq!(c.cmd("BOGUS").code, 500);
}

#[test]
fn downloads_are_resolved_through_the_index() {
    let f = fixture();
    let h = server(&f, EtpConfig::default());
    let mut c = Ftp::login(h.local_addr());
    let root = ftp_path(&f.root);
    let (data, r) = c.data(&format!("RETR {root}/docs/abc report.txt"));
    assert_eq!((data.as_slice(), r.code), (&b"hello download"[..], 226));
    assert_eq!(c.cmd("REST 6").code, 350);
    let (data, _) = c.data(&format!("RETR {root}/docs/abc report.txt"));
    assert_eq!(data, b"download");
    // Not indexed / not on disk / a folder / traversal.
    for p in [
        format!("{root}/secret.txt"),
        format!("{root}/docs/ghost abc.txt"),
        format!("{root}/docs"),
        format!("{root}/docs/../../{}", f.root.rsplit(['/', '\\']).next().unwrap()),
    ] {
        assert_eq!(c.data(&format!("RETR {p}")).1.code, 550, "{p}");
    }
    // Active mode back to the client.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    assert_eq!(c.cmd(&format!("PORT 127,0,0,1,{},{}", p >> 8, p & 0xff)).code, 200);
    assert_eq!(c.cmd(&format!("RETR {root}/docs/abc report.txt")).code, 150);
    let (mut d, _) = l.accept().unwrap();
    let mut buf = Vec::new();
    d.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"hello download");
    assert_eq!(c.reply().code, 226);
    // No bounce to other hosts.
    assert_eq!(c.cmd("PORT 10,1,2,3,4,5").code, 500);
    assert_eq!(c.cmd(&format!("EPRT |1|127.0.0.1|{p}|")).code, 200);
    drop(c);
    // Downloads off.
    let h2 = server(&f, EtpConfig { allow_downloads: false, ..EtpConfig::default() });
    let mut c = Ftp::login(h2.local_addr());
    assert_eq!(c.data(&format!("RETR {root}/docs/abc report.txt")).1.code, 550);
    drop(h);
}

#[test]
fn everything_extension_pages_results() {
    let f = fixture();
    let h = server(&f, EtpConfig::default());
    let mut c = Ftp::login(h.local_addr());
    // What Everything 1.4 sends after logging in.
    assert_eq!(c.cmd("OPTS UTF8 ON").code, 200);
    for (line, want) in [
        ("EVERYTHING SIZE_COLUMN 1", "Size column set to (1)."),
        ("EVERYTHING DATE_MODIFIED_COLUMN 1", "Date modified column set to (1)."),
        ("EVERYTHING PATH_COLUMN 1", "Path column set to (1)."),
        ("EVERYTHING COUNT 26", "Count set to (26)."),
        ("EVERYTHING SORT NAME_ASCENDING", "Sort set to (NAME_ASCENDING)."),
    ] {
        let r = c.cmd(line);
        assert_eq!((r.code, r.text()), (200, want));
    }
    let page = parse_query_reply(&c.cmd("EVERYTHING QUERY")).unwrap();
    // The root folder, docs, two files in docs, 40 pages.
    assert_eq!(page.total, 44);
    assert_eq!(page.items.len(), 26);
    // Folders first: the indexed root (sent as parent path + name, like Everything), docs.
    assert_eq!(page.items[0].full_path(), f.root);
    assert_ne!(page.items[0].name, f.root);
    assert_eq!(page.items[1].name, "docs");
    assert!(page.items[1].folder);
    assert_eq!(page.items[1].size, Some(u64::MAX), "folder sizes are unknown");
    let abc = page.items.iter().find(|i| i.name == "abc report.txt").unwrap();
    assert_eq!(abc.path.as_deref(), Some(format!("{}{}docs", f.root, std::path::MAIN_SEPARATOR).as_str()));
    assert_eq!((abc.size, abc.date_modified), (Some(14), Some(133_000_000_000_000_000)));
    // SITE form, sorting and paging.
    c.cmd("SITE EVERYTHING SEARCH page");
    c.cmd("SITE EVERYTHING SORT SIZE_DESCENDING");
    c.cmd("SITE EVERYTHING OFFSET 10");
    c.cmd("SITE EVERYTHING COUNT 5");
    let page = parse_query_reply(&c.cmd("SITE EVERYTHING QUERY")).unwrap();
    assert_eq!(page.total, 40);
    let names: Vec<&str> = page.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["page29.dat", "page28.dat", "page27.dat", "page26.dat", "page25.dat"]);
    // Options persist; a filter search is ANDed; regex.
    c.cmd("EVERYTHING OFFSET 0");
    c.cmd("EVERYTHING FILTER_SEARCH page1");
    assert_eq!(parse_query_reply(&c.cmd("EVERYTHING QUERY")).unwrap().total, 10);
    // The filter's own options apply to the filter only.
    c.cmd("EVERYTHING FILTER_SEARCH PAGE1");
    c.cmd("EVERYTHING FILTER_CASE 1");
    assert_eq!(parse_query_reply(&c.cmd("EVERYTHING QUERY")).unwrap().total, 0);
    c.cmd("EVERYTHING FILTER_SEARCH page1|page2");
    assert_eq!(parse_query_reply(&c.cmd("EVERYTHING QUERY")).unwrap().total, 20);
    c.cmd("EVERYTHING FILTER_CASE 0");
    c.cmd("EVERYTHING FILTER_SEARCH");
    c.cmd("EVERYTHING SEARCH ^page3[0-4]\\.dat$");
    c.cmd("EVERYTHING REGEX 1");
    assert_eq!(parse_query_reply(&c.cmd("EVERYTHING QUERY")).unwrap().total, 5);
    c.cmd("EVERYTHING REGEX 0");
    c.cmd("EVERYTHING SEARCH ABC");
    c.cmd("EVERYTHING CASE 1");
    assert_eq!(parse_query_reply(&c.cmd("EVERYTHING QUERY")).unwrap().total, 0);
    c.cmd("EVERYTHING CASE 0");
    assert_eq!(parse_query_reply(&c.cmd("EVERYTHING QUERY")).unwrap().total, 2);
    c.cmd("EVERYTHING SEARCH");
    c.cmd("EVERYTHING HIDE_EMPTY_SEARCH_RESULTS 1");
    assert_eq!(parse_query_reply(&c.cmd("EVERYTHING QUERY")).unwrap().total, 0);
    assert_eq!(c.cmd("EVERYTHING SORT TYPE_ASCENDING").code, 500);
    assert_eq!(c.cmd("SITE CHMOD 777 x").code, 500);
}

#[test]
fn client_round_trip() {
    let f = fixture();
    let h = server(&f, EtpConfig::default());
    let target = Target::parse(&format!("u:p@{}", h.local_addr()), 21).unwrap();
    let mut c = EtpClient::connect(&target, Duration::from_secs(10)).unwrap();
    assert!(c.welcome().contains("Fennec"));
    let mut want = SearchState { search: "abc".into(), count: 100, ..SearchState::default() };
    want.fields.path = true;
    want.fields.size = true;
    want.fields.date_modified = true;
    want.sort = (Column::Size, false);
    let page = c.query(&want).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items[0].name, "abc report.txt");
    let full = page.items[0].full_path();
    let mut buf = Vec::new();
    assert_eq!(c.retr(&full, &mut buf).unwrap(), 14);
    assert_eq!(buf, b"hello download");
    // Only changes are sent; the server keeps the rest.
    want.search = "page".into();
    want.offset = 38;
    let page = c.query(&want).unwrap();
    assert_eq!((page.total, page.items.len()), (40, 2));
    let lines = c.mlsd(&ftp_path(&f.root)).unwrap();
    assert_eq!(lines.len(), 41);
    let err = c.retr(&format!("{}{}secret.txt", f.root, std::path::MAIN_SEPARATOR), &mut Vec::new()).unwrap_err();
    assert!(matches!(err, fnc_etp::ClientError::Reply { code: 550, .. }), "{err}");
    c.quit();
    // Wrong password.
    let bad = Target { password: "x".into(), ..target };
    assert!(matches!(EtpClient::connect(&bad, Duration::from_secs(10)), Err(fnc_etp::ClientError::Login(_))));
}

#[test]
fn connection_limit_and_stop() {
    let f = fixture();
    let h = server(&f, EtpConfig { max_connections: 1, ..EtpConfig::default() });
    let mut a = Ftp::login(h.local_addr());
    let (_, r) = Ftp::connect(h.local_addr());
    assert_eq!(r.code, 421);
    assert_eq!(a.cmd("NOOP").code, 200);
    let addr = h.local_addr();
    h.stop();
    // Open sessions are closed and nothing listens any more.
    let mut line = String::new();
    assert_eq!(a.r.read_line(&mut line).unwrap_or(0), 0);
    assert!(TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err());
}

#[test]
fn command_log_masks_passwords() {
    let f = fixture();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("etp.log");
    let h = server(&f, EtpConfig { log_file: Some(log.clone()), ..EtpConfig::default() });
    let mut c = Ftp::login(h.local_addr());
    c.cmd("EVERYTHING SEARCH hello");
    c.cmd("QUIT");
    drop(h);
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        text.contains("USER u") && text.contains("PASS ****") && text.contains("EVERYTHING SEARCH hello"),
        "{text}"
    );
    assert!(!text.contains("PASS p\r"));
}
