//! `fennec-service` — Windows service that reads MFT/USN data as LocalSystem and
//! streams it to non-elevated Fennec instances over `\\.\pipe\Fennec.<instance>.svc`
//! (SPEC ch.1 §2.4, ch.4 §8). Implemented in milestone M2 (IDX-10).

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!("fennec-service {}: the service host is scheduled for milestone M2", env!("CARGO_PKG_VERSION"));
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    #[test]
    fn builds() {}
}
