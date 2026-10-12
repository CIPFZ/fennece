//! Fennec property, content, hash and duplicate extraction.
//!
//! Scheduled for milestone M3 (SPEC ch.6 §3). The crate exists from M0 so the
//! workspace layout, dependency graph and CI are final; see `README.md`.

#![forbid(unsafe_code)]

/// Milestone in which this crate's functionality lands.
pub const MILESTONE: &str = "M3";

#[cfg(test)]
mod tests {
    #[test]
    fn builds() {
        assert_eq!(super::MILESTONE, "M3");
    }
}
