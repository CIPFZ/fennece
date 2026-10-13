//! Queries against the real Windows Search system index (Windows only, `--ignored`: needs
//! the Windows Search service, and administrator rights to add the test folder to the
//! crawl scope). The folder is `FNC_WINSEARCH_TEST_DIR` or `target\winsearch-test`.

#![cfg(windows)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fnc_platform::winsearch::{Error, Hits, include_folder, indexer_version, remove_folder, search};

fn test_dir() -> PathBuf {
    std::env::var_os("FNC_WINSEARCH_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..\\..\\target\\winsearch-test"))
}

fn has(h: &Hits, name: &str) -> bool {
    h.paths.iter().any(|p| p.to_ascii_lowercase().ends_with(&format!("\\{name}")))
}

#[test]
#[ignore = "needs the Windows Search service and administrator rights"]
fn system_index_queries() {
    eprintln!("indexer {}", indexer_version().unwrap());
    let dir = test_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    // `canonicalize` gives a `\\?\` path; Windows Search wants the plain one.
    let dir = PathBuf::from(dir.to_string_lossy().trim_start_matches("\\\\?\\"));
    let token = format!("fncsi{}", std::process::id());
    std::fs::write(dir.join("alpha.txt"), format!("quarterly {token} report")).unwrap();
    std::fs::write(dir.join("beta.txt"), format!("{token} appears here too")).unwrap();
    std::fs::write(dir.join("gamma.txt"), "nothing to see").unwrap();
    include_folder(&dir).unwrap();

    // Wait for the crawler to pick the files up.
    let t0 = Instant::now();
    let hits = loop {
        match search(&token, 100, Duration::from_secs(30), &|| false) {
            Ok(h) if has(&h, "alpha.txt") && has(&h, "beta.txt") => break h,
            Ok(_) | Err(Error::Failed(_) | Error::TimedOut(_)) if t0.elapsed() < Duration::from_secs(300) => {
                std::thread::sleep(Duration::from_secs(2));
            }
            other => panic!("not indexed after {:?}: {other:?}", t0.elapsed()),
        }
    };
    eprintln!("indexed after {:?}: {:?}", t0.elapsed(), hits.paths);
    assert!(!has(&hits, "gamma.txt"));
    assert!(!hits.truncated);
    let root = dir.to_string_lossy().to_ascii_lowercase();
    assert!(hits.paths.iter().all(|p| p.to_ascii_lowercase().starts_with(&root)), "{:?}", hits.paths);

    // Phrases, properties and the limit.
    let phrase = search(&format!("\"quarterly {token}\""), 100, Duration::from_secs(30), &|| false).unwrap();
    assert!(has(&phrase, "alpha.txt") && !has(&phrase, "beta.txt"), "{phrase:?}");
    let named = search(&format!("{token} filename:beta.txt"), 100, Duration::from_secs(30), &|| false).unwrap();
    assert!(has(&named, "beta.txt") && !has(&named, "alpha.txt"), "{named:?}");
    let one = search(&token, 1, Duration::from_secs(30), &|| false).unwrap();
    assert_eq!(one.paths.len(), 1);
    assert!(one.truncated);
    assert_eq!(search(&token, 100, Duration::from_secs(30), &|| true), Err(Error::Cancelled));

    remove_folder(&dir).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
