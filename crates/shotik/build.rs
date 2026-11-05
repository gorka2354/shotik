//! Icon, version and manifest inside the executable: per-monitor DPI awareness from process start, and
//! modern common controls for native menus and dialogs.

fn main() {
    println!("cargo:rerun-if-changed=shotik.manifest");
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("../../assets/icon.ico")
        .set_manifest_file("shotik.manifest")
        .set("ProductName", "Shotik")
        .set("FileDescription", "Shotik")
        .set("OriginalFilename", "Shotik.exe");
    resource.compile().expect("the Windows resources must compile");
}
