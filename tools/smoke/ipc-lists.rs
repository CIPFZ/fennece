//! Prints the filters and bookmarks a running Fennec instance has loaded (native IPC
//! `ListFilters` / `ListBookmarks`), one per line, tab-separated; used by
//! `release-smoke.ps1` to check that an upgraded data folder was really loaded.
//!
//! Not part of the workspace. Build it as a standalone crate next to a checkout:
//!   [package] name = "smoke-ipc", edition = "2024"; [workspace];
//!   [dependencies] fnc-sdk = { path = "<checkout>/crates/fnc-sdk" }
//!   [patch.crates-io] llama-cpp-sys-2 / llama-cpp-2 = { path = "<checkout>/third_party/…" }
//! with this file as src/main.rs, then `cargo build --release`.
use std::time::Duration;

fn main() {
    let instance = std::env::args().nth(1).unwrap_or_default();
    let mut f = match fennec_sdk::Fennec::connect_instance(&instance, Duration::from_secs(30)) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("connect: {e}");
            std::process::exit(2);
        }
    };
    for x in f.filters().unwrap_or_default() {
        println!("filter\t{}\t{}\t{}", x.name, x.search, x.macro_name);
    }
    for x in f.bookmarks().unwrap_or_default() {
        println!("bookmark\t{}\t{}", x.name, x.search);
    }
}
