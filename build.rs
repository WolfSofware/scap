fn main() {
    use std::path::Path;
    use std::process::Command;

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let Ok(output) = Command::new("xcode-select").arg("-p").output() else {
        return;
    };
    let root = String::from_utf8_lossy(&output.stdout);
    let root = root.trim();
    let candidates = [
        format!("{root}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx"),
        format!("{root}/usr/lib/swift/macosx"),
    ];
    if let Some(path) = candidates.iter().find(|path| Path::new(path).is_dir()) {
        println!("cargo:rustc-link-search=native={path}");
    }
}
