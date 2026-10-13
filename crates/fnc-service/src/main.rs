//! `fennec-service` — Windows service that reads MFT/USN data as LocalSystem and
//! serves it to non-elevated Fennec instances over `\\.\pipe\Fennec.<instance>.svc`
//! (SPEC ch.1 §2.4, ch.4 §8).
//!
//! ```text
//! fennec-service -install [-instance <name>]    register (auto start) and start
//! fennec-service -uninstall [-instance <name>]  stop and remove
//! fennec-service -console [-instance <name>]    run the pipe server in the console
//! fennec-service                                 (started by the service manager)
//! ```
//!
//! The server answers only `QueryJournal`, `EnumMft` and `ReadUsn` (see
//! `fnc_ntfs::proto`) for local fixed NTFS/ReFS drives; the pipe DACL admits
//! SYSTEM, Administrators and interactive users.

#![forbid(unsafe_code)]
// Console tool: install/uninstall report on stdout.
#![allow(clippy::print_stdout)]

#[cfg(windows)]
mod server;
#[cfg(windows)]
mod service;

use std::process::ExitCode;

/// Parsed command line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    install: bool,
    uninstall: bool,
    start: bool,
    stop: bool,
    console: bool,
    instance: String,
    help: bool,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.into_iter();
    while let Some(s) = it.next() {
        match s.trim_start_matches(['-', '/']).to_ascii_lowercase().as_str() {
            "install" => a.install = true,
            "uninstall" => a.uninstall = true,
            "start" => a.start = true,
            "stop" => a.stop = true,
            "console" => a.console = true,
            "instance" => a.instance = it.next().ok_or("-instance needs a name")?,
            "help" | "h" | "?" => a.help = true,
            _ => return Err(format!("unknown argument {s}")),
        }
    }
    if a.instance.chars().any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
        return Err("instance names may only contain letters, digits, '-' and '_'".into());
    }
    Ok(a)
}

const USAGE: &str = "fennec-service -install | -uninstall | -start | -stop | -console  [-instance <name>]";

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if args.help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    run(args)
}

#[cfg(windows)]
fn run(args: Args) -> ExitCode {
    let result = if args.install {
        service::install(&args.instance)
    } else if args.uninstall {
        service::uninstall(&args.instance)
    } else if args.start || args.stop {
        service::start_stop(&args.instance, args.start)
    } else if args.console {
        let _log = init_logging(true);
        server::serve(&args.instance, &fnc_platform::StopFlag::new()).map_err(|e| e.to_string())
    } else {
        let _log = init_logging(false);
        service::dispatch(&args.instance)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fennec-service: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn init_logging(console: bool) -> Option<fnc_core::logging::LogGuard> {
    // `%ProgramData%\Fennec\logs` (the service runs as LocalSystem).
    let dir = std::env::var_os("ProgramData")
        .map_or_else(|| std::path::PathBuf::from(r"C:\ProgramData"), std::path::PathBuf::from)
        .join("Fennec")
        .join("logs");
    fnc_core::logging::init(&fnc_core::logging::LogConfig {
        dir: Some(&dir),
        prefix: "fennec-service",
        level: "info",
        max_files: 7,
        console,
    })
    .ok()
}

#[cfg(not(windows))]
fn run(_args: Args) -> ExitCode {
    eprintln!("fennec-service only runs on Windows (other platforms need no service).");
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: &[&str]) -> Result<Args, String> {
        parse(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn arguments() {
        assert_eq!(p(&[]).unwrap(), Args::default());
        assert!(p(&["-install"]).unwrap().install);
        assert!(p(&["/Uninstall"]).unwrap().uninstall);
        assert_eq!(p(&["-console", "-instance", "work"]).unwrap().instance, "work");
        assert!(p(&["-instance"]).is_err());
        assert!(p(&["-instance", "a b"]).is_err());
        assert!(p(&["-bogus"]).is_err());
    }
}
