use anyhow::{Context, Result};

const REG_KEY: &str = r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
const REG_NAME: &str = "Seed Helper";

#[cfg(windows)]
pub fn enable() -> Result<()> {
    use std::os::windows::process::CommandExt;
    cleanup_legacy();
    let exe = std::env::current_exe().context("current_exe")?;
    std::process::Command::new("reg")
        .args(["add", REG_KEY, "/v", REG_NAME, "/t", "REG_SZ",
               "/d", &exe.to_string_lossy(), "/f"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .context("reg add")?;
    Ok(())
}

#[cfg(windows)]
pub fn disable() -> Result<()> {
    use std::os::windows::process::CommandExt;
    cleanup_legacy();
    // ignore error — key may not exist if startup was never enabled
    let _ = std::process::Command::new("reg")
        .args(["delete", REG_KEY, "/v", REG_NAME, "/f"])
        .creation_flags(0x08000000)
        .output();
    Ok(())
}

#[cfg(windows)]
fn cleanup_legacy() {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let bat = std::path::PathBuf::from(appdata)
            .join(r"Microsoft\Windows\Start Menu\Programs\Startup\SquadSeeder_Startup.bat");
        let _ = std::fs::remove_file(bat);
    }
}

#[cfg(not(windows))]
pub fn enable() -> Result<()> { Ok(()) }

#[cfg(not(windows))]
pub fn disable() -> Result<()> { Ok(()) }

pub fn set(enabled: bool) -> Result<()> {
    if enabled { enable() } else { disable() }
}
