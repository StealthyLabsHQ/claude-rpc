use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    mem::size_of,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use windows::{
    core::{BOOL, PWSTR},
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM},
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_APARTMENTTHREADED,
            },
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            Threading::{
                GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
        UI::{
            Accessibility::{
                CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTogglePattern,
                IUIAutomationTreeWalker, ToggleState_On, TreeScope_Descendants,
                UIA_ButtonControlTypeId, UIA_TogglePatternId,
            },
            WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible},
        },
    },
};

const DEFAULT_DISCORD_CLIENT_ID: &str = "1483898157854363799";
const SCAN_INTERVAL_MS: u64 = 1_000;
const RPC_REFRESH_INTERVAL_MS: u64 = 15_000;
const IDLE_GRACE_MS: u64 = 10_000;
const LIMITS_CACHE_MS: u64 = 6 * 60 * 60 * 1_000;
const ACTIVITY_PLAYING: u8 = 0;
const ACTIVITY_LISTENING: u8 = 2;
const ACTIVITY_WATCHING: u8 = 3;
const ACTIVITY_COMPETING: u8 = 5;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientType {
    Idle,
    Code,
    Desktop,
}

#[derive(Debug, Clone)]
struct DetectionResult {
    client: ClientType,
    mode: Option<String>,
    submode: Option<String>,
    model: Option<String>,
    limits_line: Option<String>,
    debug_line: Option<String>,
    provider: String,
    project_name: Option<String>,
    code_instances: usize,
    started_at_ms: Option<u64>,
}

impl Default for DetectionResult {
    fn default() -> Self {
        Self {
            client: ClientType::Idle,
            mode: None,
            submode: None,
            model: None,
            limits_line: None,
            debug_line: None,
            provider: "Unknown".into(),
            project_name: None,
            code_instances: 0,
            started_at_ms: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct DesktopInfo {
    mode: Option<String>,
    submode: Option<String>,
    model: Option<String>,
    adaptive: bool,
    extended: bool,
    effort: Option<String>,
    limits: Vec<UsageLimitEntry>,
    debug_line: Option<String>,
}

#[derive(Debug, Clone)]
struct SessionInfo {
    file: PathBuf,
    started_at_ms: Option<u64>,
    project_name: Option<String>,
    model: Option<String>,
}

#[derive(Default)]
struct StateMachine {
    last_non_idle: Option<DetectionResult>,
    last_non_idle_at_ms: u64,
    cached_limits: Vec<UsageLimitEntry>,
    cached_limits_at_ms: u64,
}

#[cfg(windows)]
struct ProcessEntry {
    process_id: u32,
    name: String,
}

#[derive(Debug, Clone)]
struct ProcessSnapshot {
    process_id: u32,
    name: String,
    executable_path: Option<String>,
    creation_date_ms: Option<u64>,
}

#[derive(Debug, Clone)]
struct DesktopModelCandidate {
    model: String,
    adaptive: bool,
    extended: bool,
    effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UsageLimitEntry {
    label: String,
    used_percent: u8,
    reset: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LimitsCache {
    updated_at: u64,
    limits: Vec<UsageLimitEntry>,
}

#[derive(Debug, Clone, Copy)]
struct LimitVisibility {
    enabled: bool,
    show_5h: bool,
    show_all: bool,
    show_sonnet: bool,
    show_design: bool,
}

pub fn run(stop: Arc<AtomicBool>, config_path: Option<PathBuf>, status_path: Option<PathBuf>) {
    let config_path = config_path.unwrap_or_else(|| app_dir().join("config.json"));
    let status_path = status_path.unwrap_or_else(|| app_dir().join("status.txt"));
    let client_id = std::env::var("DISCORD_CLIENT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_DISCORD_CLIENT_ID.to_string());
    let scan_interval_ms = parse_env_u64("SCAN_INTERVAL_MS", SCAN_INTERVAL_MS, 250);
    let idle_grace_ms = parse_env_u64("IDLE_GRACE_MS", IDLE_GRACE_MS, 0);

    let mut machine = StateMachine::default();
    let mut ipc: Option<DiscordIpc> = None;
    let mut last_key = String::new();
    let mut last_rpc_refresh_at = 0;
    let mut config_modified = modified_ms(&config_path);
    let mut config = read_config(&config_path);
    let mut result = detect(
        &mut machine,
        idle_grace_ms,
        config.verbose,
        limit_visibility(&config),
    );
    let mut last_scan_at = 0;

    while !stop.load(Ordering::SeqCst) {
        if modified_ms(&config_path) != config_modified {
            config_modified = modified_ms(&config_path);
            config = read_config(&config_path);
            result = detect(
                &mut machine,
                idle_grace_ms,
                config.verbose,
                limit_visibility(&config),
            );
            last_scan_at = now_ms();
            last_key.clear();
        }

        if ipc.is_none() && !config.dnd {
            ipc = DiscordIpc::connect(&client_id).ok();
            if ipc.is_some() {
                last_key.clear();
            }
        }

        let now = now_ms();
        if now.saturating_sub(last_scan_at) >= scan_interval_ms {
            result = detect(
                &mut machine,
                idle_grace_ms,
                config.verbose,
                limit_visibility(&config),
            );
            last_scan_at = now;
        }

        write_status(
            &status_path,
            &build_status(
                &result,
                ipc.as_ref().and_then(|client| client.username.as_deref()),
            ),
        );

        let key = presence_key(&result, &config);
        if config.dnd {
            if let Some(client) = ipc.as_mut() {
                let _ = client.clear_activity();
            }
            ipc = None;
            last_key.clear();
            last_rpc_refresh_at = 0;
        } else if key != last_key
            || now.saturating_sub(last_rpc_refresh_at) >= RPC_REFRESH_INTERVAL_MS
        {
            if let Some(client) = ipc.as_mut() {
                let sent = match build_activity(&result, &config) {
                    Some(activity) => client.set_activity(activity),
                    None => client.clear_activity(),
                };

                if sent.is_ok() {
                    last_key = key;
                    last_rpc_refresh_at = now;
                } else {
                    ipc = None;
                    last_key.clear();
                    last_rpc_refresh_at = 0;
                }
            }
        }

        sleep_polling(&stop, 250);
    }

    if let Some(client) = ipc.as_mut() {
        let _ = client.clear_activity();
    }
    clear_status(&status_path);
}

fn detect(
    machine: &mut StateMachine,
    idle_grace_ms: u64,
    verbose: bool,
    limit_visibility: LimitVisibility,
) -> DetectionResult {
    let mut desktop_found = false;
    let mut desktop_process_ids = Vec::new();
    let mut code_count = 0usize;
    let mut oldest = None;

    for process in scan_claude_processes() {
        if is_desktop_process(&process) {
            desktop_found = true;
            desktop_process_ids.push(process.process_id);
            oldest = min_option(oldest, process.creation_date_ms);
        } else if is_code_process(&process) {
            code_count += 1;
            oldest = min_option(oldest, process.creation_date_ms);
        }
    }

    let session = read_session_info();
    if code_count == 0 {
        if let Some(session) = &session {
            if modified_ms(&session.file)
                .map(|mtime| now_ms().saturating_sub(mtime) <= 15_000)
                .unwrap_or(false)
            {
                code_count = 1;
                oldest = min_option(oldest, session.started_at_ms);
            }
        }
    }

    let client = if desktop_found {
        ClientType::Desktop
    } else if code_count > 0 {
        ClientType::Code
    } else {
        ClientType::Idle
    };

    let desktop = if client == ClientType::Desktop {
        read_desktop_info(&desktop_process_ids, verbose)
    } else {
        DesktopInfo::default()
    };
    let limits = current_limits(machine, &desktop.limits);
    let limits_line = if limit_visibility.enabled {
        limits_line(&limits, limit_visibility)
    } else {
        None
    };
    let desktop_model = if client == ClientType::Desktop {
        detect_desktop_model(&desktop)
    } else {
        None
    };

    let mut result = DetectionResult {
        client,
        mode: desktop.mode,
        submode: desktop.submode,
        model: match client {
            ClientType::Desktop => desktop_model,
            ClientType::Code => detect_code_model(session.as_ref()),
            ClientType::Idle => None,
        },
        limits_line,
        debug_line: desktop.debug_line,
        provider: detect_provider(),
        project_name: session
            .as_ref()
            .and_then(|session| session.project_name.clone()),
        code_instances: code_count,
        started_at_ms: oldest
            .or_else(|| session.as_ref().and_then(|session| session.started_at_ms)),
    };

    if client == ClientType::Code {
        result.model = append_code_effort(result.model);
    }

    let now = now_ms();
    if result.client != ClientType::Idle {
        machine.last_non_idle = Some(result.clone());
        machine.last_non_idle_at_ms = now;
        return result;
    }

    if now.saturating_sub(machine.last_non_idle_at_ms) <= idle_grace_ms {
        if let Some(last) = machine.last_non_idle.clone() {
            return last;
        }
    }

    DetectionResult {
        provider: detect_provider(),
        ..DetectionResult::default()
    }
}

fn build_activity(result: &DetectionResult, config: &ClaudeConfig) -> Option<Value> {
    if result.client == ClientType::Idle {
        return None;
    }

    let mode = normalize_mode(&config.rpc_mode);
    let activity_type = match mode.as_str() {
        "watching" => ACTIVITY_WATCHING,
        "listening" => ACTIVITY_LISTENING,
        "competing" => ACTIVITY_COMPETING,
        _ => ACTIVITY_PLAYING,
    };

    let mut activity = json!({
        "name": "Claude AI",
        "type": activity_type,
        "created_at": now_ms(),
        "instance": false,
        "details": build_details(result, &mode),
        "state": build_state(result, config),
        "assets": {
            "large_image": logo_image(config),
            "large_text": "Powered by Anthropic",
            "small_image": "terminal_icon",
            "small_text": small_image_text(result),
        },
    });

    if let Some(started_at_ms) = result.started_at_ms {
        activity["timestamps"] = json!({ "start": started_at_ms / 1000 });
    }
    if mode == "watching" && !config.buttons.is_empty() {
        activity["buttons"] =
            serde_json::to_value(config.buttons.iter().take(2).collect::<Vec<_>>()).ok()?;
    }

    Some(activity)
}

fn build_details(result: &DetectionResult, mode: &str) -> String {
    let base = match (result.client, mode) {
        (ClientType::Desktop, "watching") => "Watching Claude",
        (ClientType::Code, "watching") => "Watching Claude Code",
        (ClientType::Desktop, _) => "Claude Desktop",
        (ClientType::Code, _) => "Claude Code",
        (ClientType::Idle, _) => "",
    };

    if result.client == ClientType::Desktop {
        if let Some(mode_label) = desktop_mode_label(result) {
            return format!("{base} ({mode_label})");
        }
    }

    if result.client == ClientType::Code {
        if let Some(repo) = sanitize_field(result.project_name.as_deref(), 32) {
            let candidate = format!("{base} - {repo}");
            if candidate.len() <= 96 {
                return candidate;
            }
        }
    }

    base.to_string()
}

fn build_state(result: &DetectionResult, config: &ClaudeConfig) -> String {
    let model = format_rpc_model(
        result.model.as_deref().unwrap_or("Claude"),
        config.show_effort,
    );
    let mut parts = vec![model];
    if config.show_provider {
        parts.push(result.provider.clone());
    }
    if let Some(limits) = result.limits_line.as_deref() {
        parts.push(limits.to_string());
    }
    truncate(parts.join(" | "), 128)
}

fn format_rpc_model(model: &str, show_effort: bool) -> String {
    if show_effort {
        return model.to_string();
    }

    let parts = model
        .split(" | ")
        .filter(|part| !is_effort_label(part))
        .collect::<Vec<_>>();
    if parts.is_empty() {
        model.to_string()
    } else {
        parts.join(" | ")
    }
}

fn is_effort_label(value: &str) -> bool {
    matches!(
        normalize_ui_label(value).as_str(),
        "low" | "medium" | "high" | "extra high" | "xhigh" | "max"
    )
}

fn small_image_text(result: &DetectionResult) -> String {
    match result.client {
        ClientType::Desktop => "Claude Desktop".into(),
        ClientType::Code => "Claude Code CLI".into(),
        ClientType::Idle => "Claude".into(),
    }
}

fn logo_image(config: &ClaudeConfig) -> String {
    if config.logo_mode.eq_ignore_ascii_case("asset") {
        "claude_logo".into()
    } else {
        "https://raw.githubusercontent.com/StealthyLabsHQ/claude-rpc/main/logo/discord.png".into()
    }
}

fn build_status(result: &DetectionResult, discord_user: Option<&str>) -> Value {
    let claude_line = match result.client {
        ClientType::Desktop => desktop_mode_label(result)
            .map(|mode| format!("Claude: Desktop ({mode})"))
            .unwrap_or_else(|| "Claude: Desktop".into()),
        ClientType::Code => {
            if result.code_instances > 1 {
                format!("Claude: CLI (Code) [{}]", result.code_instances)
            } else {
                "Claude: CLI (Code)".into()
            }
        }
        ClientType::Idle => "Claude: Off".into(),
    };
    let discord_line = match discord_user {
        Some(user) => format!("Discord: Connected ({user})"),
        None => "Discord: RPC disabled".into(),
    };

    json!({
        "version": 5,
        "summary": "Claude RPC",
        "claudeLine": claude_line,
        "modelLine": result.model.clone().unwrap_or_else(|| "Auto-detect".into()),
        "limitsLine": result.limits_line.clone(),
        "providerLine": format!("Provider: {}", result.provider),
        "discordLine": discord_line,
        "debugLine": result.debug_line.clone(),
    })
}

fn desktop_mode_label(result: &DetectionResult) -> Option<String> {
    let mode = result.mode.as_deref()?;
    if mode.is_empty() {
        return None;
    }
    if let Some(submode) = result.submode.as_deref().filter(|value| !value.is_empty()) {
        Some(format!("{mode} - {submode}"))
    } else {
        Some(mode.to_string())
    }
}

fn presence_key(result: &DetectionResult, config: &ClaudeConfig) -> String {
    serde_json::to_string(&json!({
        "client": format!("{:?}", result.client),
        "mode": result.mode,
        "submode": result.submode,
        "model": result.model,
        "limits": result.limits_line,
        "provider": result.provider,
        "project": result.project_name,
        "rpcMode": config.rpc_mode,
        "dnd": config.dnd,
        "logoMode": config.logo_mode,
        "showLimits": config.show_limits,
        "showLimit5h": config.show_limit_5h,
        "showLimitAll": config.show_limit_all,
        "showLimitSonnet": config.show_limit_sonnet,
        "showLimitDesign": config.show_limit_design,
        "showProvider": config.show_provider,
        "showEffort": config.show_effort,
        "buttons": config.buttons,
    }))
    .unwrap_or_default()
}

fn limit_visibility(config: &ClaudeConfig) -> LimitVisibility {
    LimitVisibility {
        enabled: config.show_limits,
        show_5h: config.show_limit_5h,
        show_all: config.show_limit_all,
        show_sonnet: config.show_limit_sonnet,
        show_design: config.show_limit_design,
    }
}

fn read_config(path: &Path) -> ClaudeConfig {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return ClaudeConfig::default(),
    };
    normalize_config(
        serde_json::from_str::<ClaudeConfig>(raw.trim_start_matches('\u{feff}'))
            .unwrap_or_default(),
    )
}

fn normalize_config(mut config: ClaudeConfig) -> ClaudeConfig {
    config.logo_mode = if config.logo_mode.eq_ignore_ascii_case("asset") {
        "asset".into()
    } else {
        "url".into()
    };
    config.rpc_mode = normalize_mode(&config.rpc_mode);
    config.buttons = config
        .buttons
        .into_iter()
        .filter_map(|button| {
            let label = clean_label(&button.label)?;
            let url = clean_url(&button.url)?;
            Some(RpcButton { label, url })
        })
        .take(2)
        .collect();
    config
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

#[cfg(windows)]
fn scan_claude_processes() -> Vec<ProcessSnapshot> {
    list_process_entries()
        .into_iter()
        .filter(|entry| {
            entry.name.eq_ignore_ascii_case("claude.exe")
                || entry.name.eq_ignore_ascii_case("claude desktop.exe")
        })
        .map(|entry| ProcessSnapshot {
            process_id: entry.process_id,
            name: entry.name,
            executable_path: query_process_path(entry.process_id),
            creation_date_ms: query_process_creation_ms(entry.process_id),
        })
        .collect()
}

#[cfg(not(windows))]
fn scan_claude_processes() -> Vec<ProcessSnapshot> {
    Vec::new()
}

fn is_desktop_process(process: &ProcessSnapshot) -> bool {
    let exe = process
        .executable_path
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    process.name.eq_ignore_ascii_case("claude desktop.exe")
        || exe.contains("windowsapps")
        || exe.contains("anthropicclaude")
        || exe.contains("\\program files\\claude")
        || exe.contains("\\appdata\\local\\anthropic")
}

fn is_code_process(process: &ProcessSnapshot) -> bool {
    process.name.eq_ignore_ascii_case("claude.exe") && !is_desktop_process(process)
}

#[cfg(windows)]
fn list_process_entries() -> Vec<ProcessEntry> {
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    let mut entry = PROCESSENTRY32W::default();
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;

    if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
        loop {
            entries.push(ProcessEntry {
                process_id: entry.th32ProcessID,
                name: wide_to_string(&entry.szExeFile),
            });

            if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }

    close_handle(snapshot);
    entries
}

#[cfg(windows)]
fn query_process_path(process_id: u32) -> Option<String> {
    let handle = open_process_query(process_id)?;
    let mut buffer = vec![0u16; 32_768];
    let mut len = buffer.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };
    close_handle(handle);
    result.ok()?;
    Some(String::from_utf16_lossy(&buffer[..len as usize]))
}

#[cfg(windows)]
fn query_process_creation_ms(process_id: u32) -> Option<u64> {
    let handle = open_process_query(process_id)?;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let result =
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    close_handle(handle);
    result.ok()?;
    filetime_to_unix_ms(creation)
}

#[cfg(windows)]
fn open_process_query(process_id: u32) -> Option<HANDLE> {
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()
}

#[cfg(windows)]
fn close_handle(handle: HANDLE) {
    let _ = unsafe { CloseHandle(handle) };
}

#[cfg(windows)]
fn wide_to_string(value: &[u16]) -> String {
    let len = value.iter().position(|ch| *ch == 0).unwrap_or(value.len());
    String::from_utf16_lossy(&value[..len])
}

#[cfg(windows)]
fn filetime_to_unix_ms(value: FILETIME) -> Option<u64> {
    const WINDOWS_TO_UNIX_EPOCH_MS: u64 = 11_644_473_600_000;
    let ticks = ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64;
    let ms = ticks / 10_000;
    ms.checked_sub(WINDOWS_TO_UNIX_EPOCH_MS)
}

fn read_desktop_info(process_ids: &[u32], verbose: bool) -> DesktopInfo {
    let mut info = read_desktop_config_info();
    if let Some(ui_info) = read_desktop_ui_info(process_ids, info.mode.as_deref(), verbose) {
        if ui_info.mode.is_some() {
            info.mode = ui_info.mode;
        }
        if ui_info.submode.is_some() {
            info.submode = ui_info.submode;
        }
        if ui_info.model.is_some() {
            info.model = ui_info.model;
        }
        if ui_info.effort.is_some() {
            info.effort = ui_info.effort;
        }
        if !ui_info.limits.is_empty() {
            info.limits = ui_info.limits;
        }
        if ui_info.debug_line.is_some() {
            info.debug_line = ui_info.debug_line;
        }
        info.adaptive |= ui_info.adaptive;
        info.extended |= ui_info.extended;
    }
    info
}

fn read_desktop_config_info() -> DesktopInfo {
    let path = roaming_app_data()
        .join("Claude")
        .join("claude_desktop_config.json");
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return DesktopInfo::default(),
    };
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let raw_mode = value
        .get("preferences")
        .and_then(|prefs| prefs.get("sidebarMode"))
        .and_then(Value::as_str);
    DesktopInfo {
        mode: raw_mode.and_then(map_desktop_mode),
        ..DesktopInfo::default()
    }
}

#[cfg(windows)]
fn read_desktop_ui_info(
    process_ids: &[u32],
    fallback_mode: Option<&str>,
    verbose: bool,
) -> Option<DesktopInfo> {
    let hwnd = find_desktop_window(process_ids)?;
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_err() {
        return None;
    }
    let info = unsafe { read_desktop_ui_info_from_window(hwnd, fallback_mode, verbose) }.ok();
    unsafe { CoUninitialize() };
    info
}

#[cfg(not(windows))]
fn read_desktop_ui_info(
    _process_ids: &[u32],
    _fallback_mode: Option<&str>,
    _verbose: bool,
) -> Option<DesktopInfo> {
    None
}

#[cfg(windows)]
fn find_desktop_window(process_ids: &[u32]) -> Option<HWND> {
    if process_ids.is_empty() {
        return None;
    }

    struct WindowSearch<'a> {
        process_ids: &'a [u32],
        hwnd: Option<HWND>,
    }

    unsafe extern "system" fn enum_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut WindowSearch);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        if search.process_ids.contains(&pid) && IsWindowVisible(hwnd).as_bool() {
            search.hwnd = Some(hwnd);
            return BOOL(0);
        }

        BOOL(1)
    }

    let mut search = WindowSearch {
        process_ids,
        hwnd: None,
    };
    let _ = unsafe {
        EnumWindows(
            Some(enum_window),
            LPARAM((&mut search as *mut WindowSearch).cast::<()>() as isize),
        )
    };
    search.hwnd
}

#[cfg(windows)]
unsafe fn read_desktop_ui_info_from_window(
    hwnd: HWND,
    fallback_mode: Option<&str>,
    verbose: bool,
) -> windows::core::Result<DesktopInfo> {
    let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
    let root = automation.ElementFromHandle(hwnd)?;
    let condition = automation.CreateTrueCondition()?;
    let elements = root.FindAll(TreeScope_Descendants, &condition)?;
    let length = elements.Length()?.min(1_500);

    let mut names = Vec::new();
    let mut best_model: Option<(DesktopModelCandidate, i32)> = None;
    let mut adaptive = false;
    let mut extended = false;

    for index in 0..length {
        let Ok(element) = elements.GetElement(index) else {
            continue;
        };
        let Ok(name_bstr) = element.CurrentName() else {
            continue;
        };
        let name = name_bstr.to_string();
        let name = name.trim();
        if name.is_empty() {
            continue;
        }

        names.push(name.to_string());

        let offscreen = element
            .CurrentIsOffscreen()
            .map(|value| value.as_bool())
            .unwrap_or(true);
        let is_button = element
            .CurrentControlType()
            .map(|value| value == UIA_ButtonControlTypeId)
            .unwrap_or(false);

        if let Some(candidate) = parse_desktop_model_name(name) {
            let score = if candidate.effort.is_some() { 4 } else { 0 }
                + if !offscreen { 3 } else { 1 }
                + if is_button { 2 } else { 0 };
            if best_model
                .as_ref()
                .map(|(_, best_score)| score > *best_score)
                .unwrap_or(true)
            {
                best_model = Some((candidate, score));
            }
        }

        match normalize_ui_label(name).as_str() {
            "adaptive thinking" => {
                adaptive |= is_toggle_on(&automation, &element).unwrap_or(false);
            }
            "extended thinking" => {
                extended |= is_toggle_on(&automation, &element).unwrap_or(false);
            }
            _ => {}
        }
    }

    let mut info = desktop_info_from_ui_names(&names, fallback_mode);
    info.limits = parse_usage_limits(&names);
    if let Some((candidate, _)) = best_model {
        info.model = Some(candidate.model);
        info.adaptive |= candidate.adaptive;
        info.extended |= candidate.extended;
        if candidate.effort.is_some() {
            info.effort = candidate.effort;
        }
    }
    info.adaptive |= adaptive;
    info.extended |= extended;
    if verbose {
        info.debug_line = write_ui_debug(&names, &info);
    }

    Ok(info)
}

#[cfg(windows)]
unsafe fn is_toggle_on(automation: &IUIAutomation, element: &IUIAutomationElement) -> Option<bool> {
    if let Some(state) = read_toggle_state(element) {
        return Some(state);
    }

    let walker = automation.RawViewWalker().ok()?;
    if let Ok(parent) = walker.GetParentElement(element) {
        if let Some(state) = read_toggle_state(&parent) {
            return Some(state);
        }
        if let Some(state) = read_child_toggle_state(&walker, &parent, 12) {
            return Some(state);
        }
    }

    read_child_toggle_state(&walker, element, 12)
}

#[cfg(windows)]
unsafe fn read_toggle_state(element: &IUIAutomationElement) -> Option<bool> {
    let pattern: IUIAutomationTogglePattern =
        element.GetCurrentPatternAs(UIA_TogglePatternId).ok()?;
    pattern
        .CurrentToggleState()
        .ok()
        .map(|state| state == ToggleState_On)
}

#[cfg(windows)]
unsafe fn read_child_toggle_state(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
    limit: usize,
) -> Option<bool> {
    let mut child = walker.GetFirstChildElement(element).ok();
    let mut count = 0usize;

    while let Some(current) = child {
        if let Some(state) = read_toggle_state(&current) {
            return Some(state);
        }
        count += 1;
        if count >= limit {
            break;
        }
        child = walker.GetNextSiblingElement(&current).ok();
    }

    None
}

fn detect_desktop_model(info: &DesktopInfo) -> Option<String> {
    format_desktop_model(info)
        .or_else(read_settings_model)
        .or_else(|| {
            std::env::var("CLAUDE_MODEL")
                .ok()
                .and_then(|v| format_model_name(&v))
        })
}

fn detect_code_model(session: Option<&SessionInfo>) -> Option<String> {
    read_settings_model()
        .or_else(|| session.and_then(|session| session.model.clone()))
        .or_else(|| {
            std::env::var("CLAUDE_MODEL")
                .ok()
                .and_then(|v| format_model_name(&v))
        })
        .or_else(|| {
            std::env::var("ANTHROPIC_MODEL")
                .ok()
                .and_then(|v| format_model_name(&v))
        })
}

fn read_settings_model() -> Option<String> {
    let raw = fs::read_to_string(claude_dir().join("settings.json")).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value
        .get("model")
        .and_then(Value::as_str)
        .and_then(format_model_name)
}

fn append_code_effort(model: Option<String>) -> Option<String> {
    let model = model?;
    let effort = detect_code_effort()?;
    if model
        .to_ascii_lowercase()
        .contains(&effort.to_ascii_lowercase())
    {
        Some(model)
    } else {
        Some(format!("{model} | {effort}"))
    }
}

fn detect_code_effort() -> Option<String> {
    let raw = fs::read_to_string(claude_dir().join("settings.json")).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let effort = value
        .get("effortLevel")
        .and_then(Value::as_str)?
        .trim()
        .to_ascii_lowercase();
    Some(
        match effort.as_str() {
            "low" => "Low",
            "medium" => "Medium",
            "high" => "High",
            "xhigh" | "extrahigh" | "extra high" => "Extra high",
            "max" => "Max",
            _ => return None,
        }
        .into(),
    )
}

fn desktop_info_from_ui_names(names: &[String], fallback_mode: Option<&str>) -> DesktopInfo {
    let mut chat_score = 0;
    let mut cowork_score = 0;
    let mut code_score = 0;
    let mut dispatch = false;

    for name in names {
        let norm = normalize_ui_label(name);

        match norm.as_str() {
            "new task" => cowork_score += 5,
            "work in a project" => cowork_score += 3,
            "computer use" => cowork_score += 4,
            "code permissions" => cowork_score += 4,
            "outputs" => cowork_score += 4,
            "keep awake" => cowork_score += 4,
            "allow all browser actions" => cowork_score += 4,
            "sync tasks and refresh memory" => cowork_score += 3,
            "initialize productivity system" => cowork_score += 3,
            "dispatch" => cowork_score += 1,
            "scheduled" => cowork_score += 1,
            "new session" => code_score += 5,
            "routines" => code_score += 4,
            "overview" => code_score += 3,
            "models" => code_score += 3,
            "favorite model" => code_score += 3,
            "current streak" => code_score += 3,
            "longest streak" => code_score += 3,
            "peak hour" => code_score += 3,
            "total tokens" => code_score += 3,
            "active days" => code_score += 3,
            "messages" => code_score += 2,
            "sessions" => code_score += 2,
            "new chat" => chat_score += 5,
            "artifacts" => chat_score += 4,
            "learn" => chat_score += 4,
            "write" => chat_score += 4,
            "from calendar" => chat_score += 4,
            "from gmail" => chat_score += 4,
            _ => {}
        }

        if norm.starts_with("lets knock something off your list")
            || norm.starts_with("let's knock something off your list")
        {
            cowork_score += 6;
        }
        if norm.starts_with("get to work with productivity") {
            cowork_score += 3;
        }
        if norm.starts_with("whats up next") || norm.starts_with("what's up next") {
            code_score += 5;
        }
        if norm.starts_with("back at it") {
            chat_score += 4;
        }
        if norm.starts_with("dispatch background conversation")
            || norm.starts_with("dispatch to claude and check in")
            || norm.starts_with("files claude shares will appear here")
        {
            cowork_score += 1;
            dispatch = true;
        }
    }

    let mut ranked = [
        ("Chat", chat_score),
        ("Cowork", cowork_score),
        ("Code", code_score),
    ];
    ranked.sort_by(|left, right| right.1.cmp(&left.1));

    let mode = if ranked[0].1 <= 0 {
        fallback_mode.map(str::to_string)
    } else if ranked[0].1 == ranked[1].1 {
        fallback_mode
            .map(str::to_string)
            .or_else(|| Some(ranked[0].0.to_string()))
    } else {
        Some(ranked[0].0.to_string())
    };

    let submode = if dispatch && mode.as_deref() == Some("Cowork") {
        Some("Dispatch".into())
    } else {
        None
    };

    DesktopInfo {
        mode,
        submode,
        ..DesktopInfo::default()
    }
}

fn parse_desktop_model_name(raw: &str) -> Option<DesktopModelCandidate> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }

    let lower = value.to_ascii_lowercase();
    let model = if lower.starts_with("claude-") || lower.contains("claude-opus-") {
        format_model_name(value)?
    } else if lower.contains("opus plan") {
        "Opus Plan / Sonnet 4.6".into()
    } else {
        let family = if lower.contains("opus") {
            "Opus"
        } else if lower.contains("sonnet") {
            "Sonnet"
        } else if lower.contains("haiku") {
            "Haiku"
        } else {
            return None;
        };
        let version =
            extract_model_version(value, family).unwrap_or_else(|| default_model_version(family));
        let context = if lower.contains("1m") { " (1M)" } else { "" };
        format!("Claude {family} {version}{context}")
    };

    Some(DesktopModelCandidate {
        model,
        adaptive: lower.contains("adaptive thinking") || lower.contains(" adaptive"),
        extended: lower.contains("extended thinking") || lower.contains(" extended"),
        effort: extract_effort_label(&lower),
    })
}

fn format_desktop_model(info: &DesktopInfo) -> Option<String> {
    let mut model = info.model.clone()?;
    append_unique_label(&mut model, info.adaptive, "Adaptive");
    append_unique_label(&mut model, info.extended, "Extended");
    if let Some(effort) = info.effort.as_deref() {
        append_unique_label(&mut model, true, effort);
    }
    Some(truncate(model, 128))
}

fn append_unique_label(model: &mut String, enabled: bool, label: &str) {
    if enabled
        && !model
            .to_ascii_lowercase()
            .contains(&label.to_ascii_lowercase())
    {
        model.push_str(" | ");
        model.push_str(label);
    }
}

fn normalize_ui_label(value: &str) -> String {
    value
        .replace('\u{2019}', "'")
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_model_version(value: &str, family: &str) -> Option<String> {
    let tokens = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>();
    let parts = tokens.split_whitespace().collect::<Vec<_>>();

    for (index, part) in parts.iter().enumerate() {
        if !part.eq_ignore_ascii_case(family) {
            continue;
        }
        for candidate in parts.iter().skip(index + 1).take(3) {
            let normalized = candidate
                .chars()
                .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
                .collect::<String>();
            if normalized.contains('.') && normalized.chars().any(|ch| ch.is_ascii_digit()) {
                return Some(normalized);
            }
        }
    }

    None
}

fn default_model_version(family: &str) -> String {
    match family {
        "Haiku" => "4.5",
        _ => "4.6",
    }
    .into()
}

fn extract_effort_label(lower: &str) -> Option<String> {
    if lower.contains("extra high") || lower.contains("xhigh") {
        Some("Extra high".into())
    } else if lower.contains("medium") {
        Some("Medium".into())
    } else if lower.contains("high") {
        Some("High".into())
    } else if lower.contains("low") {
        Some("Low".into())
    } else if lower.contains("max") {
        Some("Max".into())
    } else {
        None
    }
}

fn parse_usage_limits(names: &[String]) -> Vec<UsageLimitEntry> {
    let mut entries = Vec::new();

    for (index, name) in names.iter().enumerate() {
        let Some(used_percent) = parse_used_percent(name) else {
            continue;
        };
        let Some((label, label_index)) = find_limit_label(names, index) else {
            continue;
        };
        if entries
            .iter()
            .any(|entry: &UsageLimitEntry| entry.label == label)
        {
            continue;
        }
        entries.push(UsageLimitEntry {
            label,
            used_percent,
            reset: find_limit_reset(names, label_index, index),
        });
    }

    sort_limit_entries(&mut entries);
    entries
}

fn parse_used_percent(value: &str) -> Option<u8> {
    let lower = value.to_ascii_lowercase();
    if !lower.contains("used") || !lower.contains('%') {
        return None;
    }
    let before_percent = lower.split('%').next()?;
    let digits = before_percent
        .chars()
        .rev()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    digits.parse::<u8>().ok()
}

fn find_limit_label(names: &[String], usage_index: usize) -> Option<(String, usize)> {
    let start = usage_index.saturating_sub(12);
    for index in (start..usage_index).rev() {
        let label = match normalize_ui_label(&names[index]).as_str() {
            "current session" => "5h",
            "all models" => "All",
            "sonnet only" => "Sonnet only",
            "claude design" => "Design",
            _ => continue,
        };
        return Some((label.into(), index));
    }
    None
}

fn find_limit_reset(names: &[String], label_index: usize, usage_index: usize) -> Option<String> {
    names
        .iter()
        .take(usage_index)
        .skip(label_index + 1)
        .find_map(|name| {
            let normalized = normalize_ui_label(name);
            normalized
                .strip_prefix("resets ")
                .map(|reset| reset.trim().to_string())
        })
}

fn limits_line(entries: &[UsageLimitEntry], visibility: LimitVisibility) -> Option<String> {
    if !visibility.enabled {
        return None;
    }

    let parts = visible_limit_labels(visibility)
        .into_iter()
        .filter_map(|label| {
            entries
                .iter()
                .find(|entry| entry.label == label)
                .map(|entry| format!("{} {}%", entry.label, entry.used_percent))
        })
        .collect::<Vec<_>>();

    if parts.is_empty() {
        return None;
    }
    let count = parts.len();
    let parts = parts.join(" | ");
    Some(truncate(format!("Limits ({count}): {parts}"), 128))
}

fn visible_limit_labels(visibility: LimitVisibility) -> Vec<&'static str> {
    let mut labels = Vec::new();
    if visibility.show_5h {
        labels.push("5h");
    }
    if visibility.show_all {
        labels.push("All");
    }
    if visibility.show_sonnet {
        labels.push("Sonnet only");
    }
    if visibility.show_design {
        labels.push("Design");
    }
    labels
}

fn current_limits(
    machine: &mut StateMachine,
    detected_limits: &[UsageLimitEntry],
) -> Vec<UsageLimitEntry> {
    let now = now_ms();
    if machine.cached_limits.is_empty() {
        if let Some(cache) = read_limits_cache(now) {
            machine.cached_limits = cache.limits;
            machine.cached_limits_at_ms = cache.updated_at;
        }
    }

    if !detected_limits.is_empty() {
        machine.cached_limits = merge_limit_entries(&machine.cached_limits, detected_limits);
        machine.cached_limits_at_ms = now;
        write_limits_cache(now, &machine.cached_limits);
        return machine.cached_limits.clone();
    }

    if !machine.cached_limits.is_empty()
        && now.saturating_sub(machine.cached_limits_at_ms) <= LIMITS_CACHE_MS
    {
        return machine.cached_limits.clone();
    }

    Vec::new()
}

fn write_limits_cache(updated_at: u64, limits: &[UsageLimitEntry]) {
    let path = app_dir().join("limits-cache.json");
    write_status(
        &path,
        &json!({
            "updatedAt": updated_at,
            "limits": limits,
        }),
    );
}

fn read_limits_cache(now: u64) -> Option<LimitsCache> {
    let raw = fs::read_to_string(app_dir().join("limits-cache.json")).ok()?;
    let value: Value = serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()?;
    let updated_at = value.get("updatedAt").and_then(Value::as_u64)?;
    if now.saturating_sub(updated_at) > LIMITS_CACHE_MS {
        return None;
    }
    let limits =
        normalize_limit_entries(serde_json::from_value(value.get("limits")?.clone()).ok()?);
    Some(LimitsCache { updated_at, limits })
}

fn merge_limit_entries(
    cached: &[UsageLimitEntry],
    detected: &[UsageLimitEntry],
) -> Vec<UsageLimitEntry> {
    let mut merged = normalize_limit_entries(cached.to_vec());
    for entry in normalize_limit_entries(detected.to_vec()) {
        if let Some(existing) = merged.iter_mut().find(|item| item.label == entry.label) {
            *existing = entry;
        } else {
            merged.push(entry);
        }
    }
    sort_limit_entries(&mut merged);
    merged
}

fn normalize_limit_entries(entries: Vec<UsageLimitEntry>) -> Vec<UsageLimitEntry> {
    let mut normalized = Vec::new();
    for mut entry in entries {
        let Some(label) = normalize_limit_label(&entry.label) else {
            continue;
        };
        entry.label = label.into();
        if let Some(existing) = normalized
            .iter_mut()
            .find(|item: &&mut UsageLimitEntry| item.label == entry.label)
        {
            *existing = entry;
        } else {
            normalized.push(entry);
        }
    }
    sort_limit_entries(&mut normalized);
    normalized
}

fn normalize_limit_label(label: &str) -> Option<&'static str> {
    match normalize_ui_label(label).as_str() {
        "5h" | "session" | "current session" => Some("5h"),
        "all" | "all models" => Some("All"),
        "sonnet" | "sonnet only" | "max only" => Some("Sonnet only"),
        "design" | "claude design" => Some("Design"),
        _ => None,
    }
}

fn sort_limit_entries(entries: &mut Vec<UsageLimitEntry>) {
    entries.sort_by_key(|entry| match entry.label.as_str() {
        "5h" => 0,
        "All" => 1,
        "Sonnet only" => 2,
        "Design" => 3,
        _ => 9,
    });
}

fn write_ui_debug(names: &[String], info: &DesktopInfo) -> Option<String> {
    let path = app_dir().join("ui-debug.json");
    write_status(
        &path,
        &json!({
            "updatedAt": now_ms(),
            "mode": info.mode,
            "submode": info.submode,
            "model": info.model,
            "adaptive": info.adaptive,
            "extended": info.extended,
            "effort": info.effort,
            "limits": info.limits,
            "names": names,
        }),
    );
    Some(format!("Debug: {}", path.display()))
}

fn detect_provider() -> String {
    let settings_env = read_settings_env();
    let lookup = |key: &str| {
        std::env::var(key)
            .ok()
            .or_else(|| settings_env.get(key).cloned())
    };

    if is_truthy(lookup("CLAUDE_CODE_USE_BEDROCK").as_deref()) {
        "Amazon Bedrock".into()
    } else if is_truthy(lookup("CLAUDE_CODE_USE_VERTEX").as_deref()) {
        "Google GCP Vertex".into()
    } else if is_truthy(lookup("CLAUDE_CODE_USE_FOUNDRY").as_deref()) {
        "Microsoft Foundry".into()
    } else if lookup("ANTHROPIC_API_KEY").is_some() || lookup("CLAUDE_API_KEY").is_some() {
        "Anthropic API".into()
    } else if fs::read_to_string(claude_dir().join("config.json"))
        .map(|raw| raw.contains("\"sk-ant-"))
        .unwrap_or(false)
    {
        "Anthropic API".into()
    } else if fs::read_to_string(claude_dir().join(".credentials.json"))
        .map(|raw| raw.contains("\"claudeAiOauth\""))
        .unwrap_or(false)
    {
        "Claude Account".into()
    } else {
        "Unknown".into()
    }
}

fn read_settings_env() -> HashMap<String, String> {
    let raw = match fs::read_to_string(claude_dir().join("settings.json")) {
        Ok(raw) => raw,
        Err(_) => return HashMap::new(),
    };
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let Some(env) = value.get("env").and_then(Value::as_object) else {
        return HashMap::new();
    };
    env.iter()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
        .collect()
}

fn read_session_info() -> Option<SessionInfo> {
    let file = find_latest_jsonl_file(&claude_dir().join("projects"), 24 * 60 * 60 * 1000)?;
    let started_at_ms = read_session_start_ms(&file);
    let project_name = detect_project_name(&file);
    let model = read_session_tail(&file);
    Some(SessionInfo {
        file,
        started_at_ms,
        project_name,
        model,
    })
}

fn find_latest_jsonl_file(root: &Path, max_age_ms: u64) -> Option<PathBuf> {
    fn walk(
        dir: &Path,
        depth: usize,
        now: u64,
        max_age_ms: u64,
        best: &mut Option<(PathBuf, u64)>,
    ) {
        if depth > 3 {
            return;
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                walk(&path, depth + 1, now, max_age_ms, best);
                continue;
            }
            if !file_type.is_file()
                || path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
            {
                continue;
            }
            let Some(mtime) = modified_ms(&path) else {
                continue;
            };
            if now.saturating_sub(mtime) > max_age_ms {
                continue;
            }
            if best
                .as_ref()
                .map(|(_, best_time)| mtime > *best_time)
                .unwrap_or(true)
            {
                *best = Some((path, mtime));
            }
        }
    }

    let mut best = None;
    walk(root, 0, now_ms(), max_age_ms, &mut best);
    best.map(|(path, _)| path)
}

fn read_session_start_ms(path: &Path) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0; 8192];
    let len = file.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..len]);
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let entry: Value = serde_json::from_str(line).ok()?;
        if let Some(ts) = entry
            .get("timestamp")
            .or_else(|| {
                entry
                    .get("snapshot")
                    .and_then(|snapshot| snapshot.get("timestamp"))
            })
            .and_then(Value::as_str)
        {
            return parse_iso_ms(ts);
        }
    }
    None
}

fn read_session_tail(path: &Path) -> Option<String> {
    let lines = match read_tail_lines(path, 256 * 1024) {
        Some(lines) => lines,
        None => return None,
    };
    let mut model = None;
    for line in lines.iter().rev() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if model.is_none() {
            model = entry
                .get("message")
                .and_then(|message| message.get("model"))
                .and_then(Value::as_str)
                .and_then(format_model_name);
        }
        if model.is_some() {
            break;
        }
    }
    model
}

fn detect_project_name(session_file: &Path) -> Option<String> {
    let dir_name = session_file.parent()?.file_name()?.to_string_lossy();
    let encoded = dir_name
        .split("--claude-worktrees-")
        .next()
        .unwrap_or(&dir_name);
    let parts = encoded
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    parts
        .last()
        .map(|value| sanitize_field(Some(value), 64))
        .flatten()
}

fn read_tail_lines(path: &Path, max_bytes: u64) -> Option<Vec<String>> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let len = size.min(max_bytes);
    let offset = size.saturating_sub(len);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0; len as usize];
    file.read_exact(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if offset > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    Some(lines)
}

struct DiscordIpc {
    file: File,
    username: Option<String>,
    nonce: u64,
}

impl DiscordIpc {
    fn connect(client_id: &str) -> std::io::Result<Self> {
        let mut file = None;
        for id in 0..10 {
            let path = format!(r"\\?\pipe\discord-ipc-{id}");
            if let Ok(candidate) = OpenOptions::new().read(true).write(true).open(path) {
                file = Some(candidate);
                break;
            }
        }
        let mut client = Self {
            file: file
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "discord ipc"))?,
            username: None,
            nonce: 0,
        };
        client.send_frame(0, &json!({ "v": 1, "client_id": client_id }))?;
        let ready = client.read_frame()?;
        client.username = ready
            .get("data")
            .and_then(|data| data.get("user"))
            .and_then(|user| user.get("username"))
            .and_then(Value::as_str)
            .map(|value| sanitize_discord_user(value).unwrap_or_else(|| value.to_string()));
        Ok(client)
    }

    fn set_activity(&mut self, activity: Value) -> std::io::Result<()> {
        let nonce = self.next_nonce();
        self.send_frame(
            1,
            &json!({
                "cmd": "SET_ACTIVITY",
                "args": { "pid": std::process::id(), "activity": activity },
                "nonce": nonce,
            }),
        )?;
        self.read_response(&nonce)
    }

    fn clear_activity(&mut self) -> std::io::Result<()> {
        let nonce = self.next_nonce();
        self.send_frame(
            1,
            &json!({
                "cmd": "SET_ACTIVITY",
                "args": { "pid": std::process::id() },
                "nonce": nonce,
            }),
        )?;
        self.read_response(&nonce)
    }

    fn read_response(&mut self, nonce: &str) -> std::io::Result<()> {
        for _ in 0..4 {
            let frame = self.read_frame()?;
            if frame.get("nonce").and_then(Value::as_str) == Some(nonce) {
                if frame.get("evt").and_then(Value::as_str) == Some("ERROR") {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "discord rpc error",
                    ));
                }
                return Ok(());
            }
        }
        Ok(())
    }

    fn next_nonce(&mut self) -> String {
        self.nonce += 1;
        format!("claude-rpc-{}-{}", std::process::id(), self.nonce)
    }

    fn send_frame(&mut self, opcode: u32, payload: &Value) -> std::io::Result<()> {
        let data = serde_json::to_vec(payload)?;
        self.file.write_all(&opcode.to_le_bytes())?;
        self.file.write_all(&(data.len() as u32).to_le_bytes())?;
        self.file.write_all(&data)?;
        self.file.flush()
    }

    fn read_frame(&mut self) -> std::io::Result<Value> {
        loop {
            let mut header = [0u8; 8];
            self.file.read_exact(&mut header)?;
            let opcode = u32::from_le_bytes(header[0..4].try_into().unwrap());
            let len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            if len > 1024 * 1024 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "discord ipc frame too large",
                ));
            }
            let mut payload = vec![0u8; len];
            self.file.read_exact(&mut payload)?;
            let value: Value = serde_json::from_slice(&payload)?;
            match opcode {
                1 => return Ok(value),
                2 => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionAborted,
                        "discord closed ipc",
                    ));
                }
                3 => {
                    let _ = self.send_frame(4, &value);
                }
                4 => {}
                _ => {}
            }
        }
    }
}

fn write_status(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("txt.tmp");
    if fs::write(&tmp, value.to_string()).is_ok() {
        let _ = fs::rename(tmp, path);
    }
}

fn clear_status(path: &Path) {
    let _ = fs::remove_file(path);
}

fn format_model_name(model_id: &str) -> Option<String> {
    let id = model_id.trim().to_ascii_lowercase();
    if id.is_empty() {
        return None;
    }
    let context = if id.contains("1m") || id.contains("1m]") {
        " (1M)"
    } else {
        ""
    };
    let parts = id.split('-').collect::<Vec<_>>();
    let version = parts.windows(3).find_map(|window| {
        if ["opus", "sonnet", "haiku"].contains(&window[0])
            && window[1].chars().all(|ch| ch.is_ascii_digit())
            && window[2]
                .chars()
                .next()
                .map(|ch| ch.is_ascii_digit())
                .unwrap_or(false)
        {
            Some(format!(
                "{}.{}",
                window[1],
                window[2]
                    .chars()
                    .take_while(|ch| ch.is_ascii_digit())
                    .collect::<String>()
            ))
        } else {
            None
        }
    });
    if id.contains("opusplan") {
        return Some("Opus Plan / Sonnet 4.6".into());
    }
    if id.contains("opus") {
        return Some(format!(
            "Claude Opus {}{}",
            version.unwrap_or_else(|| "4.6".into()),
            context
        ));
    }
    if id.contains("sonnet") {
        return Some(format!(
            "Claude Sonnet {}{}",
            version.unwrap_or_else(|| "4.6".into()),
            context
        ));
    }
    if id.contains("haiku") {
        return Some(format!(
            "Claude Haiku {}{}",
            version.unwrap_or_else(|| "4.5".into()),
            context
        ));
    }
    sanitize_field(Some(model_id), 64)
}

fn map_desktop_mode(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "epitaxy" | "cowork" => Some("Cowork".into()),
        "chat" => Some("Chat".into()),
        "code" => Some("Code".into()),
        _ => None,
    }
}

fn normalize_mode(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "watching" | "tv" => "watching",
        "listening" | "listen" => "listening",
        "competing" | "compete" => "competing",
        _ => "playing",
    }
    .into()
}

fn clean_label(value: &str) -> Option<String> {
    let cleaned = value
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.chars().take(32).collect())
    }
}

fn clean_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with("http://") || value.starts_with("https://") {
        Some(value.to_string())
    } else {
        None
    }
}

fn sanitize_field(value: Option<&str>, max_len: usize) -> Option<String> {
    let cleaned = value?
        .chars()
        .filter(|ch| !ch.is_control() && !matches!(ch, '<' | '>' | '"' | '\'' | '`'))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.chars().take(max_len).collect())
    }
}

fn sanitize_discord_user(value: &str) -> Option<String> {
    sanitize_field(Some(value), 32)
}

fn is_truthy(value: Option<&str>) -> bool {
    matches!(
        value.map(|value| value.trim().to_ascii_lowercase()),
        Some(value) if value == "1" || value == "true"
    )
}

fn parse_iso_ms(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.len() < 19 {
        return None;
    }
    let year = value.get(0..4)?.parse::<i32>().ok()?;
    let month = value.get(5..7)?.parse::<u32>().ok()?;
    let day = value.get(8..10)?.parse::<u32>().ok()?;
    let hour = value.get(11..13)?.parse::<u32>().ok()?;
    let minute = value.get(14..16)?.parse::<u32>().ok()?;
    let second = value.get(17..19)?.parse::<u32>().ok()?;
    Some(datetime_to_unix_ms(year, month, day, hour, minute, second))
}

fn datetime_to_unix_ms(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> u64 {
    let days = days_from_civil(year, month as i32, day as i32);
    ((days * 86_400 + (hour as i64) * 3_600 + (minute as i64) * 60 + second as i64) * 1000) as u64
}

fn days_from_civil(year: i32, month: i32, day: i32) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = (if year >= 0 { year } else { year - 399 }) / 400;
    let yoe = year - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe - 719468) as i64
}

fn truncate(value: String, max_len: usize) -> String {
    if value.chars().count() <= max_len {
        value
    } else {
        value
            .chars()
            .take(max_len.saturating_sub(3))
            .collect::<String>()
            + "..."
    }
}

fn min_option(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn modified_ms(path: &Path) -> Option<u64> {
    let modified = path.metadata().ok()?.modified().ok()?;
    system_time_ms(modified)
}

fn now_ms() -> u64 {
    system_time_ms(SystemTime::now()).unwrap_or(0)
}

fn system_time_ms(value: SystemTime) -> Option<u64> {
    Some(value.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64)
}

fn sleep_polling(stop: &Arc<AtomicBool>, interval_ms: u64) {
    let mut slept = 0;
    while slept < interval_ms && !stop.load(Ordering::SeqCst) {
        let step = (interval_ms - slept).min(100);
        thread::sleep(Duration::from_millis(step));
        slept += step;
    }
}

fn parse_env_u64(key: &str, default: u64, min: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= min)
        .unwrap_or(default)
}

fn claude_dir() -> PathBuf {
    std::env::var("CLAUDE_DIR_PATH")
        .ok()
        .map(|value| expand_home(&value))
        .unwrap_or_else(|| home_dir().join(".claude"))
}

fn app_dir() -> PathBuf {
    std::env::var("CLAUDE_RPC_DIR")
        .ok()
        .map(|value| expand_home(&value))
        .unwrap_or_else(|| home_dir().join(".claude-rpc"))
}

fn roaming_app_data() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(home_dir)
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_rpc_modes() {
        assert_eq!(normalize_mode("watching"), "watching");
        assert_eq!(normalize_mode("tv"), "watching");
        assert_eq!(normalize_mode("unknown"), "playing");
    }

    #[test]
    fn formats_known_models() {
        assert_eq!(
            format_model_name("claude-opus-4-6").as_deref(),
            Some("Claude Opus 4.6")
        );
        assert_eq!(
            format_model_name("claude-sonnet-4-6[1m]").as_deref(),
            Some("Claude Sonnet 4.6 (1M)")
        );
    }

    #[test]
    fn parses_desktop_model_labels() {
        let candidate = parse_desktop_model_name("Claude Opus 4.7 1M Adaptive Extra high")
            .expect("desktop model");
        assert_eq!(candidate.model, "Claude Opus 4.7 (1M)");
        assert!(candidate.adaptive);
        assert_eq!(candidate.effort.as_deref(), Some("Extra high"));

        let label = format_desktop_model(&DesktopInfo {
            model: Some(candidate.model),
            adaptive: candidate.adaptive,
            effort: candidate.effort,
            ..DesktopInfo::default()
        });
        assert_eq!(
            label.as_deref(),
            Some("Claude Opus 4.7 (1M) | Adaptive | Extra high")
        );
    }

    #[test]
    fn scores_desktop_ui_modes() {
        let names = vec![
            "New task".to_string(),
            "Dispatch background conversation".to_string(),
            "Code permissions".to_string(),
        ];
        let info = desktop_info_from_ui_names(&names, Some("Chat"));
        assert_eq!(info.mode.as_deref(), Some("Cowork"));
        assert_eq!(info.submode.as_deref(), Some("Dispatch"));
    }

    #[test]
    fn parses_usage_limits() {
        let names = vec![
            "Plan usage limits".to_string(),
            "Current session".to_string(),
            "Resets in 4 hr 4 min".to_string(),
            "1% used".to_string(),
            "All models".to_string(),
            "Resets Thu 9:00 AM".to_string(),
            "18% used".to_string(),
            "Sonnet only".to_string(),
            "13% used".to_string(),
            "Claude Design".to_string(),
            "0% used".to_string(),
        ];
        let limits = parse_usage_limits(&names);
        assert_eq!(limits.len(), 4);
        assert_eq!(
            limits_line(
                &limits,
                LimitVisibility {
                    enabled: true,
                    show_5h: true,
                    show_all: true,
                    show_sonnet: true,
                    show_design: true,
                }
            )
            .as_deref(),
            Some("Limits (4): 5h 1% | All 18% | Sonnet only 13% | Design 0%")
        );
        assert_eq!(
            limits_line(
                &limits,
                LimitVisibility {
                    enabled: true,
                    show_5h: false,
                    show_all: true,
                    show_sonnet: false,
                    show_design: true,
                }
            )
            .as_deref(),
            Some("Limits (2): All 18% | Design 0%")
        );
        assert_eq!(
            limits_line(
                &[],
                LimitVisibility {
                    enabled: true,
                    show_5h: true,
                    show_all: false,
                    show_sonnet: false,
                    show_design: false,
                }
            )
            .as_deref(),
            None
        );
        assert_eq!(limits[0].reset.as_deref(), Some("in 4 hr 4 min"));
    }

    #[test]
    fn merges_and_normalizes_limit_cache() {
        let cached = vec![
            UsageLimitEntry {
                label: "session".into(),
                used_percent: 2,
                reset: None,
            },
            UsageLimitEntry {
                label: "all".into(),
                used_percent: 18,
                reset: None,
            },
        ];
        let detected = vec![UsageLimitEntry {
            label: "5h".into(),
            used_percent: 3,
            reset: None,
        }];

        let limits = merge_limit_entries(&cached, &detected);
        assert_eq!(
            limits_line(
                &limits,
                LimitVisibility {
                    enabled: true,
                    show_5h: true,
                    show_all: true,
                    show_sonnet: true,
                    show_design: true,
                }
            )
            .as_deref(),
            Some("Limits (2): 5h 3% | All 18%")
        );
    }

    #[test]
    fn formats_rpc_state_visibility() {
        let result = DetectionResult {
            client: ClientType::Desktop,
            model: Some("Claude Opus 4.7 (1M) | Extra high".into()),
            provider: "Anthropic API".into(),
            limits_line: Some("Limits (1): 5h 3%".into()),
            ..DetectionResult::default()
        };
        let mut config = ClaudeConfig::default();

        assert_eq!(
            build_state(&result, &config),
            "Claude Opus 4.7 (1M) | Extra high | Anthropic API | Limits (1): 5h 3%"
        );

        config.show_provider = false;
        assert_eq!(
            build_state(&result, &config),
            "Claude Opus 4.7 (1M) | Extra high | Limits (1): 5h 3%"
        );

        config.show_effort = false;
        assert_eq!(
            build_state(&result, &config),
            "Claude Opus 4.7 (1M) | Limits (1): 5h 3%"
        );
    }

    #[test]
    fn parses_iso_timestamps_as_utc() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:01Z"), Some(1000));
    }
}
