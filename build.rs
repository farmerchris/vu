//! On macOS, embed `macos/Info.plist` in the binaries, so the window's
//! application menu is titled "VU Meter" and the microphone prompt says why.

fn main() {
    println!("cargo:rerun-if-changed=macos/Info.plist");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("macos/Info.plist");
        for bin in ["vu-meter", "vu-meter-gui"] {
            println!(
                "cargo:rustc-link-arg-bin={bin}=-Wl,-sectcreate,__TEXT,__info_plist,{}",
                plist.display()
            );
        }
    }
}
