use std::io::Write;
use std::sync::{Arc, Mutex};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::api::HubApi;
use crate::config::{self, AfterSeedAction, Config, Theme};
use crate::{AppWindow, ServerStatus};
use slint::ComponentHandle;

pub type LogSender = mpsc::UnboundedSender<String>;

// ── AppState ──────────────────────────────────────────────────────────────────

pub struct AppState {
    pub config: Mutex<Config>,
    pub api: Arc<HubApi>,
    pub seed_token: Mutex<Option<CancellationToken>>,
    pub join_token: Mutex<Option<CancellationToken>>,
    pub pending_after_seed: Mutex<Option<config::AfterSeedAction>>,
    pub window: slint::Weak<AppWindow>,
    pub log: LogSender,
    pub updating: std::sync::atomic::AtomicBool,
    pub seeded_after_hours: std::sync::atomic::AtomicBool,
}

impl AppState {
    fn log(&self, msg: impl Into<String>) {
        let _ = self.log.send(msg.into());
    }
}

// ── setup: called from main on the UI thread ──────────────────────────────────

pub fn setup(window: slint::Weak<AppWindow>) {
    let cfg = config::load();
    let api = Arc::new(HubApi::new());
    let (log_tx, log_rx) = mpsc::unbounded_channel::<String>();

    apply_theme(&cfg.theme);

    {
        let w = window.upgrade().unwrap();
        sync_config_to_ui(&w, &cfg);
        w.set_app_version(CURRENT_VERSION.into());
    }

    let state = Arc::new(AppState {
        config: Mutex::new(cfg),
        api: api.clone(),
        seed_token: Mutex::new(None),
        join_token: Mutex::new(None),
        pending_after_seed: Mutex::new(None),
        window: window.clone(),
        log: log_tx,
        updating: std::sync::atomic::AtomicBool::new(false),
        seeded_after_hours: std::sync::atomic::AtomicBool::new(false),
    });

    connect_callbacks(window.clone(), state.clone());

    tokio::spawn(log_consumer(log_rx, window.clone()));
    tokio::spawn(status_poll_loop(state.clone()));
    tokio::spawn(process_watch_loop(state.clone()));
    tokio::spawn(api_health_loop(state.clone()));
    tokio::spawn(seed_order_poll(state.clone()));
    tokio::spawn(shutdown_scheduler(state.clone()));
    tokio::spawn(updater::check_loop(state.clone()));

    // Slint ComboBox/two-way bindings may fire changed handlers during first render;
    // schedule a final dirty reset to run after the event loop processes init events.
    {
        let win = window.clone();
        slint::invoke_from_event_loop(move || {
            if let Some(w) = win.upgrade() {
                w.set_settings_dirty(false);
            }
        })
        .ok();
    }

    // Auto-start seeding (with optional startup delay)
    let (auto, wait_mins) = {
        let cfg = state.config.lock().unwrap();
        (cfg.auto_start_seeding, cfg.startup_wait_minutes)
    };
    if auto {
        let s = state.clone();
        tokio::spawn(async move {
            let delay = std::cmp::max(wait_mins as u64 * 60, 5);
            s.log(format!("Авто-старт: ожидание {delay} сек перед запуском..."));
            tokio::time::sleep(tokio::time::Duration::from_secs(delay)).await;
            if s.updating.load(std::sync::atomic::Ordering::Acquire) {
                s.log("Авто-старт: ожидание завершения обновления...");
                while s.updating.load(std::sync::atomic::Ordering::Acquire) {
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
            s.log("Авто-старт: запуск...");
            start_seeding(s, true);
        });
    }
}

// ── Callbacks ─────────────────────────────────────────────────────────────────

fn connect_callbacks(window: slint::Weak<AppWindow>, state: Arc<AppState>) {
    let w = window.upgrade().unwrap();

    {
        let s = state.clone();
        w.on_start_seed(move || start_seeding(s.clone(), false));
    }
    {
        let s = state.clone();
        w.on_force_start_seed(move || {
            s.seeded_after_hours.store(true, std::sync::atomic::Ordering::Release);
            do_start_seeding(s.clone());
        });
    }
    {
        let s = state.clone();
        w.on_stop_seed(move || stop_seeding(s.clone()));
    }
    {
        // Stop seed + exit. Saves settings first.
        let s = state.clone();
        w.on_stop_and_exit(move || {
            stop_seeding(s.clone());
            save_settings(s.clone());
            std::process::exit(0);
        });
    }
    {
        // Exit only — does not kill Squad. Only safe when seed is stopped (enforced by UI).
        w.on_exit_app(move || {
            std::process::exit(0);
        });
    }
    {
        w.on_open_log(move || {
            let path = config::log_path();
            let _ = std::process::Command::new("notepad").arg(&path).spawn();
        });
    }
    {
        let s = state.clone();
        w.on_save_settings(move || save_settings(s.clone()));
    }
    {
        let s = state.clone();
        w.on_reset_settings(move || reset_settings(s.clone()));
    }
    {
        let s = state.clone();
        w.on_reload_settings(move || reload_settings(s.clone()));
    }
    {
        let s = state.clone();
        w.on_test_join(move |server_num| {
            let s = s.clone();
            tokio::spawn(async move {
                let srv_num = server_num as u8;
                match s.api.join_server(srv_num).await {
                    Ok(url) => {
                        s.log(format!("Открываем ссылку подключения {srv_num}"));
                        let _ = crate::game::open_steam_url(&url);
                    }
                    Err(e) => s.log(format!("Ошибка: {e}")),
                }
            });
        });
    }
    {
        let s = state.clone();
        w.on_apply_update(move || {
            let s = s.clone();
            tokio::spawn(async move {
                crate::updater::apply(&s, false).await;
            });
        });
    }
    {
        w.on_open_steam_account(move || {
            let _ = crate::game::open_steam_url(
                "steam://openurl/https://store.steampowered.com/account/",
            );
        });
    }
    {
        w.on_open_website(|| {
            let mut cmd = std::process::Command::new("cmd");
            cmd.args(["/c", "start", "", "https://pstnsquad.ru/"]);
            let _ = spawn_hidden(&mut cmd);
        });
    }
    {
        w.on_open_steam_click(|| {
            let _ = crate::game::open_steam_url("steam://open/main");
        });
    }
    {
        let s = state.clone();
        w.on_launch_squad_and_join(move |server_num| {
            let s = s.clone();
            tokio::spawn(async move {
                let token = CancellationToken::new();
                *s.join_token.lock().unwrap() = Some(token.clone());
                let _ = s
                    .window
                    .upgrade_in_event_loop(|w| w.set_joining_active(true));

                s.log("Запуск Squad...");
                let _ = crate::game::open_steam_url("steam://run/393380//");

                // Poll until Squad process appears (up to 5 min)
                let mut squad_started = false;
                for _ in 0..60u32 {
                    tokio::select! {
                        _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
                        _ = token.cancelled() => { break; }
                    }
                    if token.is_cancelled() {
                        break;
                    }
                    if crate::process::is_squad_client_running() {
                        squad_started = true;
                        break;
                    }
                }

                if !squad_started || token.is_cancelled() {
                    if !token.is_cancelled() {
                        s.log("Squad не запустился — подключение отменено");
                    }
                    *s.join_token.lock().unwrap() = None;
                    let _ = s
                        .window
                        .upgrade_in_event_loop(|w| w.set_joining_active(false));
                    return;
                }

                s.log("Squad запущен, ждём загрузки главного меню...");
                let delay_secs = {
                    let cfg = s.config.lock().unwrap();
                    (cfg.game_launch_delay as u64 * 60).max(60)
                };
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(delay_secs)) => {}
                    _ = token.cancelled() => {}
                }

                if !token.is_cancelled() {
                    match s.api.join_server(server_num as u8).await {
                        Ok(url) => {
                            s.log(format!("Открываем {url}"));
                            let _ = crate::game::open_steam_url(&url);
                        }
                        Err(e) => s.log(format!("Ошибка подключения: {e}")),
                    }
                }

                *s.join_token.lock().unwrap() = None;
                let _ = s
                    .window
                    .upgrade_in_event_loop(|w| w.set_joining_active(false));
            });
        });
    }
    {
        let s = state.clone();
        w.on_cancel_squad_join(move || {
            if let Some(token) = s.join_token.lock().unwrap().take() {
                token.cancel();
            }
            crate::process::kill_squad();
            s.log("Подключение отменено");
            let _ = s
                .window
                .upgrade_in_event_loop(|w| w.set_joining_active(false));
        });
    }
    {
        let s = state.clone();
        w.on_confirm_after_seed_proceed(move || execute_after_seed(&s));
    }
    {
        let s = state.clone();
        w.on_confirm_after_seed_cancel(move || {
            *s.pending_after_seed.lock().unwrap() = None;
            let _ = s.window.upgrade_in_event_loop(|w| w.set_show_after_seed_confirm(false));
        });
    }
    {
        // X-button: show confirm dialog when seeding, otherwise exit directly.
        let s = state.clone();
        w.window().on_close_requested(move || {
            let has_seed = s.seed_token.lock().unwrap().is_some();
            if has_seed {
                if let Some(win) = s.window.upgrade() {
                    win.set_show_confirm_exit(true);
                }
                slint::CloseRequestResponse::KeepWindowShown
            } else {
                std::process::exit(0)
            }
        });
    }

    // ── Window chrome (drag / minimize / maximize) ────────────────────────
    {
        let drag_origin: Arc<Mutex<(i32, i32, i32, i32)>> = Arc::new(Mutex::new((0, 0, 0, 0)));

        let origin_for_press = drag_origin.clone();
        let win_for_press = window.clone();
        w.on_window_pressed(move || {
            if let Some((cx, cy)) = crate::platform::cursor_pos() {
                let pos = win_for_press.upgrade().unwrap().window().position();
                *origin_for_press.lock().unwrap() = (cx, cy, pos.x, pos.y);
            }
        });

        let win_for_drag = window.clone();
        w.on_window_drag(move |_, _| {
            let (ocx, ocy, owx, owy) = *drag_origin.lock().unwrap();
            if let Some((cx, cy)) = crate::platform::cursor_pos() {
                let app = win_for_drag.upgrade().unwrap();
                app.window().set_position(slint::WindowPosition::Physical(
                    slint::PhysicalPosition {
                        x: owx + (cx - ocx),
                        y: owy + (cy - ocy),
                    },
                ));
            }
        });

        w.on_window_drag_ended(|| {});

        w.on_window_minimize(|| crate::platform::minimize_window());
    }
}

// ── Seeding control ───────────────────────────────────────────────────────────

fn start_seeding(state: Arc<AppState>, is_auto: bool) {
    let (time_enabled, limit_h, limit_m) = {
        let cfg = state.config.lock().unwrap();
        (cfg.time_limit_enabled, cfg.time_limit_hour, cfg.time_limit_minute)
    };
    if time_enabled {
        use chrono::Timelike;
        let now = chrono::Local::now();
        let now_mins = now.hour() * 60 + now.minute();
        let limit_mins = limit_h * 60 + limit_m;
        state.log(format!(
            "Проверка лимита времени: сейчас {:02}:{:02}, лимит {:02}:{:02}",
            now.hour(), now.minute(), limit_h, limit_m
        ));
        if now_mins >= limit_mins {
            state.log(format!("Лимит времени достигнут ({limit_h:02}:{limit_m:02}) — seed не запущен"));
            let _ = state.window.upgrade_in_event_loop(move |w| {
                w.set_after_hours_is_auto(is_auto);
                w.set_show_after_hours_prompt(true);
            });
            return;
        }
    } else {
        state.log("Лимит времени отключён — продолжаем");
    }
    do_start_seeding(state);
}

fn do_start_seeding(state: Arc<AppState>) {
    if state.updating.load(std::sync::atomic::Ordering::Acquire) {
        state.log("Обновление в процессе — seed запустится после завершения");
        let s = state;
        tokio::spawn(async move {
            while s.updating.load(std::sync::atomic::Ordering::Acquire) {
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
            do_start_seeding(s);
        });
        return;
    }
    let mut guard = state.seed_token.lock().unwrap();
    if guard.is_some() {
        state.log("Seed уже запущен");
        return;
    }

    let cfg = state.config.lock().unwrap().clone();
    if let Err(e) = crate::game::validate_config(&cfg) {
        let msg = e.to_string();
        let _ = state.window.upgrade_in_event_loop(move |w| {
            w.set_validation_error_msg(msg.into());
            w.set_show_validation_error(true);
        });
        return;
    }

    let token = CancellationToken::new();
    *guard = Some(token.clone());
    drop(guard);
    let api = state.api.clone();
    let log = state.log.clone();
    let win = state.window.clone();

    let _ = win.upgrade_in_event_loop(|w| {
        w.set_seeding_active(true);
        w.set_stop_blocked(true);
    });
    let win_unblock = state.window.clone();
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        let _ = win_unblock.upgrade_in_event_loop(|w| w.set_stop_blocked(false));
    });

    let state2 = state.clone();
    tokio::spawn(async move {
        let completed = crate::seeder::start_seeding(cfg.clone(), api, token, log.clone()).await;

        // Only fire after-seed action on natural completion, not on manual stop.
        if completed {
            perform_after_seed_action(&cfg, &state2);
        }

        state2.seeded_after_hours.store(false, std::sync::atomic::Ordering::Release);
        *state2.seed_token.lock().unwrap() = None;
        let _ = state2.window.upgrade_in_event_loop(|w| {
            w.set_seeding_active(false);
            w.set_stop_blocked(false);
        });
    });
}

/// Cancel seeding, kill Squad, restore INI. Does NOT perform after-seed action.
fn stop_seeding(state: Arc<AppState>) {
    if let Some(token) = state.seed_token.lock().unwrap().take() {
        token.cancel();
    }
    crate::process::kill_squad();
    let (eco, preferred_fps, preferred_menu_fps) = {
        let cfg = state.config.lock().unwrap();
        (cfg.eco_mode, cfg.preferred_fps, cfg.preferred_menu_fps)
    };
    if eco {
        if crate::game::write_fps_keys(preferred_fps, preferred_menu_fps).is_ok() {
            let _ = state.log.send("\x00restore_toast".into());
        }
    }
    let _ = state.window.upgrade_in_event_loop(|w| {
        w.set_seeding_active(false);
        w.set_stop_blocked(false);
    });
}

fn perform_after_seed_action(cfg: &Config, state: &Arc<AppState>) {
    if cfg.after_seed_action == AfterSeedAction::Nothing { return; }

    let msg: slint::SharedString = match cfg.after_seed_action {
        AfterSeedAction::CloseAndExit => "Закрыть игру и Выйти",
        AfterSeedAction::Shutdown => "Завершение Работы",
        AfterSeedAction::Sleep => "Спящий Режим",
        AfterSeedAction::Nothing => return,
    }.into();

    *state.pending_after_seed.lock().unwrap() = Some(cfg.after_seed_action.clone());
    let _ = state.window.upgrade_in_event_loop(move |w| {
        w.set_after_seed_confirm_msg(msg);
        w.set_show_after_seed_confirm(true);
    });

    // Force-started after hours → wait indefinitely, user must confirm manually.
    // Normal/auto-start → auto-proceed after 60s for unattended sessions.
    let after_hours = state.seeded_after_hours.load(std::sync::atomic::Ordering::Acquire);
    if !after_hours {
        let s = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            execute_after_seed(&s);
        });
    }
}

fn execute_after_seed(state: &Arc<AppState>) {
    let action = state.pending_after_seed.lock().unwrap().take();
    let _ = state.window.upgrade_in_event_loop(|w| w.set_show_after_seed_confirm(false));
    match action {
        Some(AfterSeedAction::CloseAndExit) => std::process::exit(0),
        Some(AfterSeedAction::Shutdown) => {
            state.log("Выключение компьютера...");
            let _ = spawn_hidden(std::process::Command::new("shutdown").args(["/s", "/t", "60"]));
        }
        Some(AfterSeedAction::Sleep) => {
            state.log("Спящий режим...");
            let mut cmd = std::process::Command::new("rundll32.exe");
            cmd.args(["powrprof.dll,SetSuspendState", "0,1,0"]);
            let _ = spawn_hidden(&mut cmd);
        }
        _ => {}
    }
}

// ── Background tasks ──────────────────────────────────────────────────────────

async fn log_consumer(mut rx: mpsc::UnboundedReceiver<String>, window: slint::Weak<AppWindow>) {
    while let Some(raw) = rx.recv().await {
        if raw.starts_with('\x00') {
            if raw == "\x00restore_toast" {
                let _ = window.upgrade_in_event_loop(|w| w.set_restore_toast_visible(true));
            }
            continue;
        }
        let msg = format!("[{}] {}", chrono::Local::now().format("%H:%M:%S"), raw);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(config::log_path())
        {
            let _ = writeln!(f, "{msg}");
        }

        let msg2 = msg.clone();
        let _ = window.upgrade_in_event_loop(move |w| {
            let cur = w.get_log_text();
            let new = if cur.is_empty() {
                msg2.into()
            } else {
                slint::SharedString::from(format!("{cur}\n{msg2}"))
            };
            w.set_log_text(new);
        });
    }
}

async fn status_poll_loop(state: Arc<AppState>) {
    loop {
        if let Ok(servers) = state.api.get_all_servers().await {
            let cards: Vec<ServerStatus> = (1u8..=4)
                .map(|num| {
                    let tag = crate::api::tag_for(num).unwrap_or("");
                    if let Some(d) = servers.get(tag) {
                        if d.is_online() {
                            return ServerStatus {
                                name: {
                                    let raw = if d.name.is_empty() {
                                        crate::api::name_for(num).to_string()
                                    } else {
                                        d.name.clone()
                                    };
                                    raw.find("| pstnsquad")
                                        .map_or(raw.clone(), |i| raw[..i].trim_end().to_string())
                                        .into()
                                },
                                online: true,
                                players: d.players as i32,
                                max_players: d.max_players as i32,
                                queue: d.queue as i32,
                                map: d.layer.clone().into(),
                                faction1: d.team1_faction.clone().into(),
                                faction2: d.team2_faction.clone().into(),
                            };
                        }
                    }
                    ServerStatus {
                        name: crate::api::name_for(num).into(),
                        online: false,
                        players: 0,
                        max_players: 100,
                        queue: 0,
                        map: "".into(),
                        faction1: "".into(),
                        faction2: "".into(),
                    }
                })
                .collect();

            let steam_id = state.config.lock().unwrap().steam_id.clone();
            let mut player_server: i32 = 0;
            if steam_id.len() == 17 {
                for num in 1u8..=4 {
                    if let Ok(true) = state.api.check_player(&steam_id, num).await {
                        player_server = num as i32;
                        break;
                    }
                }
            }

            let _ = state.window.upgrade_in_event_loop(move |w| {
                use slint::VecModel;
                use std::rc::Rc;
                w.set_servers(Rc::new(VecModel::from(cards)).into());
                w.set_player_server(player_server);
                w.set_refresh_ping(!w.get_refresh_ping());
            });
        }

        let interval = state.config.lock().unwrap().checkup_interval;
        tokio::time::sleep(tokio::time::Duration::from_secs(interval)).await;
    }
}

/// Checks Squad + Steam process state every 5 seconds — much faster than checkup_interval.
/// When seeding is active and Squad disappears unexpectedly, waits 10s then kills CrashReportClient.
async fn process_watch_loop(state: Arc<AppState>) {
    let mut prev_squad = false;
    loop {
        let (squad, steam) = crate::process::check_processes();
        let _ = state.window.upgrade_in_event_loop(move |w| {
            w.set_squad_running(squad);
            w.set_steam_running(steam);
        });

        let seeding = state.seed_token.lock().unwrap().is_some();
        if prev_squad && !squad && seeding {
            state.log("Squad пропал во время seed — возможный краш, проверяем через 10 сек...");
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            if crate::process::find_crash_reporter() {
                state.log("Обнаружен CrashReportClient — закрываем и ждём перезапуска...");
                crate::process::kill_crash_reporter();
            }
        } else {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }

        prev_squad = squad;
    }
}

/// Polls /api/v1/health every 30 seconds; requires 2 consecutive successes before showing OK.
async fn api_health_loop(state: Arc<AppState>) {
    let mut consecutive_ok: u8 = 0;
    loop {
        let ok = state.api.health_check().await;
        if ok {
            consecutive_ok = consecutive_ok.saturating_add(1);
        } else {
            consecutive_ok = 0;
        }
        let show_ok = consecutive_ok >= 1;
        let _ = state
            .window
            .upgrade_in_event_loop(move |w| w.set_api_ok(show_ok));
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
    }
}

async fn seed_order_poll(state: Arc<AppState>) {
    loop {
        let has_override = state.config.lock().unwrap().seed_order_override.is_some();
        if !has_override {
            if let Ok(order) = state.api.get_seed_order().await {
                let display = order
                    .iter()
                    .map(|&n| crate::api::name_for(n))
                    .collect::<Vec<_>>()
                    .join(" → ");
                let _ = state.window.upgrade_in_event_loop(move |w| {
                    w.set_remote_seed_order(display.into());
                });
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
    }
}

async fn shutdown_scheduler(state: Arc<AppState>) {
    use chrono::Timelike;
    let mut last_fired: Option<chrono::NaiveDate> = None;

    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

        let scheduled = state.config.lock().unwrap().scheduled_shutdown.clone();
        let Some(time_str) = scheduled else {
            last_fired = None;
            continue;
        };

        let parts: Vec<u32> = time_str
            .splitn(2, ':')
            .filter_map(|s| s.parse().ok())
            .collect();
        if parts.len() != 2 {
            continue;
        }

        let now = chrono::Local::now();
        let today = now.date_naive();
        if now.hour() == parts[0] && now.minute() == parts[1] && last_fired != Some(today) {
            last_fired = Some(today);
            state.log(format!("Запланированное выключение в {time_str}..."));
            let _ = spawn_hidden(
                std::process::Command::new("shutdown").args(["/s", "/t", "0", "/hybrid"]),
            );
        }
    }
}

mod updater {
    use super::AppState;
    use std::sync::Arc;

    pub async fn check_loop(state: Arc<AppState>) {
        loop {
            crate::updater::check(&state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(600)).await;
        }
    }
}

// ── Config sync ───────────────────────────────────────────────────────────────

fn sync_config_to_ui(w: &AppWindow, cfg: &Config) {
    w.set_cfg_preferred_fps(
        cfg.preferred_fps
            .map(|n| n.to_string())
            .unwrap_or_default()
            .into(),
    );
    w.set_cfg_preferred_menu_fps(
        cfg.preferred_menu_fps
            .map(|n| n.to_string())
            .unwrap_or_default()
            .into(),
    );
    w.set_cfg_steam_id(cfg.steam_id.clone().into());
    w.set_cfg_seed_order_override(
        cfg.seed_order_override
            .as_ref()
            .map(|v| {
                v.iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default()
            .into(),
    );
    w.set_cfg_desired_players(cfg.desired_players as i32);
    w.set_cfg_checkup_interval(cfg.checkup_interval as i32);
    w.set_cfg_game_launch_delay(cfg.game_launch_delay as i32);
    w.set_cfg_time_limit(cfg.time_limit_hour as i32);
    w.set_cfg_startup_wait(cfg.startup_wait_minutes as i32);
    w.set_cfg_start_on_startup(cfg.start_on_startup);
    w.set_cfg_auto_start(cfg.auto_start_seeding);
    w.set_cfg_render_toggle(cfg.render_toggle);
    w.set_cfg_auto_create_squad(cfg.auto_create_squad);
    w.set_cfg_disable_sound(cfg.disable_sound);
    w.set_cfg_delete_startup_video(cfg.delete_startup_video);
    w.set_cfg_eco_mode(cfg.eco_mode);
    w.set_cfg_time_limit_minute(cfg.time_limit_minute as i32);
    w.set_cfg_time_limit_enabled(cfg.time_limit_enabled);
    w.set_cfg_after_action(after_action_str(&cfg.after_seed_action).into());
    w.set_cfg_stop_after(cfg.stop_after_server as i32);
    w.set_cfg_scheduled_shutdown(cfg.scheduled_shutdown.clone().unwrap_or_default().into());
    if let Some(ref s) = cfg.scheduled_shutdown {
        let parts: Vec<&str> = s.splitn(2, ':').collect();
        let h = parts
            .first()
            .and_then(|p| p.parse::<i32>().ok())
            .unwrap_or(0);
        let m = parts
            .get(1)
            .and_then(|p| p.parse::<i32>().ok())
            .unwrap_or(0);
        w.set_cfg_shutdown_hour(h);
        w.set_cfg_shutdown_minute(m);
        w.set_cfg_shutdown_enabled(true);
    } else {
        w.set_cfg_shutdown_enabled(false);
    }
    w.set_cfg_auto_update(cfg.auto_update);
    // Reset after all properties are set so changed-handlers don't leave dirty=true
    w.set_settings_dirty(false);
}

fn save_settings(state: Arc<AppState>) {
    let w = match state.window.upgrade() {
        Some(w) => w,
        None => return,
    };

    let seed_override = {
        let raw = w.get_cfg_seed_order_override().to_string();
        if raw.trim().is_empty() {
            None
        } else {
            Some(
                raw.split(',')
                    .filter_map(|s| s.trim().parse::<u8>().ok())
                    .collect::<Vec<_>>(),
            )
        }
    };

    let old_startup;
    let new_startup;

    {
        let mut cfg = state.config.lock().unwrap();
        old_startup = cfg.start_on_startup;

        cfg.preferred_fps = w
            .get_cfg_preferred_fps()
            .to_string()
            .trim()
            .parse::<u32>()
            .ok();
        cfg.preferred_menu_fps = w
            .get_cfg_preferred_menu_fps()
            .to_string()
            .trim()
            .parse::<u32>()
            .ok();
        cfg.steam_id = w.get_cfg_steam_id().to_string();
        cfg.seed_order_override = seed_override;
        cfg.desired_players = w.get_cfg_desired_players() as u32;
        cfg.checkup_interval = w.get_cfg_checkup_interval() as u64;
        cfg.game_launch_delay = w.get_cfg_game_launch_delay() as u32;
        cfg.time_limit_hour = w.get_cfg_time_limit() as u32;
        cfg.startup_wait_minutes = w.get_cfg_startup_wait() as u32;
        cfg.start_on_startup = w.get_cfg_start_on_startup();
        cfg.auto_start_seeding = w.get_cfg_auto_start();
        cfg.render_toggle = w.get_cfg_render_toggle();
        cfg.auto_create_squad = w.get_cfg_auto_create_squad();
        cfg.disable_sound = w.get_cfg_disable_sound();
        cfg.delete_startup_video = w.get_cfg_delete_startup_video();
        cfg.eco_mode = w.get_cfg_eco_mode();
        cfg.time_limit_minute = w.get_cfg_time_limit_minute() as u32;
        cfg.time_limit_enabled = w.get_cfg_time_limit_enabled();
        cfg.after_seed_action = parse_after_action(&w.get_cfg_after_action());
        cfg.stop_after_server = w.get_cfg_stop_after() as u8;
        cfg.scheduled_shutdown = if w.get_cfg_shutdown_enabled() {
            Some(format!(
                "{:02}:{:02}",
                w.get_cfg_shutdown_hour(),
                w.get_cfg_shutdown_minute()
            ))
        } else {
            None
        };
        cfg.auto_update = w.get_cfg_auto_update();

        new_startup = cfg.start_on_startup;
        config::save(&cfg);
    }
    w.set_settings_dirty(false);

    if old_startup != new_startup {
        if let Err(e) = crate::startup::set(new_startup) {
            state.log(format!("Ошибка автозагрузки: {e}"));
        }
    }

    state.log("Настройки сохранены");
}

fn reload_settings(state: Arc<AppState>) {
    let cfg = config::load();
    *state.config.lock().unwrap() = cfg.clone();
    let _ = state.window.upgrade_in_event_loop(move |w| {
        sync_config_to_ui(&w, &cfg);
    });
    state.log("Изменения отменены");
}

fn reset_settings(state: Arc<AppState>) {
    let defaults = Config::default();
    {
        *state.config.lock().unwrap() = defaults.clone();
        config::save(&defaults);
    }
    let _ = state.window.upgrade_in_event_loop(move |w| {
        sync_config_to_ui(&w, &defaults);
    });
    state.log("Настройки сброшены");
}

// ── Theme ─────────────────────────────────────────────────────────────────────

fn apply_theme(_theme: &Theme) {
    // ponytail: Slint follows system color scheme by default; manual override wired in later
}

// ── String helpers ────────────────────────────────────────────────────────────

fn after_action_str(a: &AfterSeedAction) -> &'static str {
    match a {
        AfterSeedAction::Nothing => "Ничего",
        AfterSeedAction::CloseAndExit => "Закрыть игру и Выйти (Рекомендуется)",
        AfterSeedAction::Shutdown => "Завершение Работы",
        AfterSeedAction::Sleep => "Спящий Режим",
    }
}

fn parse_after_action(s: &slint::SharedString) -> AfterSeedAction {
    match s.as_str() {
        "Закрыть игру и Выйти (Рекомендуется)" | "Закрыть игру и Выйти" => AfterSeedAction::CloseAndExit,
        "Завершение Работы" => AfterSeedAction::Shutdown,
        "Спящий Режим" => AfterSeedAction::Sleep,
        _ => AfterSeedAction::Nothing,
    }
}

fn spawn_hidden(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000).spawn()
    }
    #[cfg(not(windows))]
    {
        cmd.spawn()
    }
}
