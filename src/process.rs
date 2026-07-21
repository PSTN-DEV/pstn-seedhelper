use sysinfo::{ProcessesToUpdate, System};

const SQUAD_CLIENT: &str = "SquadGame-Win64-Shipping";
const SQUAD_SERVER: &str = "SquadGameServer";
const SQUAD_LAUNCHER: &str = "squad_launcher";

/// Single sysinfo pass for both Squad and Steam — avoids two separate refreshes.
pub fn check_processes() -> (bool /* squad */, bool /* steam */) {
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All);
    let mut squad = false;
    let mut steam = false;
    for p in sys.processes().values() {
        let name = p.name().to_string_lossy();
        if name.contains(SQUAD_CLIENT) { squad = true; }
        if name.eq_ignore_ascii_case("steam.exe") || name.eq_ignore_ascii_case("steam") {
            steam = true;
        }
        if squad && steam { break; }
    }
    (squad, steam)
}

pub fn is_squad_client_running() -> bool {
    check_processes().0
}

pub fn kill_squad() {
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All);
    for proc in sys.processes().values() {
        let name = proc.name().to_string_lossy();
        if name.contains(SQUAD_SERVER) { continue; }
        if name.contains(SQUAD_CLIENT) || name.contains(SQUAD_LAUNCHER) {
            proc.kill();
        }
    }
}
