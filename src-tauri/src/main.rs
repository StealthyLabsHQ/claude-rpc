#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};

#[derive(Default)]
struct DaemonState {
    running: Arc<Mutex<bool>>,
    error: Mutex<Option<String>>,
    stop: Arc<AtomicBool>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[derive(Default)]
struct TrayMenuState {
    dnd: Mutex<Option<CheckMenuItem<tauri::Wry>>>,
    mode_playing: Mutex<Option<CheckMenuItem<tauri::Wry>>>,
    mode_watching: Mutex<Option<CheckMenuItem<tauri::Wry>>>,
    mode_listening: Mutex<Option<CheckMenuItem<tauri::Wry>>>,
    mode_competing: Mutex<Option<CheckMenuItem<tauri::Wry>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcButton {
    label: String,
    url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeConfig {
    #[serde(default = "default_logo_mode")]
    logo_mode: String,
    #[serde(default)]
    dnd: bool,
    #[serde(default = "default_show_limits")]
    show_limits: bool,
    #[serde(default = "default_show_limits")]
    show_limit_5h: bool,
    #[serde(default = "default_show_limits")]
    show_limit_all: bool,
    #[serde(default = "default_show_limits")]
    show_limit_sonnet: bool,
    #[serde(default = "default_show_limits")]
    show_limit_design: bool,
    #[serde(default = "default_show_limits")]
    show_provider: bool,
    #[serde(default = "default_show_limits")]
    show_effort: bool,
    #[serde(default)]
    verbose: bool,
    #[serde(default)]
    webhook_url: Option<String>,
    #[serde(default = "default_rpc_mode")]
    rpc_mode: String,
    #[serde(default = "default_buttons")]
    buttons: Vec<RpcButton>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            logo_mode: default_logo_mode(),
            dnd: false,
            show_limits: default_show_limits(),
            show_limit_5h: default_show_limits(),
            show_limit_all: default_show_limits(),
            show_limit_sonnet: default_show_limits(),
            show_limit_design: default_show_limits(),
            show_provider: default_show_limits(),
            show_effort: default_show_limits(),
            verbose: false,
            webhook_url: None,
            rpc_mode: default_rpc_mode(),
            buttons: default_buttons(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeStatus {
    summary: String,
    claude_line: String,
    model_line: String,
    limits_line: Option<String>,
    provider_line: String,
    discord_line: String,
    debug_line: Option<String>,
    daemon_running: bool,
    daemon_pid: Option<u32>,
    daemon_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DaemonStatus {
    running: bool,
    pid: Option<u32>,
    error: Option<String>,
}

fn default_logo_mode() -> String {
    "url".into()
}

fn default_show_limits() -> bool {
    true
}

fn default_rpc_mode() -> String {
    "playing".into()
}

fn default_buttons() -> Vec<RpcButton> {
    vec![
        RpcButton {
            label: "Claude".into(),
            url: "https://claude.ai".into(),
        },
        RpcButton {
            label: "GitHub Repo".into(),
            url: "https://github.com/StealthyLabsHQ/claude-rpc".into(),
        },
    ]
}

#[tauri::command]
fn load_config() -> Result<ClaudeConfig, String> {
    read_config()
}

#[tauri::command]
fn save_config(app: tauri::AppHandle, config: ClaudeConfig) -> Result<(), String> {
    let config = normalize_config(config);
    write_config(&config)?;
    sync_tray_menu(&app, &config);
    Ok(())
}

#[tauri::command]
fn load_status(state: tauri::State<'_, DaemonState>) -> Result<ClaudeStatus, String> {
    let value = fs::read_to_string(status_path()?)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(raw.trim_start_matches('\u{feff}')).ok())
        .unwrap_or(Value::Null);
    let daemon = read_daemon_status(&state);

    Ok(ClaudeStatus {
        summary: value
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("Claude RPC")
            .to_string(),
        claude_line: value
            .get("claudeLine")
            .and_then(Value::as_str)
            .unwrap_or("Claude: Off")
            .to_string(),
        model_line: value
            .get("modelLine")
            .and_then(Value::as_str)
            .unwrap_or("Auto-detect")
            .to_string(),
        limits_line: value
            .get("limitsLine")
            .and_then(Value::as_str)
            .map(str::to_string),
        provider_line: value
            .get("providerLine")
            .and_then(Value::as_str)
            .unwrap_or("Provider: Unknown")
            .to_string(),
        discord_line: value
            .get("discordLine")
            .and_then(Value::as_str)
            .unwrap_or("Discord: RPC disabled")
            .to_string(),
        debug_line: value
            .get("debugLine")
            .and_then(Value::as_str)
            .map(str::to_string),
        daemon_running: daemon.running,
        daemon_pid: daemon.pid,
        daemon_error: daemon.error,
    })
}

#[tauri::command]
fn start_daemon(
    app: tauri::AppHandle,
    state: tauri::State<'_, DaemonState>,
) -> Result<DaemonStatus, String> {
    start_daemon_inner(&app, &state);
    Ok(read_daemon_status(&state))
}

#[tauri::command]
fn daemon_status(state: tauri::State<'_, DaemonState>) -> Result<DaemonStatus, String> {
    Ok(read_daemon_status(&state))
}

#[tauri::command]
fn close_settings(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.hide().map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn refresh_limits() -> Result<(), String> {
    std::process::Command::new("explorer.exe")
        .arg("https://claude.ai/settings/usage")
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

fn main() {
    tauri::Builder::default()
        .manage(DaemonState::default())
        .manage(TrayMenuState::default())
        .invoke_handler(tauri::generate_handler![
            load_config,
            save_config,
            load_status,
            start_daemon,
            daemon_status,
            close_settings,
            refresh_limits
        ])
        .setup(|app| {
            keep_window_in_tray(app);
            let handle = app.handle().clone();
            let state = app.state::<DaemonState>();
            start_daemon_inner(&handle, &state);
            create_tray(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run Claude RPC tray");
}

fn keep_window_in_tray(app: &mut tauri::App) {
    if let Some(window) = app.get_webview_window("main") {
        let window_to_hide = window.clone();
        window.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window_to_hide.hide();
            }
        });
    }
}

fn create_tray(app: &mut tauri::App) -> tauri::Result<()> {
    let config = read_config().unwrap_or_default();
    let show_item = MenuItem::with_id(app, "show", "Settings", true, None::<&str>)?;
    let dnd_item =
        CheckMenuItem::with_id(app, "dnd", "Do Not Disturb", true, config.dnd, None::<&str>)?;
    let mode_playing_item = CheckMenuItem::with_id(
        app,
        "mode_playing",
        "Mode: Playing",
        true,
        config.rpc_mode == "playing",
        None::<&str>,
    )?;
    let mode_watching_item = CheckMenuItem::with_id(
        app,
        "mode_watching",
        "Mode: Watching",
        true,
        config.rpc_mode == "watching",
        None::<&str>,
    )?;
    let mode_listening_item = CheckMenuItem::with_id(
        app,
        "mode_listening",
        "Mode: Listening",
        true,
        config.rpc_mode == "listening",
        None::<&str>,
    )?;
    let mode_competing_item = CheckMenuItem::with_id(
        app,
        "mode_competing",
        "Mode: Competing",
        true,
        config.rpc_mode == "competing",
        None::<&str>,
    )?;
    let separator_1 = PredefinedMenuItem::separator(app)?;
    let separator_2 = PredefinedMenuItem::separator(app)?;
    let separator_3 = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &separator_1,
            &dnd_item,
            &separator_2,
            &mode_playing_item,
            &mode_watching_item,
            &mode_listening_item,
            &mode_competing_item,
            &separator_3,
            &quit_item,
        ],
    )?;

    let tray_state = app.state::<TrayMenuState>();
    *tray_state.dnd.lock().expect("tray menu mutex poisoned") = Some(dnd_item.clone());
    *tray_state
        .mode_playing
        .lock()
        .expect("tray menu mutex poisoned") = Some(mode_playing_item.clone());
    *tray_state
        .mode_watching
        .lock()
        .expect("tray menu mutex poisoned") = Some(mode_watching_item.clone());
    *tray_state
        .mode_listening
        .lock()
        .expect("tray menu mutex poisoned") = Some(mode_listening_item.clone());
    *tray_state
        .mode_competing
        .lock()
        .expect("tray menu mutex poisoned") = Some(mode_competing_item.clone());

    TrayIconBuilder::new()
        .tooltip("Claude RPC")
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "show" => show_settings(app),
            "dnd" => {
                if let Ok(config) = update_config(|config| config.dnd = !config.dnd) {
                    sync_tray_menu(app, &config);
                }
            }
            "mode_playing" => set_mode(app, "playing"),
            "mode_watching" => set_mode(app, "watching"),
            "mode_listening" => set_mode(app, "listening"),
            "mode_competing" => set_mode(app, "competing"),
            "quit" => {
                let state = app.state::<DaemonState>();
                stop_daemon(&state);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_settings(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

fn set_mode(app: &tauri::AppHandle, mode: &str) {
    if let Ok(config) = update_config(|config| config.rpc_mode = mode.into()) {
        sync_tray_menu(app, &config);
    }
}

fn show_settings(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn sync_tray_menu(app: &tauri::AppHandle, config: &ClaudeConfig) {
    let state = app.state::<TrayMenuState>();
    if let Some(item) = state.dnd.lock().expect("tray menu mutex poisoned").clone() {
        let _ = item.set_checked(config.dnd);
    }
    if let Some(item) = state
        .mode_playing
        .lock()
        .expect("tray menu mutex poisoned")
        .clone()
    {
        let _ = item.set_checked(config.rpc_mode == "playing");
    }
    if let Some(item) = state
        .mode_watching
        .lock()
        .expect("tray menu mutex poisoned")
        .clone()
    {
        let _ = item.set_checked(config.rpc_mode == "watching");
    }
    if let Some(item) = state
        .mode_listening
        .lock()
        .expect("tray menu mutex poisoned")
        .clone()
    {
        let _ = item.set_checked(config.rpc_mode == "listening");
    }
    if let Some(item) = state
        .mode_competing
        .lock()
        .expect("tray menu mutex poisoned")
        .clone()
    {
        let _ = item.set_checked(config.rpc_mode == "competing");
    };
}

fn start_daemon_inner(_app: &tauri::AppHandle, state: &DaemonState) {
    let mut running = state.running.lock().expect("daemon state mutex poisoned");
    if *running {
        return;
    }

    state.stop.store(false, Ordering::SeqCst);
    *state.error.lock().expect("daemon error mutex poisoned") = None;
    *running = true;

    let stop = Arc::clone(&state.stop);
    let running_flag = Arc::clone(&state.running);
    let config_path = config_path().ok();
    let status_path = status_path().ok();
    if let Some(handle) = state
        .handle
        .lock()
        .expect("daemon handle mutex poisoned")
        .take()
    {
        let _ = handle.join();
    }

    let handle = std::thread::spawn(move || {
        daemon::run(stop, config_path, status_path);
        if let Ok(mut running) = running_flag.lock() {
            *running = false;
        }
    });
    *state.handle.lock().expect("daemon handle mutex poisoned") = Some(handle);
}

fn stop_daemon(state: &DaemonState) {
    state.stop.store(true, Ordering::SeqCst);
    if let Some(handle) = state
        .handle
        .lock()
        .expect("daemon handle mutex poisoned")
        .take()
    {
        let _ = handle.join();
    }
}

fn read_daemon_status(state: &DaemonState) -> DaemonStatus {
    let running = *state.running.lock().expect("daemon state mutex poisoned");
    let error = state
        .error
        .lock()
        .expect("daemon error mutex poisoned")
        .clone();

    DaemonStatus {
        running,
        pid: if running {
            Some(std::process::id())
        } else {
            None
        },
        error,
    }
}

fn update_config<F>(mutator: F) -> Result<ClaudeConfig, String>
where
    F: FnOnce(&mut ClaudeConfig),
{
    let mut config = read_config()?;
    mutator(&mut config);
    let config = normalize_config(config);
    write_config(&config)?;
    Ok(config)
}

fn read_config() -> Result<ClaudeConfig, String> {
    match fs::read_to_string(config_path()?) {
        Ok(raw) => Ok(normalize_config(
            serde_json::from_str::<ClaudeConfig>(raw.trim_start_matches('\u{feff}'))
                .unwrap_or_default(),
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(ClaudeConfig::default()),
        Err(err) => Err(err.to_string()),
    }
}

fn write_config(config: &ClaudeConfig) -> Result<(), String> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|err| err.to_string())?;
    fs::write(path, json).map_err(|err| err.to_string())
}

fn normalize_config(mut config: ClaudeConfig) -> ClaudeConfig {
    config.logo_mode = if config.logo_mode.eq_ignore_ascii_case("asset") {
        "asset".into()
    } else {
        "url".into()
    };
    config.rpc_mode = match config.rpc_mode.trim().to_ascii_lowercase().as_str() {
        "watching" | "tv" => "watching".into(),
        "listening" => "listening".into(),
        "competing" => "competing".into(),
        _ => "playing".into(),
    };
    config.buttons = config
        .buttons
        .into_iter()
        .filter_map(clean_button)
        .take(2)
        .collect();
    config
}

fn clean_button(button: RpcButton) -> Option<RpcButton> {
    let label = button
        .label
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let url = button.url.trim().to_string();
    if label.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    Some(RpcButton {
        label: label.chars().take(32).collect(),
        url,
    })
}

fn config_path() -> Result<PathBuf, String> {
    Ok(app_dir()?.join("config.json"))
}

fn status_path() -> Result<PathBuf, String> {
    Ok(app_dir()?.join("status.txt"))
}

fn app_dir() -> Result<PathBuf, String> {
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        return Ok(Path::new(&home).join(".claude-rpc"));
    }
    std::env::current_dir()
        .map(|path| path.join(".claude-rpc"))
        .map_err(|err| err.to_string())
}
