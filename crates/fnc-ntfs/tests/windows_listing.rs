//! Windows only: the size/date back-fill listing does not follow junctions or directory
//! symlinks (their target's children would otherwise be added under the link).
#![cfg(windows)]

use std::process::Command;

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let _ = fnc_ntfs::win::list_dir_ids(&dir.display().to_string(), |name, _, _| {
        out.push(String::from_utf16_lossy(name));
    });
    out.sort();
    out
}

#[test]
fn listing_does_not_follow_links() {
    let root = std::env::temp_dir().join(format!("fennec-listing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let target = root.join("target");
    std::fs::create_dir_all(target.join("sub")).unwrap();
    std::fs::write(target.join("a.txt"), b"a").unwrap();
    let junction = root.join("junction");
    let ok = Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&junction).arg(&target).status().unwrap().success();
    assert!(ok, "mklink /J");
    assert_eq!(names(&target), ["a.txt", "sub"]);
    assert!(names(&junction).is_empty(), "junction followed: {:?}", names(&junction));
    // Directory symlinks need the privilege (administrators); checked when available.
    let link = root.join("dirlink");
    if std::os::windows::fs::symlink_dir(&target, &link).is_ok() {
        assert!(names(&link).is_empty(), "directory symlink followed");
    }
    let _ = std::fs::remove_dir(&junction);
    let _ = std::fs::remove_dir(&link);
    let _ = std::fs::remove_dir_all(&root);
}
