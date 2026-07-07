#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod daemon;

use config::ClaudeConfig;
use serde::Serialize;
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
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};
use tauri_plugin_updater::UpdaterExt;

#[derive(Default)]
struct DaemonState {
    running: Arc<Mutex<bool>>,
    error: Mutex<Option<String>>,
    stop: Arc<AtomicBool>,
    force_refresh: Arc<AtomicBool>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[derive(Default)]
struct UpdateState {
    available: Mutex<Option<UpdateInfo>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInfo {
    version: String,
    notes: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrayInfo {
    dnd: bool,
    start_on_windows: bool,
    startup_label: String,
    rpc_mode: String,
    update_version: Option<String>,
    app_version: String,
}

#[cfg(windows)]
const STARTUP_REG_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const STARTUP_REG_VALUE: &str = "Claude RPC";
#[cfg(target_os = "macos")]
const MACOS_LAUNCH_AGENT_LABEL: &str = "eu.stealthylabs.claude-rpc";

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
    preview_header: Option<String>,
    preview_primary: Option<String>,
    preview_secondary: Option<String>,
    preview_tertiary: Option<String>,
    cost_shown: bool,
    cost_line: Option<String>,
    cost_total_shown: bool,
    cost_total_line: Option<String>,
    project_tokens_shown: bool,
    project_tokens_line: Option<String>,
    all_tokens_shown: bool,
    all_tokens_line: Option<String>,
    model_costs_all: Value,
    model_costs_current: Value,
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

#[tauri::command]
fn load_config() -> Result<ClaudeConfig, String> {
    read_config()
}

#[tauri::command]
fn save_config(config: ClaudeConfig) -> Result<(), String> {
    let config = config::normalize_config(config);
    write_config(&config)?;
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
        preview_header: value
            .get("previewHeader")
            .and_then(Value::as_str)
            .map(str::to_string),
        preview_primary: value
            .get("previewPrimary")
            .and_then(Value::as_str)
            .map(str::to_string),
        preview_secondary: value
            .get("previewSecondary")
            .and_then(Value::as_str)
            .map(str::to_string),
        preview_tertiary: value
            .get("previewTertiary")
            .and_then(Value::as_str)
            .map(str::to_string),
        cost_shown: value
            .get("costShown")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        cost_line: value
            .get("costLine")
            .and_then(Value::as_str)
            .map(str::to_string),
        cost_total_shown: value
            .get("costTotalShown")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        cost_total_line: value
            .get("costTotalLine")
            .and_then(Value::as_str)
            .map(str::to_string),
        project_tokens_shown: value
            .get("projectTokensShown")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        project_tokens_line: value
            .get("projectTokensLine")
            .and_then(Value::as_str)
            .map(str::to_string),
        all_tokens_shown: value
            .get("allTokensShown")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        all_tokens_line: value
            .get("allTokensLine")
            .and_then(Value::as_str)
            .map(str::to_string),
        model_costs_all: value
            .get("modelCostsAll")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
        model_costs_current: value
            .get("modelCostsCurrent")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
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
fn refresh_limits(state: tauri::State<'_, DaemonState>) -> Result<(), String> {
    state.force_refresh.store(true, Ordering::SeqCst);
    open_url("https://claude.ai/settings/usage")
}

fn open_url(url: &str) -> Result<(), String> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("explorer.exe");
        command.arg(url);
        command
    } else {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };

    command.spawn().map(|_| ()).map_err(|err| err.to_string())
}

async fn fetch_update(app: &tauri::AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = app.updater().map_err(|err| err.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => Ok(Some(UpdateInfo {
            version: update.version.clone(),
            notes: update.body.clone().unwrap_or_default(),
        })),
        Ok(None) => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

async fn download_and_install(app: &tauri::AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|err| err.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "No update available".to_string())?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|err| err.to_string())?;
    app.restart();
}

#[tauri::command]
async fn check_update(app: tauri::AppHandle) -> Result<Option<UpdateInfo>, String> {
    let info = fetch_update(&app).await?;
    *app.state::<UpdateState>()
        .available
        .lock()
        .expect("update state mutex poisoned") = info.clone();
    Ok(info)
}

#[tauri::command]
fn pending_update(state: tauri::State<'_, UpdateState>) -> Option<UpdateInfo> {
    state
        .available
        .lock()
        .expect("update state mutex poisoned")
        .clone()
}

#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    download_and_install(&app).await
}

fn read_tray_info(app: &tauri::AppHandle) -> TrayInfo {
    let config = read_config().unwrap_or_default();
    let update_version = app
        .state::<UpdateState>()
        .available
        .lock()
        .expect("update state mutex poisoned")
        .as_ref()
        .map(|info| info.version.clone());
    TrayInfo {
        dnd: config.dnd,
        start_on_windows: is_start_on_windows_enabled(),
        startup_label: startup_menu_label().to_string(),
        rpc_mode: config.rpc_mode,
        update_version,
        app_version: app.package_info().version.to_string(),
    }
}

#[tauri::command]
fn tray_state(app: tauri::AppHandle) -> TrayInfo {
    read_tray_info(&app)
}

#[tauri::command]
async fn tray_action(app: tauri::AppHandle, action: String) -> Result<TrayInfo, String> {
    let hide_tray = || {
        if let Some(window) = app.get_webview_window("tray") {
            let _ = window.hide();
        }
    };
    match action.as_str() {
        "close" => hide_tray(),
        "settings" => {
            hide_tray();
            show_settings(&app);
        }
        "dnd" => {
            update_config(|config| config.dnd = !config.dnd)?;
        }
        "startup" => {
            set_start_on_windows(!is_start_on_windows_enabled())?;
        }
        "mode_playing" => set_mode("playing")?,
        "mode_watching" => set_mode("watching")?,
        "mode_listening" => set_mode("listening")?,
        "mode_competing" => set_mode("competing")?,
        "update" => {
            let pending = app
                .state::<UpdateState>()
                .available
                .lock()
                .expect("update state mutex poisoned")
                .is_some();
            if pending {
                hide_tray();
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = download_and_install(&handle).await;
                });
            } else {
                let info = fetch_update(&app).await?;
                *app.state::<UpdateState>()
                    .available
                    .lock()
                    .expect("update state mutex poisoned") = info;
            }
        }
        "quit" => {
            hide_tray();
            let state = app.state::<DaemonState>();
            stop_daemon(&state);
            app.exit(0);
        }
        other => return Err(format!("unknown tray action: {other}")),
    }
    Ok(read_tray_info(&app))
}

fn main() {
    tauri::Builder::default()
        // Must be the first plugin: a second launch (autostart + manual, double
        // click, installer post-run) is rejected and instead focuses/opens the
        // existing instance's settings — so only one daemon ever drives Discord.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_settings(app);
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(DaemonState::default())
        .manage(UpdateState::default())
        .invoke_handler(tauri::generate_handler![
            load_config,
            save_config,
            load_status,
            start_daemon,
            daemon_status,
            close_settings,
            refresh_limits,
            check_update,
            pending_update,
            install_update,
            tray_state,
            tray_action
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let state = app.state::<DaemonState>();
            start_daemon_inner(&handle, &state);
            create_tray(app)?;
            spawn_update_check(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run Claude RPC tray");
}

fn spawn_update_check(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let Ok(Some(info)) = fetch_update(&app).await else {
            return;
        };
        *app.state::<UpdateState>()
            .available
            .lock()
            .expect("update state mutex poisoned") = Some(info);
    });
}

fn create_tray(app: &mut tauri::App) -> tauri::Result<()> {
    TrayIconBuilder::new()
        .tooltip("Claude RPC")
        .icon(app.default_window_icon().unwrap().clone())
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state: MouseButtonState::Up,
                position,
                ..
            } = event
            {
                match button {
                    MouseButton::Left => show_settings(tray.app_handle()),
                    MouseButton::Right => show_tray_menu(tray.app_handle(), position),
                    MouseButton::Middle => {}
                }
            }
        })
        .build(app)?;

    Ok(())
}

// Logical size of the custom tray menu window; tray.css lays the menu out with
// fixed item heights so the content always fits this box, bottom-anchored.
const TRAY_MENU_WIDTH: f64 = 260.0;
const TRAY_MENU_HEIGHT: f64 = 416.0;

fn show_tray_menu(app: &tauri::AppHandle, cursor: tauri::PhysicalPosition<f64>) {
    let window = match app.get_webview_window("tray") {
        Some(window) => window,
        None => {
            let Ok(window) = tauri::WebviewWindowBuilder::new(
                app,
                "tray",
                tauri::WebviewUrl::App("tray.html".into()),
            )
            .inner_size(TRAY_MENU_WIDTH, TRAY_MENU_HEIGHT)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible(false)
            .build() else {
                return;
            };
            let window_to_hide = window.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::Focused(false) = event {
                    let _ = window_to_hide.hide();
                }
            });
            window
        }
    };

    let size = window
        .outer_size()
        .unwrap_or(tauri::PhysicalSize::new(0, 0));
    let x = (cursor.x - f64::from(size.width)).max(0.0);
    let y = (cursor.y - f64::from(size.height)).max(0.0);
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    let _ = window.show();
    let _ = window.set_focus();
}

fn set_mode(mode: &str) -> Result<(), String> {
    update_config(|config| config.rpc_mode = mode.into()).map(|_| ())
}

fn show_settings(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return;
    }
    if let Ok(window) =
        tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
            .title("Claude RPC Settings")
            .inner_size(790.0, 640.0)
            .min_inner_size(680.0, 480.0)
            .resizable(true)
            .decorations(false)
            .build()
    {
        let window_to_hide = window.clone();
        window.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window_to_hide.hide();
            }
        });
    }
}

#[cfg(windows)]
fn startup_menu_label() -> &'static str {
    "Start on Windows"
}

#[cfg(not(windows))]
fn startup_menu_label() -> &'static str {
    "Start at Login"
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(windows)]
fn is_start_on_windows_enabled() -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("reg.exe")
        .args(["query", STARTUP_REG_KEY, "/v", STARTUP_REG_VALUE])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn is_start_on_windows_enabled() -> bool {
    launch_agent_path()
        .map(|path| path.exists())
        .unwrap_or(false)
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn is_start_on_windows_enabled() -> bool {
    false
}

#[cfg(windows)]
fn set_start_on_windows(enabled: bool) -> Result<(), String> {
    if enabled {
        let exe = std::env::current_exe().map_err(|err| err.to_string())?;
        let command = format!("\"{}\"", exe.to_string_lossy());
        run_reg(&[
            "add",
            STARTUP_REG_KEY,
            "/v",
            STARTUP_REG_VALUE,
            "/t",
            "REG_SZ",
            "/d",
            command.as_str(),
            "/f",
        ])
    } else if is_start_on_windows_enabled() {
        run_reg(&["delete", STARTUP_REG_KEY, "/v", STARTUP_REG_VALUE, "/f"])
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn set_start_on_windows(enabled: bool) -> Result<(), String> {
    let path = launch_agent_path()?;
    if enabled {
        let exe = std::env::current_exe().map_err(|err| err.to_string())?;
        let exe = xml_escape(&exe.to_string_lossy());
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{MACOS_LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#
        );
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        fs::write(path, plist).map_err(|err| err.to_string())
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn set_start_on_windows(_enabled: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn run_reg(args: &[&str]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new("reg.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        Err(format!("reg.exe failed: {}", output.status))
    } else {
        Err(stderr)
    }
}

#[cfg(target_os = "macos")]
fn launch_agent_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
    Ok(Path::new(&home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{MACOS_LAUNCH_AGENT_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn start_daemon_inner(_app: &tauri::AppHandle, state: &DaemonState) {
    let mut running = state.running.lock().expect("daemon state mutex poisoned");
    if *running {
        return;
    }

    state.stop.store(false, Ordering::SeqCst);
    state.force_refresh.store(false, Ordering::SeqCst);
    *state.error.lock().expect("daemon error mutex poisoned") = None;
    *running = true;

    let stop = Arc::clone(&state.stop);
    let force_refresh = Arc::clone(&state.force_refresh);
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
        daemon::run(stop, force_refresh, config_path, status_path);
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
    let config = config::normalize_config(config);
    write_config(&config)?;
    Ok(config)
}

fn read_config() -> Result<ClaudeConfig, String> {
    match fs::read_to_string(config_path()?) {
        Ok(raw) => Ok(config::normalize_config(
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

fn config_path() -> Result<PathBuf, String> {
    Ok(app_dir()?.join("config.json"))
}

fn status_path() -> Result<PathBuf, String> {
    Ok(app_dir()?.join("status.txt"))
}

fn app_dir() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("CLAUDE_RPC_DIR") {
        return Ok(expand_home(&path));
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        return Ok(Path::new(&home).join(".claude-rpc"));
    }
    std::env::current_dir()
        .map(|path| path.join(".claude-rpc"))
        .map_err(|err| err.to_string())
}

fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return home_dir();
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return home_dir().join(rest);
    }
    PathBuf::from(value)
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
