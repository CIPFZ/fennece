//! Embeds the application icon and version information into `fennec.exe`.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon/fennec.ico");
    // Resource compilation needs the Windows SDK, so it only runs on Windows hosts.
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("../../assets/icon/fennec.ico");
            res.set("ProductName", "Fennec");
            res.set("FileDescription", "Fennec — find anything in a blink");
            res.set("OriginalFilename", "fennec.exe");
            if let Err(e) = res.compile() {
                println!("cargo:warning=could not embed the icon resource: {e}");
            }
        }
    }
}
