//! A blocking ETP client: what Everything's "Connect to ETP server" does — log in,
//! switch to UTF-8, then send only the search options that changed before each
//! `EVERYTHING QUERY`, and download files with `RETR` over a passive data connection.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::proto::{self, QueryPage, Reply, ReplyParser, SearchState};

/// Client failures.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Network failure.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The host name did not resolve.
    #[error("cannot resolve {0}")]
    Resolve(String),
    /// User name or password rejected.
    #[error("login failed: {0}")]
    Login(String),
    /// The server answered with an error.
    #[error("{code} {text}")]
    Reply {
        /// Reply code.
        code: u16,
        /// Reply text.
        text: String,
    },
    /// The server's reply could not be understood.
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// Where to connect: Everything's `username:password@host:port` (every part but the host
/// optional; IPv6 hosts in brackets).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    /// Host name or address.
    pub host: String,
    /// Port (default 21).
    pub port: u16,
    /// User name (empty: `anonymous`).
    pub username: String,
    /// Password.
    pub password: String,
}

impl Target {
    /// Parses `username:password@host:port`; `default_port` when no port is given.
    pub fn parse(s: &str, default_port: u16) -> Option<Self> {
        let s = s.trim().trim_start_matches("ftp://").trim_start_matches("etp://").trim_end_matches('/');
        let (cred, hostport) = match s.rfind('@') {
            Some(i) => (Some(&s[..i]), &s[i + 1..]),
            None => (None, s),
        };
        let (username, password) = match cred {
            Some(c) => match c.split_once(':') {
                Some((u, p)) => (u.to_string(), p.to_string()),
                None => (c.to_string(), String::new()),
            },
            None => (String::new(), String::new()),
        };
        let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
            let end = rest.find(']')?;
            let port = rest[end + 1..].strip_prefix(':').map(str::parse::<u16>);
            (rest[..end].to_string(), port)
        } else if hostport.matches(':').count() == 1 {
            let (h, p) = hostport.split_once(':')?;
            (h.to_string(), Some(p.parse::<u16>()))
        } else {
            (hostport.to_string(), None)
        };
        let port = match port {
            Some(Ok(p)) => p,
            Some(Err(_)) => return None,
            None => default_port,
        };
        (!host.is_empty()).then_some(Self { host, port, username, password })
    }
}

/// A logged-in connection.
pub struct EtpClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    peer: SocketAddr,
    timeout: Duration,
    /// Options as the server has them (Everything only sends changes).
    sent: SearchState,
    welcome: String,
}

fn connect_any(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, ClientError> {
    let addrs: Vec<SocketAddr> =
        (host, port).to_socket_addrs().map_err(|_| ClientError::Resolve(host.to_string()))?.collect();
    let mut last = None;
    for a in &addrs {
        match TcpStream::connect_timeout(a, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or_else(|| ClientError::Resolve(host.to_string()), ClientError::Io))
}

impl EtpClient {
    /// Connects, reads the greeting, logs in (`USER`/`PASS`) and enables UTF-8.
    pub fn connect(target: &Target, timeout: Duration) -> Result<Self, ClientError> {
        let stream = connect_any(&target.host, target.port, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let _ = stream.set_nodelay(true);
        let peer = stream.peer_addr()?;
        let mut c = Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
            peer,
            timeout,
            sent: SearchState::default(),
            welcome: String::new(),
        };
        let hello = c.read_reply()?;
        if hello.code != 220 {
            return Err(ClientError::Reply { code: hello.code, text: hello.text().to_string() });
        }
        c.welcome = hello.lines.join("\n");
        let user = if target.username.is_empty() { "anonymous" } else { target.username.as_str() };
        let r = c.command(&format!("USER {user}"))?;
        let r = if r.code == 331 { c.command(&format!("PASS {}", target.password))? } else { r };
        if r.code != 230 {
            return Err(ClientError::Login(format!("{} {}", r.code, r.text())));
        }
        // Everything sends this and ignores the answer.
        let _ = c.command("OPTS UTF8 ON")?;
        Ok(c)
    }

    /// The server's greeting text.
    pub fn welcome(&self) -> &str {
        &self.welcome
    }

    /// Server address.
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    fn read_line(&mut self) -> Result<String, ClientError> {
        let mut buf = Vec::new();
        let n = (&mut self.reader).take(proto::MAX_LINE as u64 * 8).read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Err(ClientError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed")));
        }
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// Reads one (possibly multi-line) reply.
    pub fn read_reply(&mut self) -> Result<Reply, ClientError> {
        let mut p = ReplyParser::default();
        loop {
            let line = self.read_line()?;
            if let Some(r) = p.push(&line).map_err(ClientError::Protocol)? {
                return Ok(r);
            }
        }
    }

    /// Sends one command line and reads its reply.
    pub fn command(&mut self, line: &str) -> Result<Reply, ClientError> {
        self.writer.write_all(format!("{}\r\n", proto::one_line(line)).as_bytes())?;
        self.read_reply()
    }

    fn expect_ok(&mut self, line: &str) -> Result<Reply, ClientError> {
        let r = self.command(line)?;
        if r.is_ok() { Ok(r) } else { Err(ClientError::Reply { code: r.code, text: r.text().to_string() }) }
    }

    /// Runs a search: sends the options that differ from what the server has (as
    /// `EVERYTHING <option> <value>`, like Everything's client) and `EVERYTHING QUERY`.
    pub fn query(&mut self, want: &SearchState) -> Result<QueryPage, ClientError> {
        for cmd in want.commands_from(&self.sent) {
            let r = self.command(&format!("EVERYTHING {cmd}"))?;
            if !r.is_ok() {
                // The server may not know a newer option; it keeps its old value.
                tracing::debug!(command = %cmd, reply = %r.text(), "ETP option refused");
            }
        }
        self.sent = want.clone();
        let r = self.command("EVERYTHING QUERY")?;
        if !r.is_ok() {
            return Err(ClientError::Reply { code: r.code, text: r.text().to_string() });
        }
        proto::parse_query_reply(&r).map_err(ClientError::Protocol)
    }

    /// Opens a passive data connection (`EPSV`, falling back to `PASV`).
    fn passive(&mut self) -> Result<TcpStream, ClientError> {
        let r = self.command("EPSV")?;
        let addr = if r.code == 229 {
            let port = proto::parse_epsv(r.text()).ok_or_else(|| ClientError::Protocol(r.text().to_string()))?;
            SocketAddr::new(self.peer.ip(), port)
        } else {
            let r = self.expect_ok("PASV")?;
            let a = proto::parse_host_port(r.text()).ok_or_else(|| ClientError::Protocol(r.text().to_string()))?;
            // Behind NAT the advertised address may be private: use the control address.
            let ip = if a.ip().is_unspecified() { self.peer.ip() } else { IpAddr::V4(*a.ip()) };
            SocketAddr::new(ip, a.port())
        };
        let s = TcpStream::connect_timeout(&addr, self.timeout)?;
        s.set_read_timeout(Some(self.timeout))?;
        Ok(s)
    }

    /// Runs a data command (`RETR`, `LIST`, …) and copies the data to `out`; returns the
    /// byte count.
    pub fn data_command(&mut self, line: &str, out: &mut dyn Write) -> Result<u64, ClientError> {
        let _ = self.expect_ok("TYPE I")?;
        let mut data = self.passive()?;
        let r = self.command(line)?;
        if !r.is_preliminary() {
            return Err(ClientError::Reply { code: r.code, text: r.text().to_string() });
        }
        let n = io::copy(&mut data, out)?;
        drop(data);
        let done = self.read_reply()?;
        if done.code != 226 && done.code != 250 {
            return Err(ClientError::Reply { code: done.code, text: done.text().to_string() });
        }
        Ok(n)
    }

    /// Downloads the server file `server_path` (an index path such as `C:\Data\a.txt`, as
    /// returned by a query) into `out`.
    pub fn retr(&mut self, server_path: &str, out: &mut dyn Write) -> Result<u64, ClientError> {
        let ftp = crate::namespace::ftp_path(server_path);
        self.data_command(&format!("RETR {ftp}"), out)
    }

    /// Lists an FTP folder (`MLSD` facts lines).
    pub fn mlsd(&mut self, ftp_path: &str) -> Result<Vec<String>, ClientError> {
        let mut buf = Vec::new();
        self.data_command(&format!("MLSD {ftp_path}"), &mut buf)?;
        Ok(String::from_utf8_lossy(&buf).lines().map(str::to_string).collect())
    }

    /// Sends `QUIT`.
    pub fn quit(mut self) {
        let _ = self.command("QUIT");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_targets() {
        let t = Target::parse("bob:pw@host:2121", 21).unwrap();
        assert_eq!((t.username.as_str(), t.password.as_str(), t.host.as_str(), t.port), ("bob", "pw", "host", 2121));
        let t = Target::parse("192.168.1.5", 21).unwrap();
        assert_eq!((t.host.as_str(), t.port, t.username.as_str()), ("192.168.1.5", 21, ""));
        let t = Target::parse("ftp://u@[::1]:99/", 21).unwrap();
        assert_eq!((t.host.as_str(), t.port, t.username.as_str()), ("::1", 99, "u"));
        assert_eq!(Target::parse("::1", 21).unwrap().host, "::1");
        // A password may contain `@` and `:`.
        let t = Target::parse("u:p@ss:w@h", 21).unwrap();
        assert_eq!((t.username.as_str(), t.password.as_str(), t.host.as_str()), ("u", "p@ss:w", "h"));
        assert!(Target::parse("h:notaport", 21).is_none());
        assert!(Target::parse("", 21).is_none());
    }
}
