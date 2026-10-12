//! Single instance (SPEC ch.0 §4.1, ch.3 §11).
//!
//! The first process for an instance name becomes *primary*: on Windows it owns the
//! named mutex `Local\Fennec.<instance>`; everywhere it listens on the local socket
//! `Fennec.<instance>.cmd` (a named pipe on Windows, an abstract/namespaced socket on
//! Linux). Later processes forward their command line to the primary as one JSON line
//! and exit.

use std::io::{self, BufRead, BufReader, Write};

use crossbeam_channel::{Receiver, Sender};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream};

/// Outcome of [`acquire`].
pub enum Instance {
    /// This process is the primary instance; forwarded command lines arrive on
    /// [`Primary::commands`].
    Primary(Primary),
    /// Another instance is running and received our command line.
    Secondary,
}

/// Primary-instance handle.
pub struct Primary {
    rx: Receiver<Vec<String>>,
    #[cfg(windows)]
    _mutex: win::NamedMutex,
}

impl Primary {
    /// Command lines forwarded by later launches.
    pub fn commands(&self) -> &Receiver<Vec<String>> {
        &self.rx
    }
}

fn pipe_name(instance: &str) -> String {
    if instance.is_empty() { "Fennec..cmd".to_string() } else { format!("Fennec.{instance}.cmd") }
}

fn send_once(name: &str, args: &[String]) -> io::Result<()> {
    let ns = name.to_ns_name::<GenericNamespaced>()?;
    let mut conn = Stream::connect(ns)?;
    let mut line = serde_json::to_string(args).map_err(io::Error::other)?;
    line.push('\n');
    conn.write_all(line.as_bytes())?;
    conn.flush()
}

/// Forwards `args`, retrying for ~2 s: the primary may hold the mutex but not be
/// listening yet (it is still starting up).
fn send(name: &str, args: &[String]) -> io::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match send_once(name, args) {
            Ok(()) => return Ok(()),
            Err(e) if std::time::Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}

/// `true` if creating the listener failed because another process owns the name.
/// Windows reports an existing first-instance pipe as access denied.
fn name_in_use(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied)
}

fn serve(listener: interprocess::local_socket::Listener, tx: Sender<Vec<String>>, notify: Box<dyn Fn() + Send>) {
    for conn in listener.incoming().filter_map(Result::ok) {
        let mut line = String::new();
        let mut reader = BufReader::new(conn);
        if reader.read_line(&mut line).is_ok()
            && let Ok(args) = serde_json::from_str::<Vec<String>>(line.trim_end())
        {
            if tx.send(args).is_err() {
                return;
            }
            notify();
        }
    }
}

/// Becomes primary for `instance` or forwards `args` to the running primary.
/// `notify` is called (from a background thread) whenever a command arrives.
/// The returned [`Primary`] must be kept alive for the whole process lifetime.
pub fn acquire(instance: &str, args: &[String], notify: Box<dyn Fn() + Send>) -> io::Result<Instance> {
    let name = pipe_name(instance);
    #[cfg(windows)]
    let mutex = match win::NamedMutex::create(instance)? {
        Some(m) => m,
        None => {
            send(&name, args)?;
            return Ok(Instance::Secondary);
        }
    };
    let ns = name.as_str().to_ns_name::<GenericNamespaced>()?;
    let listener = match ListenerOptions::new().name(ns).create_sync() {
        Ok(l) => l,
        Err(e) if name_in_use(&e) => {
            send(&name, args)?;
            return Ok(Instance::Secondary);
        }
        Err(e) => return Err(e),
    };
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new().name("fennec-instance".into()).spawn(move || serve(listener, tx, notify))?;
    Ok(Instance::Primary(Primary {
        rx,
        #[cfg(windows)]
        _mutex: mutex,
    }))
}

#[cfg(windows)]
mod win {
    use std::io;

    use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::PCWSTR;

    /// Owned named mutex handle.
    pub struct NamedMutex(HANDLE);

    // SAFETY: a mutex handle is a kernel object reference usable from any thread.
    unsafe impl Send for NamedMutex {}

    impl NamedMutex {
        /// Creates `Local\Fennec.<instance>`; `None` if it already exists.
        pub fn create(instance: &str) -> io::Result<Option<Self>> {
            let name: Vec<u16> = format!("Local\\Fennec.{instance}").encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: `name` is NUL-terminated and outlives the call; default security.
            let h = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }.map_err(io::Error::other)?;
            // SAFETY: reads the calling thread's last-error value set by CreateMutexW.
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                // SAFETY: `h` is a valid handle returned above, closed once.
                let _ = unsafe { CloseHandle(h) };
                return Ok(None);
            }
            Ok(Some(Self(h)))
        }
    }

    impl Drop for NamedMutex {
        fn drop(&mut self) {
            // SAFETY: the handle is owned by this value and closed exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_launch_forwards_its_command_line() {
        let inst = format!("test-{}", std::process::id());
        let primary = match acquire(&inst, &[], Box::new(|| {})).unwrap() {
            Instance::Primary(p) => p,
            Instance::Secondary => panic!("first acquire must be primary"),
        };
        let args = vec!["-s".to_string(), "ext:pdf".to_string()];
        assert!(matches!(acquire(&inst, &args, Box::new(|| {})).unwrap(), Instance::Secondary));
        let got = primary.commands().recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(got, args);
    }
}
