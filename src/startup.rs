use anyhow::{Context, Result};
use std::path::PathBuf;

fn startup_folder() -> PathBuf {
    std::env::var("APPDATA")
        .map(|p| {
            PathBuf::from(p)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("Startup")
        })
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn bat_path() -> PathBuf {
    startup_folder().join("SquadSeeder_Startup.bat")
}

pub fn enable() -> Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;
    let content = format!("@echo off\nstart \"\" \"{}\"\n", exe.display());
    std::fs::write(bat_path(), content).context("write startup bat")?;
    Ok(())
}

pub fn disable() -> Result<()> {
    let path = bat_path();
    if path.exists() {
        std::fs::remove_file(path).context("remove startup bat")?;
    }
    Ok(())
}

pub fn set(enabled: bool) -> Result<()> {
    if enabled { enable() } else { disable() }
}
