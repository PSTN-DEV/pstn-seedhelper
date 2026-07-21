use anyhow::Result;
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
async fn do_launch(config: &Config, token: &CancellationToken, log: &LogSender) -> anyhow::Result<()> {
    if config.delete_startup_video {
        crate::game::remove_welcome_video(config, log);
    }
    if config.eco_mode {
        crate::game::launch_game_eco(config, token, log).await?;
    } else {
        crate::game::launch_game_steam(config, token, log).await?;
    }
    Ok(())
}

/// Returns true = completed naturally (after-seed action should fire),
///         false = cancelled by Stop button (skip after-seed action).
pub async fn start_seeding(
    config: Config,
    api: Arc<HubApi>,
    token: CancellationToken,
    log: LogSender,
) -> bool {
    macro_rules! log {
        ($($arg:tt)*) => {{
            let msg = format!("[{}] {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), format!($($arg)*));
            let _ = log.send(msg);
        }};
    }

    log!("Начинаем Seed серверов!");

    // 1. Time limit check
    if !check_time_limit(&config) {
        log!("Слишком поздно — лимит времени достигнут");
        return true;
    }

    // 2. Network wait (up to 5 min)
    log!("Проверка доступности сети...");
    let mut ready = false;
    for _ in 0..20 {
        if token.is_cancelled() { return false; }
        if api.ping().await {
            log!("Сеть доступна!");
            ready = true;
            break;
        }
        log!("Сеть недоступна, повтор через 15 сек...");
        if isleep(15, &token).await.is_err() { return false; }
    }
    if !ready {
        log!("Сеть недоступна после 5 минут — seed отменён");
        return true;
    }

    // 3. Validate config
    if let Err(e) = crate::game::validate_config(&config) {
        log!("Ошибка конфига: {e}");
        return true;
    }

    // 4. Resolve seed order
    let order = resolve_seed_order(&config, &api, &log).await;

    // 5. Launch game
    if let Err(e) = do_launch(&config, &token, &log).await {
        log!("Ошибка запуска игры: {e}");
        if config.eco_mode {
            let _ = crate::game::write_fps_keys(config.preferred_fps, config.preferred_menu_fps);
        }
        return !token.is_cancelled();
    }
    if token.is_cancelled() { return false; }

    // 6. Server seed loop
    'servers: for &server_num in &order {
        if token.is_cancelled() { break; }

        loop {
            if token.is_cancelled() { break 'servers; }

            match seed_server(server_num, &config, &api, &token, &log).await {
                SeedResult::Cancelled => break 'servers,
                SeedResult::Restart => {
                    log!("Перезапуск игры для сервера {server_num}...");
                    if let Err(e) = do_launch(&config, &token, &log).await {
                        log!("Ошибка перезапуска: {e}");
                        break 'servers;
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

    // 7. Cleanup
    if config.eco_mode {
        let _ = crate::game::write_fps_keys(config.preferred_fps, config.preferred_menu_fps);
    }

    if !token.is_cancelled() {
        log!("Все сервера обработаны!");
        crate::process::kill_squad();
        true
    } else {
        log!("Seed остановлен.");
        false
    }
}

async fn resolve_seed_order(
    config: &Config,
    api: &HubApi,
    log: &LogSender,
) -> Vec<u8> {
    if let Some(ref local) = config.seed_order_override {
        let _ = log.send(format!("[info] Используем локальный порядок: {local:?}"));
        return local.clone();
    }
    match api.get_seed_order().await {
        Ok(order) => {
            let _ = log.send(format!("[info] Порядок сида с сервера: {order:?}"));
            order
        }
        Err(e) => {
            let _ = log.send(format!("[warn] Не удалось получить порядок с сервера: {e}. Используем 1-2-3-4"));
            vec![1, 2, 3, 4]
        }
    }
}

async fn seed_server(
    server_num: u8,
    config: &Config,
    api: &HubApi,
    token: &CancellationToken,
    log: &LogSender,
) -> SeedResult {
    macro_rules! log {
        ($($arg:tt)*) => {{
            let msg = format!("[{}] {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), format!($($arg)*));
            let _ = log.send(msg);
        }};
    }

    log!("Сид сервера {} ({})", server_num, crate::api::name_for(server_num));

    // Check player count first
    let status = match api.get_server(server_num).await {
        Ok(s) => s,
        Err(e) => { log!("Не удалось получить статус сервера {server_num}: {e}"); return SeedResult::Failed; }
    };

    if status.players >= config.desired_players {
        log!("Сервер {server_num} уже полон ({} игроков)", status.players);
        return SeedResult::Success;
    }

    // Request join URL then open it
    let connect_url = match api.join_server(server_num).await {
        Ok(u) => u,
        Err(e) => { log!("Не удалось получить URL подключения: {e}"); return SeedResult::Failed; }
    };
    log!("Подключаемся к {}...", connect_url);
    if let Err(e) = crate::game::open_steam_url(&connect_url) {
        log!("Ошибка открытия steam URL: {e}");
        return SeedResult::Failed;
    }

    // Wait 2 minutes then verify connection
    log!("Ждём 2 минуты перед проверкой подключения...");
    if isleep(120, token).await.is_err() { return SeedResult::Cancelled; }

    // Up to 3 connection attempts
    let mut connected = false;
    for attempt in 1..=3u8 {
        if token.is_cancelled() { return SeedResult::Cancelled; }
        match api.check_player(&config.steam_id, server_num).await {
            Ok(true) => { connected = true; break; }
            Ok(false) => {
                log!("Подключение не подтверждено (попытка {attempt}/3)");
                if attempt < 3 {
                    if let Ok(url) = api.join_server(server_num).await {
                        let _ = crate::game::open_steam_url(&url);
                    }
                    if isleep(120, token).await.is_err() { return SeedResult::Cancelled; }
                }
            }
            Err(e) => log!("Ошибка проверки подключения: {e}"),
        }
    }

    if !connected {
        log!("Не удалось подтвердить подключение к серверу {server_num}");
        return SeedResult::Failed;
    }

    // Auto-create squad: only when game window is interactive.
    // Skipped in eco+nullrhi (render_toggle=true) since there is no visible window.
    let can_interact = !config.eco_mode || !config.render_toggle;
    if config.auto_create_squad && can_interact {
        crate::input::create_ingame_squad(token, log).await;
    }

    // Monitor loop
    log!("Мониторинг сервера {server_num} до {} игроков...", config.desired_players);
    loop {
        if isleep(config.checkup_interval, token).await.is_err() {
            return SeedResult::Cancelled;
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
                log!("Сервер {server_num}: {}/{}", s.players, config.desired_players);
                if s.players >= config.desired_players {
                    log!("Сервер {server_num} достиг {} игроков!", config.desired_players);
                    return SeedResult::Success;
                }
            }
            Err(e) => log!("Ошибка статуса: {e}"),
        }
    }
}

pub fn check_time_limit(config: &Config) -> bool {
    if !config.time_limit_enabled {
        return true;
    }
    use chrono::Timelike;
    let moscow = chrono::Utc::now().with_timezone(&chrono_tz::Europe::Moscow);
    let now_min = moscow.hour() * 60 + moscow.minute();
    let limit_min = config.time_limit_hour * 60 + config.time_limit_minute;
    now_min < limit_min
}
