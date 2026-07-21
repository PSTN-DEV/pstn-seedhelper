use sysinfo::{ProcessesToUpdate, System};

pub fn kill_squad() {
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All);

    let mut killed = false;
    for (pid, proc) in sys.processes() {
        let name = proc.name().to_string_lossy();
        if name.contains("SquadGame") || name.contains("squad_launcher") {
            proc.kill();
            log::info!("Завершён процесс: {} (PID: {})", name, pid);
            killed = true;
        }
    }

    if !killed {
        log::info!("Процессы Squad не найдены");
    }
}
