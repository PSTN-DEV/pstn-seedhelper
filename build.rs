fn main() {
    slint_build::compile("src/ui/main.slint").expect("Slint compile error");

    // Bake target arch into binary so updater requests the correct release asset
    // Map Rust target arch to the platform token the backend expects
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".to_string());
    let platform = match arch.as_str() { "x86" => "x86", _ => "x64" };
    println!("cargo:rustc-env=APP_ARCH={platform}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
        let win_version = format!("{}.0", pkg_version);
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon.ico");
        res.set("FileDescription", "Seed Helper");
        res.set("ProductName", "Seed Helper");
        res.set("CompanyName", "PSTN Squad");
        res.set("LegalCopyright", "© PSTN Squad");
        res.set("InternalName", "Seed Helper");
        res.set("OriginalFilename", "Seed Helper.exe");
        res.set("FileVersion", &win_version);
        res.set("ProductVersion", &win_version);
        res.set_manifest(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0"
          xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">True/PM</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>"#,
        );
        res.compile().expect("Windows resource compile error");
    }
}
