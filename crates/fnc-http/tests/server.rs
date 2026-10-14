//! In-process HTTP server tests over real sockets (INT-04 and protections).

// Test-only helpers outside `#[test]` functions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use fnc_core::FileTime;
use fnc_http::{HttpConfig, HttpHandle};
use fnc_index::{IndexStore, Meta, SharedIndex, VolumeInfo, VolumeKind};
use fnc_ipc::backend::{DefaultHost, IndexBackend};

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn b64(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = s.as_bytes();
    let mut out = String::new();
    for c in b.chunks(3) {
        let n =
            (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn raw(addr: SocketAddr, request: &str) -> Resp {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(request.as_bytes()).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").expect("header end");
    let head = String::from_utf8_lossy(&buf[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    let mut body = buf[split + 4..].to_vec();
    if headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked")) {
        let mut out = Vec::new();
        let mut rest = &body[..];
        loop {
            let nl = rest.windows(2).position(|w| w == b"\r\n").unwrap();
            let n = usize::from_str_radix(std::str::from_utf8(&rest[..nl]).unwrap().trim(), 16).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&rest[nl + 2..nl + 2 + n]);
            rest = &rest[nl + 2 + n + 2..];
        }
        body = out;
    }
    Resp { status, headers, body }
}

fn get(addr: SocketAddr, path: &str, auth: Option<(&str, &str)>) -> Resp {
    let auth = auth.map(|(u, p)| format!("Authorization: Basic {}\r\n", b64(&format!("{u}:{p}")))).unwrap_or_default();
    raw(addr, &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n"))
}

const AUTH: Option<(&str, &str)> = Some(("u", "p"));

struct Fixture {
    _dir: tempfile::TempDir,
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
    b.push(4, 2, &u16s("<i>x&y.txt"), &Meta { attributes: Some(32), size: Some(1), ..Meta::default() }).unwrap();
    b.push(5, 2, &u16s("ghost abc.txt"), &Meta { attributes: Some(32), size: Some(3), ..Meta::default() }).unwrap();
    for i in 0..40 {
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

fn server(f: &Fixture, cfg: HttpConfig) -> HttpHandle {
    let cfg = HttpConfig { port: 0, username: "u".into(), password: "p".into(), ..cfg };
    fnc_http::start(cfg, f.backend.clone()).unwrap()
}

#[test]
fn int04_json_shape() {
    let f = fixture();
    let h = server(&f, HttpConfig::default());
    let r = get(h.local_addr(), "/?search=abc&json=1", AUTH);
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("application/json; charset=utf-8"));
    assert_eq!(r.header("server"), Some(concat!("Fennec/", env!("CARGO_PKG_VERSION"))));
    let body = r.text();
    assert_eq!(
        body,
        "{\r\n\"totalResults\":2,\r\n\"results\":[\r\n\t{\r\n\t\t\"type\":\"file\",\r\n\t\t\"name\":\"abc report.txt\"\r\n\t},\r\n\t{\r\n\t\t\"type\":\"file\",\r\n\t\t\"name\":\"ghost abc.txt\"\r\n\t}\r\n]\r\n}\r\n"
    );
    let r = get(
        h.local_addr(),
        "/?search=docs&json=1&path_column=1&size_column=1&date_modified_column=1&attributes_column=1",
        AUTH,
    );
    let body = r.text();
    assert!(body.contains("\"totalResults\":1"), "{body}");
    assert!(body.contains("\"type\":\"folder\""));
    assert!(body.contains("\"date_modified\":\"133000000000000000\""));
    assert!(body.contains("\"attributes\":\"16\""));
    assert!(!body.contains("\"size\""), "folder size unknown → omitted");
    let esc = f.root.replace('\\', "\\\\");
    assert!(body.contains(&format!("\"path\":\"{esc}\"")), "{body}");
    let r = get(h.local_addr(), "/?s=report&j=1&size_column=1", AUTH);
    assert!(r.text().contains("\"size\":\"14\""));
    // Sort, offset, count.
    let r = get(h.local_addr(), "/?search=page&json=1&sort=size&ascending=0&count=2&offset=1", AUTH);
    let body = r.text();
    assert!(body.contains("\"totalResults\":40"));
    assert!(body.contains("page38.dat") && body.contains("page37.dat") && !body.contains("page39.dat"), "{body}");
}

#[test]
fn authentication() {
    let f = fixture();
    let h = server(&f, HttpConfig::default());
    let r = get(h.local_addr(), "/?search=abc&json=1", None);
    assert_eq!(r.status, 401);
    assert!(r.header("www-authenticate").unwrap().starts_with("Basic"));
    assert!(!r.text().contains("abc report"));
    assert_eq!(get(h.local_addr(), "/?search=abc&json=1", Some(("u", "wrong"))).status, 401);
    assert_eq!(get(h.local_addr(), "/?search=abc&json=1", Some(("x", "p"))).status, 401);
    assert_eq!(get(h.local_addr(), "/api/v1/status", None).status, 401);
    let bad = raw(h.local_addr(), "GET / HTTP/1.1\r\nHost: x\r\nAuthorization: Basic !!!\r\nConnection: close\r\n\r\n");
    assert_eq!(bad.status, 401);
    assert_eq!(get(h.local_addr(), "/?search=abc&json=1", AUTH).status, 200);
    // No password → refuses to start.
    let e = fnc_http::start(HttpConfig { port: 0, ..HttpConfig::default() }, f.backend.clone());
    assert!(matches!(e, Err(fnc_http::HttpError::NoPassword)));
}

#[test]
fn downloads() {
    let f = fixture();
    let h = server(&f, HttpConfig::default());
    let url = fnc_http::params::download_url(&format!("{}/docs/abc report.txt", f.root));
    let r = get(h.local_addr(), &url, AUTH);
    assert_eq!(r.status, 200, "{url}");
    assert_eq!(r.body, b"hello download");
    assert_eq!(r.header("content-length"), Some("14"));
    assert!(r.header("content-disposition").unwrap().contains("filename=\"abc report.txt\""));
    // Folders are not listed; unknown and not-indexed paths are 404; traversal cannot escape.
    assert_eq!(get(h.local_addr(), &fnc_http::params::download_url(&format!("{}/docs", f.root)), AUTH).status, 404);
    assert_eq!(
        get(h.local_addr(), &fnc_http::params::download_url(&format!("{}/docs/ghost abc.txt", f.root)), AUTH).status,
        404
    );
    let traversal = fnc_http::params::download_url(&format!("{}/docs/../docs/abc report.txt", f.root));
    assert_eq!(get(h.local_addr(), &traversal, AUTH).status, 404);
    assert_eq!(get(h.local_addr(), "/etc/passwd", AUTH).status, 404);
    assert_eq!(get(h.local_addr(), "/..%2f..%2fetc%2fpasswd", AUTH).status, 404);
    drop(h);
    // Disabled downloads.
    let h = server(&f, HttpConfig { allow_downloads: false, ..HttpConfig::default() });
    assert_eq!(get(h.local_addr(), &url, AUTH).status, 403);
}

#[test]
fn html_page_escapes_and_pages() {
    let f = fixture();
    let h = server(&f, HttpConfig { page_size: 10, ..HttpConfig::default() });
    let r = get(h.local_addr(), "/?search=%3Cscript%3Ealert(1)%3C%2Fscript%3E", AUTH);
    assert_eq!(r.status, 200);
    let body = r.text();
    assert!(!body.contains("<script>"), "search text must be escaped");
    assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(r.header("content-security-policy").unwrap().contains("default-src 'none'"));
    // File names from the index are escaped too.
    let body = get(h.local_addr(), "/?search=%26y.txt", AUTH).text();
    assert!(body.contains("&lt;i&gt;x&amp;y.txt") && !body.contains("<i>x"), "{body}");
    // Paging: 40 results, 10 per page.
    let body = get(h.local_addr(), "/?search=page", AUTH).text();
    assert!(body.contains("40 results"));
    assert_eq!(body.matches("<tr>").count(), 11, "header + 10 rows");
    assert!(body.contains("offset=10") && body.contains("rel=\"next\"") && !body.contains("rel=\"prev\""));
    let body = get(h.local_addr(), "/?search=page&offset=30", AUTH).text();
    assert!(body.contains("page39.dat") && body.contains("rel=\"prev\"") && !body.contains("rel=\"next\""));
    // Sort links.
    assert!(body.contains("sort=size&amp;ascending=1"));
    // Folder links search the folder instead of listing it.
    let body = get(h.local_addr(), "/?search=docs", AUTH).text();
    assert!(body.contains("href=\"/?search=%22"), "{body}");
    // Empty search shows only the form.
    let body = get(h.local_addr(), "/", AUTH).text();
    assert!(body.contains("<form") && !body.contains("<table"));
}

#[test]
fn limits_and_status() {
    let f = fixture();
    let h = server(&f, HttpConfig { max_results: 5, max_terms: 3, ..HttpConfig::default() });
    let body = get(h.local_addr(), "/?search=page&json=1", AUTH).text();
    assert_eq!(body.matches("\"type\"").count(), 5, "result cap");
    assert!(body.contains("\"totalResults\":40"));
    assert_eq!(get(h.local_addr(), "/?search=a+b+c+d&json=1", AUTH).status, 400);
    assert_eq!(get(h.local_addr(), &format!("/?search={}&json=1", "x".repeat(2000)), AUTH).status, 400);
    assert_eq!(get(h.local_addr(), "/?search=a&offset=zz", AUTH).status, 400);
    let long = format!("/?search={}", "y".repeat(9000));
    assert_eq!(get(h.local_addr(), &long, AUTH).status, 414);
    let post = raw(
        h.local_addr(),
        &format!(
            "POST / HTTP/1.1\r\nHost: x\r\nAuthorization: Basic {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            b64("u:p")
        ),
    );
    assert_eq!(post.status, 405);
    let st = get(h.local_addr(), "/api/v1/status", AUTH);
    assert_eq!(st.status, 200);
    assert!(st.text().contains("\"entries\":45"), "{}", st.text());
    let v = get(h.local_addr(), "/api/v1/volumes", AUTH).text();
    assert!(v.contains("\"kind\":\"folder\""));
    assert_eq!(get(h.local_addr(), "/api/v1/nope", AUTH).status, 404);
}

#[test]
fn static_root() {
    let f = fixture();
    let site = tempfile::tempdir().unwrap();
    std::fs::write(site.path().join("index.html"), "<h1>custom</h1>").unwrap();
    std::fs::write(site.path().join("app.css"), "body{}").unwrap();
    let h = server(&f, HttpConfig { static_root: Some(site.path().to_path_buf()), ..HttpConfig::default() });
    let r = get(h.local_addr(), "/", AUTH);
    assert_eq!(r.text(), "<h1>custom</h1>");
    assert_eq!(get(h.local_addr(), "/app.css", AUTH).header("content-type"), Some("text/css; charset=utf-8"));
    assert_eq!(get(h.local_addr(), "/../app.css", AUTH).status, 404);
    assert_eq!(get(h.local_addr(), "/%2e%2e/app.css", AUTH).status, 404);
    // Searches still work with a static root.
    assert!(get(h.local_addr(), "/?search=abc&json=1", AUTH).text().contains("\"totalResults\":2"));
}

#[test]
fn stop_releases_the_port() {
    let f = fixture();
    let h = server(&f, HttpConfig::default());
    let addr = h.local_addr();
    h.stop();
    assert!(TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_err());
}

#[test]
fn request_timeout() {
    /// A host whose searches take 500 ms to start.
    struct Slow;
    impl fnc_ipc::Host for Slow {
        fn search_options(&self) -> fnc_exec::SearchOptions {
            std::thread::sleep(Duration::from_millis(500));
            fnc_exec::SearchOptions::default()
        }
    }
    let f = fixture();
    let slow = Fixture { backend: Arc::new(IndexBackend::new(f.backend.index().clone(), Arc::new(Slow))), ..f };
    let h = server(&slow, HttpConfig { request_timeout: Duration::from_millis(100), ..HttpConfig::default() });
    let t = std::time::Instant::now();
    let r = get(h.local_addr(), "/?search=abc&json=1", AUTH);
    assert_eq!(r.status, 503);
    assert!(t.elapsed() < Duration::from_millis(450), "answered at the deadline");
    assert_eq!(r.header("server"), Some(concat!("Fennec/", env!("CARGO_PKG_VERSION"))));
}

#[test]
fn hashed_password_from_settings_authenticates() {
    let f = fixture();
    let stored = fnc_core::secret::hash("s3cret");
    let cfg = HttpConfig { port: 0, username: "u".into(), password: stored, ..HttpConfig::default() };
    let h = fnc_http::start(cfg, f.backend.clone()).unwrap();
    assert_eq!(get(h.local_addr(), "/?search=abc&json=1", Some(("u", "s3cret"))).status, 200);
    assert_eq!(get(h.local_addr(), "/?search=abc&json=1", Some(("u", "wrong"))).status, 401);
    // The hash itself is not a password.
    let hash_as_pw = fnc_core::secret::hash("x");
    assert_eq!(get(h.local_addr(), "/", Some(("u", &hash_as_pw))).status, 401);
    // Cached verification is still correct.
    assert_eq!(get(h.local_addr(), "/?search=abc&json=1", Some(("u", "s3cret"))).status, 200);
    assert_eq!(get(h.local_addr(), "/", Some(("v", "s3cret"))).status, 401);
}
