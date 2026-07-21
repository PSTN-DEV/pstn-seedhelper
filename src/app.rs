use std::io::Write;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::api::HubApi;
use crate::config::{self, AfterSeedAction, Config, Theme};
use slint::ComponentHandle;
use crate::{AppWindow, ServerStatus};

pub type LogSender = mpsc::UnboundedSender<String>;

// ── AppState ──────────────────────────────────────────────────────────────────

pub struct AppState {
    pub config: Mutex<Config>,
    pub api: Arc<HubApi>,
    pub seed_token: Mutex<Option<CancellationToken>>,
    pub join_token: Mutex<Option<CancellationToken>>,
    pub window: slint::Weak<AppWindow>,
    pub log: LogSender,
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
    }

    let state = Arc::new(AppState {
        config: Mutex::new(cfg),
        api: api.clone(),
        seed_token: Mutex::new(None),
        join_token: Mutex::new(None),
        window: window.clone(),
        log: log_tx,
    });

    connect_callbacks(window.clone(), state.clone());

    tokio::spawn(log_consumer(log_rx, window.clone()));
    tokio::spawn(status_poll_loop(state.clone()));
    tokio::spawn(process_watch_loop(state.clone()));
    tokio::spawn(api_health_loop(state.clone()));
    tokio::spawn(seed_order_poll(state.clone()));
    tokio::spawn(shutdown_scheduler(state.clone()));
    tokio::spawn(updater::check(state.clone()));

    // Auto-start seeding (with optional startup delay)
    let (auto, wait_mins) = {
        let cfg = state.config.lock().unwrap();
        (cfg.auto_start_seeding, cfg.startup_wait_minutes)
    };
    if auto {
        let s = state.clone();
        tokio::spawn(async move {
            if wait_mins > 0 {
                tokio::time::sleep(tokio::time::Duration::from_secs(wait_mins as u64 * 60)).await;
            }
            start_seeding(s);
        });
    }
}

// ── Callbacks ─────────────────────────────────────────────────────────────────

fn connect_callbacks(window: slint::Weak<AppWindow>, state: Arc<AppState>) {
    let w = window.upgrade().unwrap();

    {
        let s = state.clone();
        w.on_start_seed(move || start_seeding(s.clone()));
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
                        s.log(format!("Открываем {url}"));
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
            tokio::spawn(async move { crate::updater::apply(&s).await; });
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
            let _ = std::process::Command::new("cmd")
                .args(["/c", "start", "", "https://pstnsquad.ru/"])
                .spawn();
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
                let _ = s.window.upgrade_in_event_loop(|w| w.set_joining_active(true));

                s.log("Запуск Squad...");
                let _ = crate::game::open_steam_url("steam://run/393380//");

                // Poll until Squad process appears (up to 5 min)
                let mut squad_started = false;
                for _ in 0..60u32 {
                    tokio::select! {
                        _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
                        _ = token.cancelled() => { break; }
                    }
                    if token.is_cancelled() { break; }
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
                    let _ = s.window.upgrade_in_event_loop(|w| w.set_joining_active(false));
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
                let _ = s.window.upgrade_in_event_loop(|w| w.set_joining_active(false));
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
            let _ = s.window.upgrade_in_event_loop(|w| w.set_joining_active(false));
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
        let drag_origin: Arc<Mutex<(i32, i32, i32, i32)>> =
            Arc::new(Mutex::new((0, 0, 0, 0)));

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

        w.on_window_minimize(|| crate::platform::minimize_window());
    }
}

// ── Seeding control ───────────────────────────────────────────────────────────

fn start_seeding(state: Arc<AppState>) {
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

    let _ = win.upgrade_in_event_loop(|w| { w.set_seeding_active(true); w.set_stop_blocked(true); });
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

        *state2.seed_token.lock().unwrap() = None;
        let _ = state2.window.upgrade_in_event_loop(|w| { w.set_seeding_active(false); w.set_stop_blocked(false); });
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
    state.log("Seed остановлен");
    let _ = state.window.upgrade_in_event_loop(|w| { w.set_seeding_active(false); w.set_stop_blocked(false); });
}

fn perform_after_seed_action(cfg: &Config, state: &Arc<AppState>) {
    match cfg.after_seed_action {
        AfterSeedAction::Nothing => {}
        AfterSeedAction::CloseAndExit => std::process::exit(0),
        AfterSeedAction::Shutdown => {
            state.log("Выключение компьютера...");
            let _ = std::process::Command::new("shutdown").args(["/s", "/t", "60"]).spawn();
        }
        AfterSeedAction::Sleep => {
            state.log("Спящий режим...");
            let _ = std::process::Command::new("rundll32.exe")
                .args(["powrprof.dll,SetSuspendState", "0,1,0"])
                .spawn();
        }
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
                                name: crate::api::clean_name(crate::api::name_for(num)).into(),
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
                        name: crate::api::clean_name(crate::api::name_for(num)).into(),
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

            let _ = state.window.upgrade_in_event_loop(move |w| {
                use slint::VecModel;
                use std::rc::Rc;
                w.set_servers(Rc::new(VecModel::from(cards)).into());
                w.set_refresh_ping(!w.get_refresh_ping());
            });
        }

        let interval = state.config.lock().unwrap().checkup_interval;
        tokio::time::sleep(tokio::time::Duration::from_secs(interval)).await;
    }
}

/// Checks Squad + Steam process state every 5 seconds — much faster than checkup_interval.
async fn process_watch_loop(state: Arc<AppState>) {
    loop {
        let (squad, steam) = crate::process::check_processes();
        let _ = state.window.upgrade_in_event_loop(move |w| {
            w.set_squad_running(squad);
            w.set_steam_running(steam);
        });
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}

/// Polls /api/v1/health every 30 seconds for the status bar indicator.
async fn api_health_loop(state: Arc<AppState>) {
    loop {
        let ok = state.api.health_check().await;
        let _ = state.window.upgrade_in_event_loop(move |w| w.set_api_ok(ok));
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
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;

        let scheduled = state.config.lock().unwrap().scheduled_shutdown.clone();
        let Some(time_str) = scheduled else { last_fired = None; continue; };

        let parts: Vec<u32> = time_str.splitn(2, ':')
            .filter_map(|s| s.parse().ok())
            .collect();
        if parts.len() != 2 { continue; }

        let now = chrono::Local::now();
        let today = now.date_naive();
        if now.hour() == parts[0] && now.minute() == parts[1] && last_fired != Some(today) {
            last_fired = Some(today);
            state.log(format!("Запланированное выключение в {time_str}..."));
            let _ = std::process::Command::new("shutdown").args(["/s", "/t", "0", "/hybrid"]).spawn();
        }
    }
}

mod updater {
    use std::sync::Arc;
    use super::AppState;

    pub async fn check(state: Arc<AppState>) {
        crate::updater::check(&state).await;
    }
}

// ── Config sync ───────────────────────────────────────────────────────────────

fn sync_config_to_ui(w: &AppWindow, cfg: &Config) {
    w.set_cfg_preferred_fps(cfg.preferred_fps.map(|n| n.to_string()).unwrap_or_default().into());
    w.set_cfg_preferred_menu_fps(cfg.preferred_menu_fps.map(|n| n.to_string()).unwrap_or_default().into());
    w.set_cfg_steam_id(cfg.steam_id.clone().into());
    w.set_cfg_seed_order_override(
        cfg.seed_order_override
            .as_ref()
            .map(|v| v.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(","))
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
        let h = parts.first().and_then(|p| p.parse::<i32>().ok()).unwrap_or(0);
        let m = parts.get(1).and_then(|p| p.parse::<i32>().ok()).unwrap_or(0);
        w.set_cfg_shutdown_hour(h);
        w.set_cfg_shutdown_minute(m);
        w.set_cfg_shutdown_enabled(true);
    } else {
        w.set_cfg_shutdown_enabled(false);
    }
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

        cfg.preferred_fps        = w.get_cfg_preferred_fps().to_string().trim().parse::<u32>().ok();
        cfg.preferred_menu_fps   = w.get_cfg_preferred_menu_fps().to_string().trim().parse::<u32>().ok();
        cfg.steam_id             = w.get_cfg_steam_id().to_string();
        cfg.seed_order_override  = seed_override;
        cfg.desired_players      = w.get_cfg_desired_players() as u32;
        cfg.checkup_interval     = w.get_cfg_checkup_interval() as u64;
        cfg.game_launch_delay    = w.get_cfg_game_launch_delay() as u32;
        cfg.time_limit_hour      = w.get_cfg_time_limit() as u32;
        cfg.startup_wait_minutes = w.get_cfg_startup_wait() as u32;
        cfg.start_on_startup     = w.get_cfg_start_on_startup();
        cfg.auto_start_seeding   = w.get_cfg_auto_start();
        cfg.render_toggle        = w.get_cfg_render_toggle();
        cfg.auto_create_squad    = w.get_cfg_auto_create_squad();
        cfg.disable_sound        = w.get_cfg_disable_sound();
        cfg.delete_startup_video = w.get_cfg_delete_startup_video();
        cfg.eco_mode             = w.get_cfg_eco_mode();
        cfg.time_limit_minute    = w.get_cfg_time_limit_minute() as u32;
        cfg.time_limit_enabled   = w.get_cfg_time_limit_enabled();
        cfg.after_seed_action    = parse_after_action(&w.get_cfg_after_action());
        cfg.stop_after_server    = w.get_cfg_stop_after() as u8;
        cfg.scheduled_shutdown   = if w.get_cfg_shutdown_enabled() {
            Some(format!("{:02}:{:02}", w.get_cfg_shutdown_hour(), w.get_cfg_shutdown_minute()))
        } else {
            None
        };

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
        AfterSeedAction::Nothing      => "Ничего",
        AfterSeedAction::CloseAndExit => "Закрыть игру и Выйти",
        AfterSeedAction::Shutdown     => "Завершение Работы",
        AfterSeedAction::Sleep        => "Спящий Режим",
    }
}

fn parse_after_action(s: &slint::SharedString) -> AfterSeedAction {
    match s.as_str() {
        "Закрыть игру и Выйти" => AfterSeedAction::CloseAndExit,
        "Завершение Работы"    => AfterSeedAction::Shutdown,
        "Спящий Режим"         => AfterSeedAction::Sleep,
        _                      => AfterSeedAction::Nothing,
    }
}
