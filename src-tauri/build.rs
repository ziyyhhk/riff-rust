fn main() {
    // The window/tray icon and the Windows resource must track the icon files.
    // Without this, cargo considers the build script fresh after icons are
    // replaced and keeps embedding the old crab logo forever.
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-changed=icons/tray-32x32.rgba");
    tauri_build::build()
}
