use anyhow::Result;
use chrono::Timelike;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

use crate::api::HubApi;
use crate::app::LogSender;
use crate::config::Config;

pub enum SeedResult {
    Success,
    Restart,
    Failed,
    Cancelled,
    NightMode,
    PeriodEnd,
}

pub enum SeedOutcome {
    Completed,
    NightCompleted,
    Cancelled,
}

/// Interruptible sleep: Ok after `secs`, Err if cancelled.
async fn isleep(secs: u64, token: &CancellationToken) -> Result<(), ()> {
    tokio::select! {
        _ = sleep(Duration::from_secs(secs)) => Ok(()),
        _ = token.cancelled() => Err(()),
    }
}

/// Launch Squad (eco or Steam), including backup/modify/restore for eco.
/// Re-used for restarts inside the server loop.
async fn do_launch(
    config: &Config,
    token: &CancellationToken,
    log: &LogSender,
) -> anyhow::Result<()> {
    if crate::process::is_squad_client_running() {
        let _ = log.send("Squad уже запущен — пропускаем запуск".into());
        // INI was never touched, so unblock stop immediately in eco mode.
        if config.eco_mode && !config.render_toggle {
            let _ = log.send("\x00restore_toast".into());
        }
        return Ok(());
    }
    if config.delete_startup_video {
        crate::game::remove_welcome_video(log);
    }
    if config.eco_mode {
        crate::game::launch_game_eco(config, token, log).await?;
    } else {
        crate::game::launch_game_steam(config, token, log).await?;
    }
    Ok(())
}

/// Returns true if every online server in `order` already has enough players.
/// Offline servers are skipped — they can't be seeded anyway.
async fn all_seeded(order: &[u8], threshold: u32, api: &HubApi, log: &LogSender) -> bool {
    let servers = match api.get_all_servers().await {
        Ok(s) => s,
        Err(_) => return false,
    };
    for &num in order {
        let tag = crate::api::tag_for(num).unwrap_or("");
        if let Some(s) = servers.get(tag) {
            if s.is_online() && s.players < threshold {
                return false;
            }
        }
    }
    let _ = log.send("Все активные сервера уже заполнены — запуск игры не требуется".into());
    true
}

pub async fn start_seeding(
    config: Config,
    api: Arc<HubApi>,
    token: CancellationToken,
    log: LogSender,
    seeding_server: Arc<std::sync::atomic::AtomicBool>,
    crash_restart: Arc<std::sync::atomic::AtomicBool>,
) -> SeedOutcome {
    macro_rules! log {
        ($($arg:tt)*) => {{ let _ = log.send(format!($($arg)*)); }};
    }

    log!("Начинаем Seed серверов!");

    // 1. Network wait (up to 5 min)
    log!("Проверка доступности сети...");
    let mut ready = false;
    for _ in 0..20 {
        if token.is_cancelled() {
            return SeedOutcome::Cancelled;
        }
        if api.ping().await {
            log!("Сеть доступна!");
            ready = true;
            break;
        }
        log!("Сеть недоступна, повтор через 15 сек...");
        if isleep(15, &token).await.is_err() {
            return SeedOutcome::Cancelled;
        }
    }
    if !ready {
        log!("Сеть недоступна после 5 минут — seed отменён");
        return SeedOutcome::Completed;
    }

    // 2. Validate config
    if let Err(e) = crate::game::validate_config(&config) {
        log!("Ошибка конфига: {e}");
        return SeedOutcome::Completed;
    }

    // 2a. Jump straight to night mode if window is already open
    if config.night_mode_enabled && is_in_night_window(
        config.night_start_hour, config.night_start_minute,
        config.night_end_hour, config.night_end_minute,
    ) {
        let _ = log.send("\x00night_mode_on".into());
        log!("Ночной период — запускаем ночной режим");
        let done = start_night_seeding(config, api, token, log.clone(), seeding_server).await;
        let _ = log.send("\x00night_mode_off".into());
        return if done { SeedOutcome::NightCompleted } else { SeedOutcome::Cancelled };
    }

    // 3. Resolve seed order
    let order = resolve_seed_order(&api, &log).await;

    // 4. Skip launch if every online server is already seeded
    if all_seeded(&order, config.desired_players, &api, &log).await {
        return SeedOutcome::Completed;
    }

    // 5. Launch game
    if let Err(e) = do_launch(&config, &token, &log).await {
        log!("Ошибка запуска игры: {e}");
        if config.eco_mode {
            crate::game::restore_ini_keys(&config);
        }
        return if token.is_cancelled() { SeedOutcome::Cancelled } else { SeedOutcome::Completed };
    }
    if token.is_cancelled() {
        log!("Seed остановлен.");
        return SeedOutcome::Cancelled;
    }

    // 6. Server seed loop
    'servers: for &server_num in &order {
        if token.is_cancelled() {
            break;
        }

        // Transition to night mode if window opened between servers
        if config.night_mode_enabled && is_in_night_window(
            config.night_start_hour, config.night_start_minute,
            config.night_end_hour, config.night_end_minute,
        ) {
            let _ = log.send("\x00night_mode_on".into());
            log!("Ночной период — переключаемся на ночной режим");
            let done = start_night_seeding(config.clone(), api.clone(), token.clone(), log.clone(), seeding_server.clone()).await;
            let _ = log.send("\x00night_mode_off".into());
            if !token.is_cancelled() {
                crate::process::kill_squad();
                if config.eco_mode { crate::game::restore_ini_keys(&config); }
            }
            return if done { SeedOutcome::NightCompleted } else { SeedOutcome::Cancelled };
        }

        loop {
            if token.is_cancelled() {
                break 'servers;
            }

            match seed_server(server_num, &config, &api, &token, &log, &seeding_server).await {
                SeedResult::NightMode => {
                    let _ = log.send("\x00night_mode_on".into());
                    log!("Ночной период — переключаемся на ночной режим");
                    let done = start_night_seeding(config.clone(), api.clone(), token.clone(), log.clone(), seeding_server.clone()).await;
                    let _ = log.send("\x00night_mode_off".into());
                    if !token.is_cancelled() {
                        crate::process::kill_squad();
                        if config.eco_mode { crate::game::restore_ini_keys(&config); }
                    }
                    return if done { SeedOutcome::NightCompleted } else { SeedOutcome::Cancelled };
                }
                SeedResult::PeriodEnd => {
                    log!("Период сидинга завершён — seed остановлен");
                    break 'servers;
                }
                SeedResult::Cancelled => break 'servers,
                SeedResult::Restart => {
                    log!("Перезапуск игры для сервера {server_num}...");
                    if let Err(e) = do_launch(&config, &token, &log).await {
                        log!("Ошибка перезапуска: {e}");
                        break 'servers;
                    }
                    if crash_restart.swap(false, std::sync::atomic::Ordering::AcqRel) {
                        log!("Игра перезапущена после краша — пропускаем переподключение");
                        break;
                    }
                    continue;
                }
                SeedResult::Failed => {
                    log!("Не удалось заполнить сервер {server_num}");
                    break;
                }
                SeedResult::Success => {
                    let stop_after = config.stop_after_server;
                    if stop_after != 0 && server_num == stop_after {
                        log!("Сервер {server_num} заполнен — достигнут сервер остановки");
                        break 'servers;
                    }
                    break;
                }
            }
        }
    }

    // 7. Cleanup — kill first: Squad rewrites resolution/fps keys on every map
    // change, so the INI restore only sticks once the process is dead.
    if !token.is_cancelled() {
        log!("Все сервера обработаны!");
        crate::process::kill_squad();
        if config.eco_mode {
            crate::game::restore_ini_keys(&config);
            log!("Настройки FPS/разрешения восстановлены");
        }
        SeedOutcome::Completed
    } else {
        // Cancelled: stop_seeding() kills Squad and restores the INI itself.
        log!("Seed остановлен.");
        SeedOutcome::Cancelled
    }
}

/// Returns true if current Moscow time (UTC+3) is inside [start, end).
/// Handles cross-midnight windows, e.g. 23:00 → 05:00.
pub fn is_in_night_window(start_h: u32, start_m: u32, end_h: u32, end_m: u32) -> bool {
    let now = chrono::Local::now();
    let now_mins = now.hour() * 60 + now.minute();
    let start_mins = start_h * 60 + start_m;
    let end_mins = end_h * 60 + end_m;
    if start_mins > end_mins {
        now_mins >= start_mins || now_mins < end_mins
    } else {
        now_mins >= start_mins && now_mins < end_mins
    }
}

/// Night-mode seeding: find servers with 50–90 players, join the fullest one.
/// Runs until the night window ends (returns true) or the token is cancelled (returns false).
pub async fn start_night_seeding(
    config: Config,
    api: Arc<HubApi>,
    token: CancellationToken,
    log: LogSender,
    seeding_server: Arc<std::sync::atomic::AtomicBool>,
) -> bool {
    macro_rules! log {
        ($($arg:tt)*) => {{ let _ = log.send(format!($($arg)*)); }};
    }

    const NIGHT_MIN: u32 = 50;
    const NIGHT_MAX: u32 = 90;

    log!("Ночной режим: поиск серверов {NIGHT_MIN}–{NIGHT_MAX} игроков (МСК)");

    let mut current_server: Option<u8> = None;

    loop {
        if token.is_cancelled() {
            seeding_server.store(false, std::sync::atomic::Ordering::Release);
            return false;
        }

        if !is_in_night_window(
            config.night_start_hour, config.night_start_minute,
            config.night_end_hour, config.night_end_minute,
        ) {
            log!("Ночной режим: период завершён");
            crate::process::kill_squad();
            seeding_server.store(false, std::sync::atomic::Ordering::Release);
            return true;
        }

        let servers = match api.get_all_servers().await {
            Ok(s) => s,
            Err(e) => {
                log!("Ночной режим: ошибка API — {e}");
                if isleep(60, &token).await.is_err() {
                    seeding_server.store(false, std::sync::atomic::Ordering::Release);
                    return false;
                }
                continue;
            }
        };

        // Pick online server in [NIGHT_MIN, NIGHT_MAX) with most players
        let best = (1u8..=4)
            .filter_map(|num| {
                let tag = crate::api::tag_for(num)?;
                let s = servers.get(tag)?;
                if s.is_online() && s.players >= NIGHT_MIN && s.players < NIGHT_MAX {
                    Some((num, s.players))
                } else {
                    None
                }
            })
            .max_by_key(|(_, p)| *p)
            .map(|(num, _)| num);

        let target = match best {
            None => {
                log!("Ночной режим: нет серверов в диапазоне {NIGHT_MIN}–{NIGHT_MAX} — режим ожидания");
                // Store false BEFORE killing squad so process_watch_loop doesn't mistake
                // this intentional kill for a crash.
                seeding_server.store(false, std::sync::atomic::Ordering::Release);
                if crate::process::is_squad_client_running() {
                    crate::process::kill_squad();
                }
                // Unblock stop button in eco mode regardless of whether squad was
                // running — if night mode entered stand-by before ever launching,
                // \x00restore_toast was never sent by launch_game_eco.
                if config.eco_mode {
                    let _ = log.send("\x00restore_toast".into());
                }
                current_server = None;
                if isleep(config.checkup_interval, &token).await.is_err() {
                    return false;
                }
                continue;
            }
            Some(t) => t,
        };

        if current_server != Some(target) {
            // First join or server switch — launch game if not running
            if !crate::process::is_squad_client_running() {
                if let Err(e) = do_launch(&config, &token, &log).await {
                    log!("Ночной режим: ошибка запуска — {e}");
                    seeding_server.store(false, std::sync::atomic::Ordering::Release);
                    return !token.is_cancelled();
                }
                if token.is_cancelled() {
                    seeding_server.store(false, std::sync::atomic::Ordering::Release);
                    return false;
                }
            }

            log!("Ночной режим: подключаемся к серверу {target}...");
            let url = match api.join_server(target).await {
                Ok(u) => u,
                Err(e) => {
                    log!("Ночной режим: ошибка URL — {e}");
                    if isleep(60, &token).await.is_err() {
                        seeding_server.store(false, std::sync::atomic::Ordering::Release);
                        return false;
                    }
                    continue;
                }
            };
            if let Err(e) = crate::game::open_steam_url(&url) {
                log!("Ночной режим: ошибка открытия URL — {e}");
                if isleep(60, &token).await.is_err() {
                    seeding_server.store(false, std::sync::atomic::Ordering::Release);
                    return false;
                }
                continue;
            }

            if isleep(120, &token).await.is_err() {
                seeding_server.store(false, std::sync::atomic::Ordering::Release);
                return false;
            }

            // Keep retrying join while server still has room. Stops when:
            // - connected, or
            // - server hit NIGHT_MAX (pointless to join), or
            // - cancelled.
            let mut connected = false;
            loop {
                if token.is_cancelled() {
                    seeding_server.store(false, std::sync::atomic::Ordering::Release);
                    return false;
                }

                // Check player count before every attempt.
                match api.get_server(target).await {
                    Ok(s) if s.players >= NIGHT_MAX => {
                        log!("Ночной режим: сервер {target} достиг {NIGHT_MAX} игроков — отмена подключения");
                        break;
                    }
                    Err(e) => log!("Ночной режим: ошибка статуса — {e}"),
                    _ => {}
                }

                match api.check_player(&config.steam_id, target).await {
                    Ok(true) => { connected = true; break; }
                    Ok(false) => {
                        log!("Ночной режим: подключение не подтверждено — повтор через 2 мин");
                        if let Ok(u) = api.join_server(target).await {
                            let _ = crate::game::open_steam_url(&u);
                        }
                        if isleep(120, &token).await.is_err() {
                            seeding_server.store(false, std::sync::atomic::Ordering::Release);
                            return false;
                        }
                    }
                    Err(e) => {
                        log!("Ночной режим: ошибка проверки — {e}");
                        if isleep(30, &token).await.is_err() {
                            seeding_server.store(false, std::sync::atomic::Ordering::Release);
                            return false;
                        }
                    }
                }
            }

            if !connected {
                log!("Ночной режим: не удалось подключиться к серверу {target}");
                current_server = None;
                seeding_server.store(false, std::sync::atomic::Ordering::Release);
                if isleep(60, &token).await.is_err() { return false; }
                continue;
            }

            current_server = Some(target);
            seeding_server.store(true, std::sync::atomic::Ordering::Release);
            log!("Ночной режим: на сервере {target} — мониторинг");
        }

        // Check whether the server we're on has now reached 90 — if so, force a
        // re-evaluation next iteration so we switch or go to stand-by.
        if let Some(srv) = current_server {
            if let Ok(s) = api.get_server(srv).await {
                if s.players >= NIGHT_MAX {
                    log!("Ночной режим: сервер {srv} достиг {NIGHT_MAX} игроков — переключаемся");
                    seeding_server.store(false, std::sync::atomic::Ordering::Release);
                    crate::process::kill_squad();
                    current_server = None;
                    continue;
                }
            }
        }

        if isleep(config.checkup_interval, &token).await.is_err() {
            seeding_server.store(false, std::sync::atomic::Ordering::Release);
            return false;
        }
    }
}

async fn resolve_seed_order(api: &HubApi, log: &LogSender) -> Vec<u8> {
    match api.get_seed_order().await {
        Ok(order) => {
            let _ = log.send(format!("Порядок сида с сервера: {order:?}"));
            order
        }
        Err(e) => {
            let _ = log.send(format!(
                "Не удалось получить порядок с сервера: {e}. Используем 1-2-3-4"
            ));
            vec![1, 2, 3, 4]
        }
    }
}

struct ActiveServerGuard<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for ActiveServerGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

async fn seed_server(
    server_num: u8,
    config: &Config,
    api: &HubApi,
    token: &CancellationToken,
    log: &LogSender,
    seeding_server: &std::sync::atomic::AtomicBool,
) -> SeedResult {
    macro_rules! log {
        ($($arg:tt)*) => {{ let _ = log.send(format!($($arg)*)); }};
    }

    log!(
        "Сид сервера {} ({})",
        server_num,
        crate::api::name_for(server_num)
    );

    // Check player count first
    let status = match api.get_server(server_num).await {
        Ok(s) => s,
        Err(e) => {
            log!("Не удалось получить статус сервера {server_num}: {e}");
            return SeedResult::Failed;
        }
    };

    if !status.is_online() {
        log!("Сервер {server_num} офлайн — пропускаем");
        return SeedResult::Success;
    }

    if status.players >= config.desired_players {
        log!("Сервер {server_num} уже полон ({} игроков)", status.players);
        return SeedResult::Success;
    }

    // Request join URL then open it
    let connect_url = match api.join_server(server_num).await {
        Ok(u) => u,
        Err(e) => {
            log!("Не удалось получить URL подключения: {e}");
            return SeedResult::Failed;
        }
    };
    log!("Подключаемся к {}...", server_num);
    if let Err(e) = crate::game::open_steam_url(&connect_url) {
        log!("Ошибка открытия steam URL: {e}");
        return SeedResult::Failed;
    }

    // Wait 2 minutes then verify connection
    log!("Ждём 2 минуты перед проверкой подключения...");
    if isleep(120, token).await.is_err() {
        return SeedResult::Cancelled;
    }

    // Up to 3 connection attempts
    let mut connected = false;
    for attempt in 1..=3u8 {
        if token.is_cancelled() {
            return SeedResult::Cancelled;
        }
        match api.check_player(&config.steam_id, server_num).await {
            Ok(true) => {
                connected = true;
                break;
            }
            Ok(false) => {
                log!("Подключение не подтверждено (попытка {attempt}/3)");
                if attempt < 3 {
                    if let Ok(url) = api.join_server(server_num).await {
                        let _ = crate::game::open_steam_url(&url);
                    }
                    if isleep(120, token).await.is_err() {
                        return SeedResult::Cancelled;
                    }
                }
            }
            Err(e) => log!("Ошибка проверки подключения: {e}"),
        }
    }

    if !connected {
        log!("Не удалось подтвердить подключение к серверу {server_num}");
        return SeedResult::Failed;
    }

    // Mark as actively seeding this server; cleared automatically on return.
    seeding_server.store(true, std::sync::atomic::Ordering::Release);
    let _guard = ActiveServerGuard(seeding_server);

    // Auto-create squad: only when game window is interactive.
    // Skipped in eco+nullrhi (render_toggle=true) since there is no visible window.
    let can_interact = !config.eco_mode || !config.render_toggle;
    if config.auto_create_squad && can_interact {
        crate::input::create_ingame_squad(token, log).await;
    }

    // Monitor loop
    log!(
        "Мониторинг сервера {server_num} до {} игроков...",
        config.desired_players
    );
    loop {
        if isleep(config.checkup_interval, token).await.is_err() {
            return SeedResult::Cancelled;
        }

        if config.time_limit_enabled {
            let now = chrono::Local::now();
            let now_mins = now.hour() * 60 + now.minute();
            let limit_mins = config.time_limit_hour * 60 + config.time_limit_minute;
            if now_mins >= limit_mins {
                log!("Период сидинга завершён ({:02}:{:02})", config.time_limit_hour, config.time_limit_minute);
                return SeedResult::PeriodEnd;
            }
        }

        if config.night_mode_enabled && is_in_night_window(
            config.night_start_hour, config.night_start_minute,
            config.night_end_hour, config.night_end_minute,
        ) {
            return SeedResult::NightMode;
        }

        match api.check_player(&config.steam_id, server_num).await {
            Ok(false) => {
                log!("Потеряно соединение с сервером!");
                return SeedResult::Restart;
            }
            Err(e) => log!("Ошибка проверки: {e}"),
            Ok(true) => {}
        }

        match api.get_server(server_num).await {
            Ok(s) => {
                log!(
                    "Сервер {server_num}: {}/{}",
                    s.players,
                    config.desired_players
                );
                if s.players >= config.desired_players {
                    log!(
                        "Сервер {server_num} достиг {} игроков!",
                        config.desired_players
                    );
                    return SeedResult::Success;
                }
            }
            Err(e) => log!("Ошибка статуса: {e}"),
        }
    }
}

