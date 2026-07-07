mod ipc;
mod usage;

pub(crate) use ipc::*;
pub(crate) use usage::*;

use crate::config::{normalize_mode, read_config, ClaudeConfig};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
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
const SCAN_INTERVAL_MS: u64 = 250;
const RPC_REFRESH_INTERVAL_MS: u64 = 15_000;
const IDLE_GRACE_MS: u64 = 10_000;
const ACTIVITY_PLAYING: u8 = 0;
const ACTIVITY_LISTENING: u8 = 2;
const ACTIVITY_WATCHING: u8 = 3;
const ACTIVITY_COMPETING: u8 = 5;

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
    session_title: Option<String>,
    code_instances: usize,
    started_at_ms: Option<u64>,
    cost_line: Option<String>,
    model_costs_all: Vec<ModelCost>,
    model_costs_current: Vec<ModelCost>,
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
            session_title: None,
            code_instances: 0,
            started_at_ms: None,
            cost_line: None,
            model_costs_all: Vec::new(),
            model_costs_current: Vec::new(),
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
    session_title: Option<String>,
    model: Option<String>,
    cwd: Option<String>,
}

#[derive(Default)]
pub(crate) struct StateMachine {
    last_non_idle: Option<DetectionResult>,
    last_non_idle_at_ms: u64,
    cached_limits: Vec<UsageLimitEntry>,
    oauth_last_attempt_ms: u64,
    oauth_backoff_until_ms: u64,
    last_session_mtime: u64,
    pending_activity_refresh: bool,
    cached_code_model: Option<String>,
    cached_code_model_session: Option<PathBuf>,
    cached_code_effort: Option<String>,
    cached_code_effort_session: Option<PathBuf>,
    cached_project_usages: Option<Vec<ProjectUsage>>,
    cached_project_usages_mtime: u64,
    cached_session_costs: Option<Vec<ModelCost>>,
    cached_session_costs_file: Option<PathBuf>,
    cached_session_costs_mtime: u64,
}

#[cfg(windows)]
struct ProcessEntry {
    process_id: u32,
    name: String,
}

#[cfg(target_os = "macos")]
struct MacProcessEntry {
    process_id: u32,
    name: String,
    command: String,
}

#[derive(Debug, Clone)]
struct ProcessSnapshot {
    process_id: u32,
    name: String,
    executable_path: Option<String>,
    creation_date_ms: Option<u64>,
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone)]
struct DesktopModelCandidate {
    model: String,
    adaptive: bool,
    extended: bool,
    effort: Option<String>,
}

pub fn run(
    stop: Arc<AtomicBool>,
    force_refresh: Arc<AtomicBool>,
    config_path: Option<PathBuf>,
    status_path: Option<PathBuf>,
) {
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
        cost_enabled(&config),
    );
    let mut last_scan_at = 0;

    while !stop.load(Ordering::SeqCst) {
        // UI "Refresh limits" button: bypass the OAuth poll interval/backoff and
        // force an immediate usage re-fetch on this iteration.
        if force_refresh.swap(false, Ordering::SeqCst) {
            machine.oauth_last_attempt_ms = 0;
            machine.oauth_backoff_until_ms = 0;
            machine.pending_activity_refresh = true;
            last_scan_at = 0;
        }

        let current_modified = modified_ms(&config_path);
        if current_modified != config_modified {
            config_modified = current_modified;
            config = read_config(&config_path);
            result = detect(
                &mut machine,
                idle_grace_ms,
                config.verbose,
                limit_visibility(&config),
                cost_enabled(&config),
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
                cost_enabled(&config),
            );
            last_scan_at = now;
        }

        write_status(
            &status_path,
            &build_status(
                &result,
                ipc.as_ref().and_then(|client| client.username.as_deref()),
                &config,
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
    cost_enabled: bool,
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

    // Any live Claude client (Desktop included, even with no churning CLI session
    // file) counts as activity, so OAuth usage polls at the 60s cadence instead of
    // the 10-minute idle floor and the displayed percentages stay fresh.
    if client != ClientType::Idle {
        machine.pending_activity_refresh = true;
    }

    let desktop = if client == ClientType::Desktop {
        read_desktop_info(&desktop_process_ids, verbose)
    } else {
        DesktopInfo::default()
    };
    if let Some(session) = &session {
        if let Some(mtime) = modified_ms(&session.file) {
            if machine.last_session_mtime != 0 && mtime != machine.last_session_mtime {
                machine.pending_activity_refresh = true;
            }
            machine.last_session_mtime = mtime;
        }
    }
    let limits = current_limits(machine, &desktop.limits, verbose);
    let limits_line = if limit_visibility.enabled {
        limits_line(&limits, limit_visibility)
    } else {
        None
    };
    let desktop_model = if client == ClientType::Desktop {
        detect_desktop_model(&desktop, session.as_ref())
    } else {
        None
    };

    let mut result = DetectionResult {
        client,
        mode: desktop.mode,
        submode: desktop.submode,
        model: match client {
            ClientType::Desktop => desktop_model,
            ClientType::Code => resolve_code_model(machine, session.as_ref()),
            ClientType::Idle => None,
        },
        limits_line,
        debug_line: desktop.debug_line,
        provider: detect_provider(),
        project_name: session
            .as_ref()
            .and_then(|session| session.project_name.clone()),
        session_title: session
            .as_ref()
            .and_then(|session| session.session_title.clone()),
        code_instances: code_count,
        started_at_ms: oldest
            .or_else(|| session.as_ref().and_then(|session| session.started_at_ms)),
        cost_line: None,
        model_costs_all: Vec::new(),
        model_costs_current: Vec::new(),
    };

    if client == ClientType::Code {
        let effort = resolve_code_effort(machine, session.as_ref());
        result.model = append_code_effort(result.model, effort);
    }

    if cost_enabled && result.client != ClientType::Idle {
        // All-projects cumulative from each project's last completed session,
        // plus the current project's own stored last session (`current_stored`),
        // which fold_live_session drops once the live session supersedes it.
        let (all, current_stored) = {
            let cwd = session.as_ref().and_then(|session| session.cwd.as_deref());
            let usages = project_usages(machine);
            aggregate_costs(usages, cwd)
        };
        // Current = the live, in-progress session straight from its .jsonl, since
        // ~/.claude.json holds no usage for a running session.
        let current = session
            .as_ref()
            .map(|session| session_costs(machine, &session.file))
            .unwrap_or_default();
        result.cost_line = build_cost_line(&current);
        result.model_costs_all = fold_live_session(all, &current_stored, &current);
        result.model_costs_current = current;
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
    if result.client == ClientType::Idle && !config.show_idle {
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
        "name": "Clawd",
        "type": activity_type,
        "created_at": now_ms(),
        "instance": false,
        "details": build_details(result, &mode, config),
        "state": build_state(result, config),
        "assets": {
            "large_image": logo_image(),
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

fn build_details(result: &DetectionResult, mode: &str, config: &ClaudeConfig) -> String {
    let base = match (result.client, mode) {
        (ClientType::Desktop, "watching") => "Watching Claude",
        (ClientType::Code, "watching") => "Watching Claude Code",
        (ClientType::Desktop, _) => "Claude Desktop",
        (ClientType::Code, _) => "Claude Code",
        (ClientType::Idle, _) => "Claude",
    };

    if result.client == ClientType::Desktop {
        if let Some(mode_label) = desktop_mode_label(result) {
            return format!("{base} ({mode_label})");
        }
    }

    if result.client == ClientType::Code && config.show_session_title {
        if let Some(title) = sanitize_field(result.session_title.as_deref(), 64) {
            let candidate = format!("{base} - {title}");
            if candidate.len() <= 96 {
                return candidate;
            }
            if base.len() + 5 < 96 {
                let budget = 96 - base.len() - 5;
                let trimmed: String = title.chars().take(budget).collect();
                return format!("{base} - {trimmed}…");
            }
        }
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
    if config.show_cost {
        if let Some(cost) = result.cost_line.as_deref() {
            parts.push(cost.to_string());
        }
    }
    if config.show_project_tokens {
        if let Some(tokens) = build_project_tokens_line(&result.model_costs_current) {
            parts.push(tokens);
        }
    }
    if config.show_cost_total {
        if let Some(total) = build_total_line(&result.model_costs_all) {
            parts.push(total);
        }
    }
    if config.show_all_tokens {
        if let Some(tokens) = build_all_tokens_line(&result.model_costs_all) {
            parts.push(tokens);
        }
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
        "low" | "medium" | "high" | "extra high" | "xhigh" | "max" | "ultracode"
    )
}

fn small_image_text(result: &DetectionResult) -> String {
    match result.client {
        ClientType::Desktop => "Claude Desktop".into(),
        ClientType::Code => "Claude Code CLI".into(),
        ClientType::Idle => "Claude".into(),
    }
}

fn logo_image() -> String {
    "https://raw.githubusercontent.com/stealthsrc/claude-rpc/main/logo/clawd.png".into()
}

fn activity_verb(mode: &str) -> &'static str {
    match mode {
        "watching" => "Watching",
        "listening" => "Listening to",
        "competing" => "Competing in",
        _ => "Playing",
    }
}

fn build_status(
    result: &DetectionResult,
    discord_user: Option<&str>,
    config: &ClaudeConfig,
) -> Value {
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

    // Mirror Discord's per-activity-type card layout:
    // - Playing (type 0): header is just the verb; the `name` ("Clawd")
    //   takes the bold line, then details, then state.
    // - Watching/Listening/Competing: header is "{verb} {name}", then details
    //   (bold), then state, then large_text ("Powered by Anthropic").
    let (preview_header, preview_primary, preview_secondary, preview_tertiary) =
        if result.client == ClientType::Idle && !config.show_idle {
            (None, None, None, None)
        } else {
            let mode = normalize_mode(&config.rpc_mode);
            let verb = activity_verb(&mode);
            let details = build_details(result, &mode, config);
            let state = build_state(result, config);
            if mode == "playing" {
                (
                    Some("Playing".to_string()),
                    Some("Clawd".to_string()),
                    Some(details),
                    Some(state),
                )
            } else {
                (
                    Some(format!("{verb} Clawd")),
                    Some(details),
                    Some(state),
                    Some("Powered by Anthropic".to_string()),
                )
            }
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
        "previewHeader": preview_header,
        "previewPrimary": preview_primary,
        "previewSecondary": preview_secondary,
        "previewTertiary": preview_tertiary,
        "costShown": config.show_cost,
        "costLine": result.cost_line.clone(),
        "costTotalShown": config.show_cost_total,
        "costTotalLine": build_total_line(&result.model_costs_all),
        "projectTokensShown": config.show_project_tokens,
        "projectTokensLine": build_project_tokens_line(&result.model_costs_current),
        "allTokensShown": config.show_all_tokens,
        "allTokensLine": build_all_tokens_line(&result.model_costs_all),
        "modelCostsAll": result.model_costs_all,
        "modelCostsCurrent": result.model_costs_current,
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
        "showLimits": config.show_limits,
        "showLimit5h": config.show_limit_5h,
        "showLimitAll": config.show_limit_all,
        "showLimitSonnet": config.show_limit_sonnet,
        "showProvider": config.show_provider,
        "showEffort": config.show_effort,
        "showSessionTitle": config.show_session_title,
        "showCost": config.show_cost,
        "cost": result.cost_line,
        "showCostTotal": config.show_cost_total,
        "costTotal": build_total_line(&result.model_costs_all),
        "showProjectTokens": config.show_project_tokens,
        "projectTokens": build_project_tokens_line(&result.model_costs_current),
        "showAllTokens": config.show_all_tokens,
        "allTokens": build_all_tokens_line(&result.model_costs_all),
        "showIdle": config.show_idle,
        "buttons": config.buttons,
    }))
    .unwrap_or_default()
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

#[cfg(target_os = "macos")]
fn scan_claude_processes() -> Vec<ProcessSnapshot> {
    list_macos_process_entries()
        .into_iter()
        .filter(|entry| is_macos_claude_candidate(&entry.name, &entry.command))
        .map(|entry| ProcessSnapshot {
            process_id: entry.process_id,
            name: entry.name,
            executable_path: Some(entry.command),
            creation_date_ms: None,
        })
        .collect()
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn scan_claude_processes() -> Vec<ProcessSnapshot> {
    Vec::new()
}

fn is_desktop_process(process: &ProcessSnapshot) -> bool {
    let exe = process
        .executable_path
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let exe_unix = exe.replace('\\', "/");
    process.name.eq_ignore_ascii_case("claude desktop.exe")
        || process.name.eq_ignore_ascii_case("claude desktop")
        || (process.name.eq_ignore_ascii_case("claude")
            && exe_unix.contains(".app/contents/macos/"))
        || exe.contains("windowsapps")
        || exe.contains("anthropicclaude")
        || exe.contains("\\program files\\claude")
        || exe.contains("\\appdata\\local\\anthropic")
        || exe_unix.contains("/applications/claude.app/")
        || exe_unix.contains("/claude.app/contents/macos/")
        || exe_unix.contains("/library/application support/claude/")
}

fn is_code_process(process: &ProcessSnapshot) -> bool {
    let exe = process
        .executable_path
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase()
        .replace('\\', "/");
    (process.name.eq_ignore_ascii_case("claude.exe")
        || process.name.eq_ignore_ascii_case("claude")
        || exe.contains("/node_modules/@anthropic-ai/claude-code/")
        || exe.contains("/node_modules/claude-code/")
        || exe.contains("/.claude/local/")
        || exe.contains("/claude-code/"))
        && !is_desktop_process(process)
}

#[cfg(windows)]
fn list_process_entries() -> Vec<ProcessEntry> {
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

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

#[cfg(target_os = "macos")]
fn list_macos_process_entries() -> Vec<MacProcessEntry> {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,comm=,command="])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_macos_process_line)
        .collect()
}

#[cfg(target_os = "macos")]
fn parse_macos_process_line(line: &str) -> Option<MacProcessEntry> {
    let (process_id, rest) = split_process_field(line)?;
    let (comm, rest) = split_process_field(rest)?;
    let process_id = process_id.parse().ok()?;
    let command = rest.trim_start().to_string();
    if command.is_empty() {
        return None;
    }
    Some(MacProcessEntry {
        process_id,
        name: command_basename(comm),
        command,
    })
}

#[cfg(target_os = "macos")]
fn split_process_field(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    if input.is_empty() {
        return None;
    }
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    Some((&input[..end], &input[end..]))
}

#[cfg(target_os = "macos")]
fn is_macos_claude_candidate(name: &str, command: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let command = command.to_ascii_lowercase();
    if command.contains("claude-rpc") {
        return false;
    }
    name == "claude"
        || name == "claude desktop"
        || command.contains("/applications/claude.app/")
        || command.contains("/claude.app/contents/macos/")
        || command.contains("/node_modules/@anthropic-ai/claude-code/")
        || command.contains("/node_modules/claude-code/")
        || command.contains("/.claude/local/")
        || command.contains("/claude-code/")
}

#[cfg(target_os = "macos")]
fn command_basename(command: &str) -> String {
    let executable = command.split_whitespace().next().unwrap_or(command);
    executable
        .trim_matches('"')
        .trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(executable)
        .to_ascii_lowercase()
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

fn detect_desktop_model(info: &DesktopInfo, session: Option<&SessionInfo>) -> Option<String> {
    format_desktop_model(info)
        .or_else(|| read_platform_desktop_model(info.mode.as_deref(), session))
        .or_else(read_settings_model)
        .or_else(|| {
            std::env::var("CLAUDE_MODEL")
                .ok()
                .and_then(|v| format_model_name(&v))
        })
}

#[cfg(target_os = "macos")]
fn read_platform_desktop_model(
    mode: Option<&str>,
    session: Option<&SessionInfo>,
) -> Option<String> {
    match mode {
        Some("Code") => append_desktop_effort(
            read_sticky_model_selector().or_else(|| detect_code_model(session)),
        ),
        Some("Cowork") => read_cowork_sticky_model_selector()
            .or_else(read_sticky_model_selector)
            .or_else(read_latest_local_agent_model),
        Some("Chat") => read_sticky_model_selector(),
        _ => read_sticky_model_selector()
            .or_else(read_latest_local_agent_model)
            .or_else(|| append_code_effort(detect_code_model(session))),
    }
}

#[cfg(not(target_os = "macos"))]
fn read_platform_desktop_model(
    _mode: Option<&str>,
    _session: Option<&SessionInfo>,
) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn read_sticky_model_selector() -> Option<String> {
    read_desktop_local_storage_value(parse_sticky_model_selector_text)
}

#[cfg(target_os = "macos")]
fn read_cowork_sticky_model_selector() -> Option<String> {
    read_desktop_local_storage_value(parse_cowork_model_selector_text)
}

#[cfg(target_os = "macos")]
fn append_desktop_effort(model: Option<String>) -> Option<String> {
    let mut model = model?;
    if let Some(effort) = read_desktop_effort_level() {
        append_unique_label(&mut model, true, &effort);
    }
    Some(model)
}

#[cfg(target_os = "macos")]
fn read_desktop_effort_level() -> Option<String> {
    read_desktop_local_storage_value(parse_desktop_effort_text)
}

#[cfg(target_os = "macos")]
fn read_desktop_local_storage_value(parser: fn(&str) -> Option<String>) -> Option<String> {
    let dir = roaming_app_data()
        .join("Claude")
        .join("Local Storage")
        .join("leveldb");
    let mut files = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let ext = path.extension().and_then(|value| value.to_str())?;
                if !matches!(ext, "ldb" | "log") {
                    return None;
                }
                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .unwrap_or(UNIX_EPOCH);
                Some((modified, path))
            })
            .collect::<Vec<_>>(),
        Err(_) => return None,
    };
    files.sort_by(|left, right| right.0.cmp(&left.0));

    for (_, path) in files {
        let Ok(raw) = fs::read(&path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&raw);
        if let Some(value) = parser(&text) {
            return Some(value);
        }
    }
    None
}

#[cfg(any(target_os = "macos", test))]
fn parse_sticky_model_selector_text(raw: &str) -> Option<String> {
    let mut model = None;
    for marker in [
        "sticky-model-selector",
        "ticky-model-selector",
        "sticky-model-",
    ] {
        let mut offset = 0usize;
        while let Some(index) = raw[offset..].find(marker) {
            let start = offset + index + marker.len();
            let end = (start + 512).min(raw.len());
            if let Some(candidate) = extract_first_model_id(&raw[start..end]) {
                model = Some(append_desktop_thinking_labels(candidate, &raw[start..end]));
            }
            offset = start;
        }
    }
    model
}

#[cfg(any(target_os = "macos", test))]
fn parse_cowork_model_selector_text(raw: &str) -> Option<String> {
    let mut model = None;
    for marker in [
        "cowork-sticky-model-selector",
        "owork-sticky-model-selector",
    ] {
        let mut offset = 0usize;
        while let Some(index) = raw[offset..].find(marker) {
            let start = offset + index + marker.len();
            let end = (start + 512).min(raw.len());
            if let Some(candidate) = extract_first_model_id(&raw[start..end]) {
                model = Some(append_desktop_thinking_labels(candidate, &raw[start..end]));
            }
            offset = start;
        }
    }
    model
}

#[cfg(any(target_os = "macos", test))]
fn parse_desktop_effort_text(raw: &str) -> Option<String> {
    let marker = "ccd-effort-level";
    let mut offset = 0usize;
    let mut effort = None;
    while let Some(index) = raw[offset..].find(marker) {
        let start = offset + index + marker.len();
        let end = (start + 128).min(raw.len());
        if let Some(value) = extract_effort_label(&raw[start..end].to_ascii_lowercase()) {
            effort = Some(value);
        }
        offset = start;
    }
    effort
}

#[cfg(any(target_os = "macos", test))]
fn append_desktop_thinking_labels(mut model: String, raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    append_unique_label(&mut model, lower.contains("adaptive"), "Adaptive");
    append_unique_label(&mut model, lower.contains("extended"), "Extended");
    model
}

#[cfg(any(target_os = "macos", test))]
fn extract_first_model_id(raw: &str) -> Option<String> {
    let (start, needs_prefix) = find_model_token_start(raw)?;
    let tail = &raw[start..];
    let id = tail
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '[' | ']'))
        .collect::<String>();
    let id = if needs_prefix {
        format!("claude-{id}")
    } else {
        id
    };
    format_model_name(&id)
}

#[cfg(any(target_os = "macos", test))]
fn find_model_token_start(raw: &str) -> Option<(usize, bool)> {
    let mut best: Option<(usize, bool)> = None;
    for (needle, needs_prefix) in [
        ("claude-", false),
        ("opus-", true),
        ("sonnet-", true),
        ("haiku-", true),
    ] {
        if let Some(index) = raw.find(needle) {
            if best
                .as_ref()
                .map(|(best_index, _)| index < *best_index)
                .unwrap_or(true)
            {
                best = Some((index, needs_prefix));
            }
        }
    }
    best
}

#[cfg(target_os = "macos")]
fn read_latest_local_agent_model() -> Option<String> {
    let root = roaming_app_data()
        .join("Claude")
        .join("local-agent-mode-sessions");
    let mut stack = vec![root];
    let mut best: Option<(u64, String)> = None;
    let mut visited = 0usize;

    while let Some(dir) = stack.pop() {
        visited += 1;
        if visited > 10_000 {
            break;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }

            let is_local_session = path
                .file_name()
                .and_then(|value| value.to_str())
                .map(|name| name.starts_with("local_") && name.ends_with(".json"))
                .unwrap_or(false);
            if !is_local_session {
                continue;
            }

            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            let Some(model) = desktop_model_from_local_agent_session(&raw) else {
                continue;
            };
            let score = local_agent_session_timestamp(&raw).unwrap_or_else(|| {
                entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_millis() as u64)
                    .unwrap_or(0)
            });
            if best
                .as_ref()
                .map(|(best_score, _)| score > *best_score)
                .unwrap_or(true)
            {
                best = Some((score, model));
            }
        }
    }

    best.map(|(_, model)| model)
}

#[cfg(any(target_os = "macos", test))]
fn desktop_model_from_local_agent_session(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw)
        .ok()?
        .get("model")
        .and_then(Value::as_str)
        .and_then(format_model_name)
}

#[cfg(target_os = "macos")]
fn local_agent_session_timestamp(raw: &str) -> Option<u64> {
    let value: Value = serde_json::from_str(raw).ok()?;
    value
        .get("lastActivityAt")
        .or_else(|| value.get("createdAt"))
        .and_then(Value::as_u64)
}

// Hold the last model detected for a session so the RPC doesn't fall back to
// "Auto-detect" when the assistant entry scrolls past the tail-read window
// (e.g. after large image attachments) or before the first reply lands.
fn resolve_code_model(machine: &mut StateMachine, session: Option<&SessionInfo>) -> Option<String> {
    let session_file = session.map(|session| session.file.clone());
    if machine.cached_code_model_session != session_file {
        machine.cached_code_model = None;
        machine.cached_code_model_session = session_file;
    }
    if let Some(model) = detect_code_model(session) {
        machine.cached_code_model = Some(model.clone());
        return Some(model);
    }
    machine.cached_code_model.clone()
}

fn detect_code_model(session: Option<&SessionInfo>) -> Option<String> {
    session
        .and_then(|session| session.model.clone())
        .or_else(read_settings_model)
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
        .or_else(|| {
            session
                .and_then(|session| session.cwd.as_deref())
                .and_then(read_recent_project_model)
        })
}

// Last-resort guess for a brand-new session with no assistant reply yet:
// the model that did the most work in this project's previous session,
// recorded in ~/.claude.json under projects.<cwd>.lastModelUsage.
fn read_recent_project_model(cwd: &str) -> Option<String> {
    let raw = fs::read_to_string(home_dir().join(".claude.json")).ok()?;
    let value: Value = serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()?;
    let projects = value.get("projects")?.as_object()?;
    let target = cwd.replace('\\', "/");
    let project = projects
        .iter()
        .find(|(key, _)| key.replace('\\', "/").eq_ignore_ascii_case(&target))
        .map(|(_, value)| value)?;
    let usage = project.get("lastModelUsage")?.as_object()?;
    usage
        .iter()
        .filter(|(id, _)| !id.to_ascii_lowercase().contains("haiku"))
        .max_by_key(|(_, value)| {
            value
                .get("outputTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        })
        .and_then(|(id, _)| format_model_name(id))
}

fn read_settings_model() -> Option<String> {
    let raw = fs::read_to_string(claude_dir().join("settings.json")).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value
        .get("model")
        .and_then(Value::as_str)
        .and_then(format_model_name)
}

fn append_code_effort(model: Option<String>, effort: Option<String>) -> Option<String> {
    let model = model?;
    let effort = effort?;
    if model
        .to_ascii_lowercase()
        .contains(&effort.to_ascii_lowercase())
    {
        Some(model)
    } else {
        Some(format!("{model} | {effort}"))
    }
}

// A `/effort` "this session only" override (recorded in the session log) wins over
// the persisted settings.json effortLevel. It is the ONLY signal for the
// "ultracode" tier, which leaves effortLevel at "xhigh" (it is xhigh + workflow
// orchestration) and would otherwise display as "Extra high". Cached per-session
// so it survives the override scrolling past the tail-read window.
fn resolve_code_effort(
    machine: &mut StateMachine,
    session: Option<&SessionInfo>,
) -> Option<String> {
    let session_file = session.map(|session| session.file.clone());
    if machine.cached_code_effort_session != session_file {
        machine.cached_code_effort = None;
        machine.cached_code_effort_session = session_file;
    }
    if let Some(effort) = read_session_effort_override(session) {
        machine.cached_code_effort = Some(effort.clone());
        return Some(effort);
    }
    machine
        .cached_code_effort
        .clone()
        .or_else(read_settings_effort)
}

fn read_settings_effort() -> Option<String> {
    let raw = fs::read_to_string(claude_dir().join("settings.json")).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    effort_label(value.get("effortLevel").and_then(Value::as_str)?)
}

fn read_session_effort_override(session: Option<&SessionInfo>) -> Option<String> {
    let lines = read_tail_lines(&session?.file, 256 * 1024)?;
    for line in lines.iter().rev() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if is_sidechain_entry(&entry) {
            continue;
        }
        if let Some(effort) = read_command_effort(&entry) {
            return Some(effort);
        }
    }
    None
}

fn read_command_effort(entry: &Value) -> Option<String> {
    let content = entry.get("message")?.get("content")?;
    if let Some(text) = content.as_str() {
        return parse_command_effort_text(text);
    }
    content.as_array()?.iter().find_map(|item| {
        item.get("text")
            .and_then(Value::as_str)
            .and_then(parse_command_effort_text)
    })
}

fn parse_command_effort_text(text: &str) -> Option<String> {
    // Only trust actual /effort command output, not assistant prose that may
    // mention the phrase.
    if !text.contains("command-stdout") {
        return None;
    }
    let cleaned = strip_ansi(text)
        .replace("<local-command-stdout>", " ")
        .replace("</local-command-stdout>", " ");
    let marker = "set effort level to ";
    let lower = cleaned.to_ascii_lowercase();
    let start = lower.find(marker)? + marker.len();
    let rest = cleaned[start..].lines().next().unwrap_or_default();
    let word = rest.split(['(', ':']).next().unwrap_or(rest).trim();
    effort_label(word)
}

fn effort_label(raw: &str) -> Option<String> {
    Some(
        match raw.trim().to_ascii_lowercase().as_str() {
            "low" => "Low",
            "medium" => "Medium",
            "high" => "High",
            "xhigh" | "extrahigh" | "extra high" => "Extra high",
            "max" => "Max",
            "ultracode" => "Ultracode",
            _ => return None,
        }
        .into(),
    )
}

#[cfg(any(windows, test))]
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
    ranked.sort_by_key(|(_, score)| std::cmp::Reverse(*score));

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

#[cfg(any(windows, test))]
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
        } else if lower.contains("fable") {
            "Fable"
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

#[cfg(any(windows, test))]
fn default_model_version(family: &str) -> String {
    match family {
        "Haiku" => "4.5",
        "Fable" => "5",
        _ => "4.6",
    }
    .into()
}

fn extract_effort_label(lower: &str) -> Option<String> {
    if lower.contains("ultracode") {
        Some("Ultracode".into())
    } else if lower.contains("extra high") || lower.contains("xhigh") {
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

#[cfg(windows)]
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
    let has_value = |key: &str| is_present(lookup(key).as_deref());

    if is_truthy(lookup("CLAUDE_CODE_USE_BEDROCK").as_deref())
        || has_value("ANTHROPIC_BEDROCK_BASE_URL")
        || has_value("AWS_BEARER_TOKEN_BEDROCK")
    {
        "Amazon Bedrock".into()
    } else if is_truthy(lookup("CLAUDE_CODE_USE_VERTEX").as_deref())
        || has_value("ANTHROPIC_VERTEX_PROJECT_ID")
    {
        "Google GCP Vertex".into()
    } else if is_truthy(lookup("CLAUDE_CODE_USE_FOUNDRY").as_deref()) {
        "Microsoft Foundry".into()
    } else if has_value("ANTHROPIC_API_KEY") || has_value("CLAUDE_API_KEY") {
        "Anthropic API".into()
    } else {
        let config_texts = claude_config_texts();
        if config_texts.iter().any(|raw| has_anthropic_api_auth(raw)) {
            "Anthropic API".into()
        } else if config_texts.iter().any(|raw| has_claude_account_auth(raw)) {
            "Claude Account".into()
        } else {
            "Unknown".into()
        }
    }
}

fn is_present(value: Option<&str>) -> bool {
    value.map(|value| !value.trim().is_empty()).unwrap_or(false)
}

fn read_settings_env() -> HashMap<String, String> {
    let mut result = HashMap::new();
    read_settings_env_file(&claude_dir().join("settings.json"), &mut result);
    read_settings_env_file(&claude_dir().join("settings.local.json"), &mut result);
    result
}

fn read_settings_env_file(path: &Path, result: &mut HashMap<String, String>) {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return,
    };
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let Some(env) = value.get("env").and_then(Value::as_object) else {
        return;
    };
    result.extend(
        env.iter().filter_map(|(key, value)| {
            value.as_str().map(|value| (key.clone(), value.to_string()))
        }),
    );
}

fn claude_config_texts() -> Vec<String> {
    let mut paths = vec![
        claude_dir().join("config.json"),
        claude_dir().join(".credentials.json"),
        claude_dir().join("settings.json"),
        claude_dir().join("settings.local.json"),
    ];
    paths.push(home_dir().join(".claude.json"));

    paths
        .into_iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .collect()
}

fn has_anthropic_api_auth(raw: &str) -> bool {
    raw.contains("sk-ant-") || raw.contains("\"apiKeyHelper\"")
}

fn has_claude_account_auth(raw: &str) -> bool {
    raw.contains("\"claudeAiOauth\"")
        || raw.contains("\"oauthAccount\"")
        || raw.contains("\"CLAUDE_CODE_OAUTH_TOKEN\"")
}

fn read_session_info() -> Option<SessionInfo> {
    let file = find_latest_jsonl_file(&claude_dir().join("projects"), 24 * 60 * 60 * 1000)?;
    let started_at_ms = read_session_start_ms(&file);
    let project_name = detect_project_name(&file);
    let session_title = read_session_title(&file);
    let model = read_session_tail(&file);
    let cwd = read_session_cwd(&file);
    Some(SessionInfo {
        file,
        started_at_ms,
        project_name,
        session_title,
        model,
        cwd,
    })
}

fn read_session_title(path: &Path) -> Option<String> {
    let lines = read_tail_lines(path, 256 * 1024)?;
    for line in lines.iter().rev() {
        if !line.contains("\"ai-title\"") {
            continue;
        }
        let entry: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if entry.get("type").and_then(Value::as_str) != Some("ai-title") {
            continue;
        }
        if let Some(title) = entry.get("aiTitle").and_then(Value::as_str) {
            return sanitize_field(Some(title), 96);
        }
    }
    None
}

fn find_latest_jsonl_file(root: &Path, max_age_ms: u64) -> Option<PathBuf> {
    let mut candidates = collect_jsonl_candidates(root, max_age_ms);
    candidates.sort_by_key(|(_, mtime)| std::cmp::Reverse(*mtime));
    candidates
        .into_iter()
        .find(|(path, _)| is_user_session_file(path))
        .map(|(path, _)| path)
}

fn collect_jsonl_candidates(root: &Path, max_age_ms: u64) -> Vec<(PathBuf, u64)> {
    fn walk(dir: &Path, depth: usize, now: u64, max_age_ms: u64, out: &mut Vec<(PathBuf, u64)>) {
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
                walk(&path, depth + 1, now, max_age_ms, out);
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
            out.push((path, mtime));
        }
    }

    let mut out = Vec::new();
    walk(root, 0, now_ms(), max_age_ms, &mut out);
    out
}

fn is_user_session_file(path: &Path) -> bool {
    match read_session_cwd(path) {
        Some(cwd) => is_user_project_cwd(&cwd),
        // No cwd readable yet (very fresh file) — assume valid; tail-based detection will refine
        None => true,
    }
}

fn read_session_cwd(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0; 32 * 1024];
    let len = file.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..len]);
    for line in text.lines().take(20) {
        let entry: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if let Some(cwd) = entry.get("cwd").and_then(Value::as_str) {
            return Some(cwd.to_string());
        }
    }
    None
}

fn is_user_project_cwd(cwd: &str) -> bool {
    // Reject sessions whose cwd lives inside a hidden directory (e.g. C:\Users\x\.claude-mem\...)
    // Background subagents/observers run from these; real Claude Code sessions don't.
    cwd.split(['/', '\\'])
        .filter(|seg| !seg.is_empty())
        .all(|seg| !seg.starts_with('.') || seg.chars().all(|c| c == '.'))
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
    let lines = read_tail_lines(path, 256 * 1024)?;
    // Pass 1: latest "/model" command anywhere — user-set model wins
    for line in lines.iter().rev() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if is_sidechain_entry(&entry) {
            continue;
        }
        if let Some(model) = read_command_model(&entry) {
            return Some(model);
        }
    }
    // Pass 2: latest assistant message.model from the main thread (skip sidechains)
    for line in lines.iter().rev() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if is_sidechain_entry(&entry) {
            continue;
        }
        if entry.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if let Some(model) = entry
            .get("message")
            .and_then(|message| message.get("model"))
            .and_then(Value::as_str)
            .and_then(format_model_name)
        {
            return Some(model);
        }
    }
    None
}

fn is_sidechain_entry(entry: &Value) -> bool {
    entry
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn read_command_model(entry: &Value) -> Option<String> {
    let content = entry.get("message")?.get("content")?;
    if let Some(text) = content.as_str() {
        return parse_command_model_text(text);
    }
    content.as_array()?.iter().find_map(|item| {
        item.get("text")
            .and_then(Value::as_str)
            .and_then(parse_command_model_text)
    })
}

fn parse_command_model_text(text: &str) -> Option<String> {
    let cleaned = strip_ansi(text)
        .replace("<local-command-stdout>", " ")
        .replace("</local-command-stdout>", " ")
        .replace("<command-name>", " ")
        .replace("</command-name>", " ")
        .replace("<command-message>", " ")
        .replace("</command-message>", " ")
        .replace("<command-args>", " ")
        .replace("</command-args>", " ");
    let marker = "set model to ";
    let lower = cleaned.to_ascii_lowercase();
    let start = lower.find(marker)? + marker.len();
    let raw = cleaned[start..]
        .lines()
        .next()
        .unwrap_or_default()
        .replace("(default)", "")
        .trim()
        .to_string();
    format_model_name(&raw)
}

fn strip_ansi(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            output.push(ch);
        }
    }
    output
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
        .and_then(|value| sanitize_field(Some(value), 64))
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
    if id.is_empty() || id == "<synthetic>" || id == "synthetic" {
        return None;
    }
    let context = if id.contains("1m") || id.contains("1m]") || id.contains("1m context") {
        " (1M)"
    } else {
        ""
    };
    let parts = id.split('-').collect::<Vec<_>>();
    let version = parts.windows(3).find_map(|window| {
        if ["opus", "sonnet", "haiku", "fable"].contains(&window[0])
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
    if id.contains("fable") {
        return Some(format!(
            "Claude Fable {}{}",
            version
                .or_else(|| {
                    id.split("fable-").nth(1).map(|rest| {
                        rest.chars()
                            .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
                            .collect::<String>()
                    })
                })
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "5".into()),
            context
        ));
    }
    if id.contains("opus") {
        return Some(format!(
            "Claude Opus {}{}",
            version
                .or_else(|| extract_model_version(model_id, "Opus"))
                .unwrap_or_else(|| "4.7".into()),
            context
        ));
    }
    if id.contains("sonnet") {
        return Some(format!(
            "Claude Sonnet {}{}",
            version
                .or_else(|| extract_model_version(model_id, "Sonnet"))
                .unwrap_or_else(|| "4.6".into()),
            context
        ));
    }
    if id.contains("haiku") {
        return Some(format!(
            "Claude Haiku {}{}",
            version
                .or_else(|| extract_model_version(model_id, "Haiku"))
                .unwrap_or_else(|| "4.5".into()),
            context
        ));
    }
    sanitize_field(Some(model_id), 64)
}

fn map_desktop_mode(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "cowork" | "task" => Some("Cowork".into()),
        "epitaxy" => Some("Code".into()),
        "chat" => Some("Chat".into()),
        "code" => Some("Code".into()),
        _ => None,
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
    if let Some(app_data) = std::env::var_os("APPDATA") {
        return PathBuf::from(app_data);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library")
            .join("Application Support");
    }
    home_dir()
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
    fn prices_models_by_id() {
        assert_eq!(
            model_pricing("claude-opus-4-8[1m]"),
            Some(("Opus", 5.0, 25.0))
        );
        assert_eq!(
            model_pricing("claude-opus-4-1-20250805"),
            Some(("Opus", 15.0, 75.0))
        );
        assert_eq!(
            model_pricing("claude-sonnet-4-6"),
            Some(("Sonnet", 3.0, 15.0))
        );
        assert_eq!(
            model_pricing("claude-haiku-4-5-20251001"),
            Some(("Haiku", 1.0, 5.0))
        );
        assert_eq!(model_pricing("claude-fable-5"), Some(("Fable", 10.0, 50.0)));
        assert_eq!(model_pricing("gpt-4o"), None);
    }

    #[test]
    fn sums_live_session_costs() {
        // Two Opus turns: cache split as 5m and 1h to exercise both write rates.
        let raw = concat!(
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":1000000,"output_tokens":1000000,"cache_read_input_tokens":1000000,"cache_creation_input_tokens":1000000,"cache_creation":{"ephemeral_5m_input_tokens":1000000,"ephemeral_1h_input_tokens":0}}}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"ignored"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":2000000,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":2000000}}}}"#,
        );
        let costs = session_model_costs_from_str(raw);
        assert_eq!(costs.len(), 1);
        let opus = &costs[0];
        assert_eq!(opus.label, "Opus");
        assert_eq!(opus.input_tokens, 1_000_000);
        assert_eq!(opus.output_tokens, 1_000_000);
        // input $5 + output $25 + cacheRead 1M*0.1*5=$0.5 + write5m 1M*1.25*5=$6.25
        //   + write1h 2M*2*5=$20  => $56.75
        assert!(
            (opus.cost_usd - 56.75).abs() < 1e-6,
            "cost_usd was {}",
            opus.cost_usd
        );
        assert!((opus.input_cost - 5.0).abs() < 1e-6);
        assert!((opus.output_cost - 25.0).abs() < 1e-6);
    }

    #[test]
    fn fold_live_session_replaces_current_project_prior_run() {
        let mc = |label: &str, input: u64, output: u64, cost: f64| ModelCost {
            label: label.into(),
            input_tokens: input,
            output_tokens: output,
            cost_usd: cost,
            ..ModelCost::default()
        };
        // Rollup already includes the current project's PRIOR session: Opus is
        // (other project 100/200 $5) + (current prior 10/20 $1); Haiku is the
        // current project's prior session only (5/5 $0.5).
        let all = vec![mc("Opus", 110, 220, 6.0), mc("Haiku", 5, 5, 0.5)];
        let current_stored = vec![mc("Opus", 10, 20, 1.0), mc("Haiku", 5, 5, 0.5)];
        let live = vec![mc("Opus", 50, 60, 3.0)];
        let folded = fold_live_session(all, &current_stored, &live);
        // Opus: other (100/200 $5) + live (50/60 $3), prior removed. Haiku nets to
        // zero (live no longer uses it) so the family disappears — no double count.
        assert_eq!(folded.len(), 1);
        let opus = &folded[0];
        assert_eq!(opus.label, "Opus");
        assert_eq!(opus.input_tokens, 150);
        assert_eq!(opus.output_tokens, 260);
        assert!(
            (opus.cost_usd - 8.0).abs() < 1e-9,
            "cost_usd was {}",
            opus.cost_usd
        );
    }

    #[test]
    fn fold_live_session_keeps_rollup_without_live() {
        let mc = |label: &str, input: u64| ModelCost {
            label: label.into(),
            input_tokens: input,
            ..ModelCost::default()
        };
        // No live session yet: the stored rollup is returned untouched so a
        // just-opened session still shows its prior spend.
        let folded = fold_live_session(vec![mc("Opus", 110)], &[mc("Opus", 10)], &[]);
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].input_tokens, 110);
    }

    #[test]
    fn builds_per_model_cost_line() {
        let mc = |label: &str, input: u64, output: u64, cost: f64| ModelCost {
            label: label.into(),
            input_tokens: input,
            output_tokens: output,
            cost_usd: cost,
            ..ModelCost::default()
        };
        // Current present: top-3 by spend (source is pre-sorted) plus overflow,
        // each with input/output token counts.
        let current = vec![
            mc("Opus", 80_000, 362_000, 11.17),
            mc("Sonnet", 9_000, 4_000, 0.55),
            mc("Haiku", 369, 19, 0.23),
            mc("Fable", 100, 50, 0.10),
        ];
        // Price only (per model, top-3 + overflow) — tokens belong to the token toggles.
        assert_eq!(
            build_cost_line(&current).as_deref(),
            Some("Opus $11.17 · Sonnet $0.55 · Haiku $0.23 · +1")
        );
        // Current project only — empty current means no line (no all-projects fallback).
        assert_eq!(build_cost_line(&[]), None);
        assert_eq!(build_cost_line(&[mc("Opus", 0, 0, 0.0)]), None);
    }

    #[test]
    fn builds_total_line() {
        let mc = |label: &str, cost: f64| ModelCost {
            label: label.into(),
            cost_usd: cost,
            ..ModelCost::default()
        };
        assert_eq!(
            build_total_line(&[mc("Opus", 300.0), mc("Sonnet", 21.99)]).as_deref(),
            Some("($321.99)")
        );
        assert_eq!(build_total_line(&[]), None);
        assert_eq!(build_total_line(&[mc("Opus", 0.0)]), None);
    }

    #[test]
    fn builds_token_lines() {
        let mc = |label: &str, input: u64, output: u64| ModelCost {
            label: label.into(),
            input_tokens: input,
            output_tokens: output,
            ..ModelCost::default()
        };
        // Project line sums tokens across models (no $).
        let current = vec![mc("Opus", 80_000, 360_000), mc("Sonnet", 4_000, 2_000)];
        assert_eq!(
            build_project_tokens_line(&current).as_deref(),
            Some("84K/362K tok")
        );
        // All line sums everything and prefixes with sigma; millions render as M.
        let all = vec![mc("Opus", 5_000_000, 13_200_000), mc("Haiku", 500_000, 0)];
        assert_eq!(
            build_all_tokens_line(&all).as_deref(),
            Some("\u{03a3} 5.5M/13.2M tok")
        );
        assert_eq!(build_project_tokens_line(&[]), None);
        assert_eq!(build_all_tokens_line(&[mc("Opus", 0, 0)]), None);
    }

    #[test]
    fn formats_known_models() {
        assert_eq!(
            format_model_name("claude-opus-4-6").as_deref(),
            Some("Claude Opus 4.6")
        );
        assert_eq!(
            format_model_name("Opus 4.7 (1M context) (default)").as_deref(),
            Some("Claude Opus 4.7 (1M)")
        );
        assert_eq!(
            format_model_name("claude-sonnet-4-6[1m]").as_deref(),
            Some("Claude Sonnet 4.6 (1M)")
        );
    }

    #[test]
    fn parses_model_command_output() {
        assert_eq!(
            parse_command_model_text(
                "<local-command-stdout>Set model to \u{1b}[1mOpus 4.7 (1M context) (default)\u{1b}[22m</local-command-stdout>"
            )
            .as_deref(),
            Some("Claude Opus 4.7 (1M)")
        );
        assert_eq!(
            parse_command_model_text(
                "<local-command-stdout>Set model to \u{1b}[1mSonnet 4.6\u{1b}[22m</local-command-stdout>"
            )
            .as_deref(),
            Some("Claude Sonnet 4.6")
        );
        assert_eq!(format_model_name("<synthetic>"), None);
    }

    #[test]
    fn detects_provider_auth_markers() {
        assert!(has_anthropic_api_auth(
            r#"{"apiKeyHelper":"/usr/local/bin/anthropic-key"}"#
        ));
        assert!(has_anthropic_api_auth(r#"{"key":"sk-ant-redacted"}"#));
        assert!(has_claude_account_auth(
            r#"{"oauthAccount":{"email":"user@example.com"}}"#
        ));
        assert!(has_claude_account_auth(r#"{"claudeAiOauth":{}}"#));
        assert!(!is_present(Some("  ")));
    }

    #[test]
    fn parses_macos_desktop_model_sources() {
        assert_eq!(
            parse_sticky_model_selector_text(
                "\0_https://claude.ai\0sticky-model-selector\0claude-opus-4-7[1m]\0"
            )
            .as_deref(),
            Some("Claude Opus 4.7 (1M)")
        );
        assert_eq!(
            parse_sticky_model_selector_text(
                "\0sticky-model-selector\0claude-sonnet-4-6\0_https://claude.ai"
            )
            .as_deref(),
            Some("Claude Sonnet 4.6")
        );
        assert_eq!(
            parse_sticky_model_selector_text("en-US\0ticky-model-selector\0claude-opus-4-7\0")
                .as_deref(),
            Some("Claude Opus 4.7")
        );
        assert_eq!(
            parse_sticky_model_selector_text(
                "sticky-model-\u{001d}or\u{0001}P-sonnet-4-6\u{0014}\u{0015}default\u{0009}opus-4-7"
            )
            .as_deref(),
            Some("Claude Sonnet 4.6")
        );
        assert_eq!(
            parse_sticky_model_selector_text("sticky-model-selector\0claude-opus-4-7\0Adaptive")
                .as_deref(),
            Some("Claude Opus 4.7 | Adaptive")
        );
        assert_eq!(
            parse_sticky_model_selector_text("sticky-model-selector\0claude-opus-4-7\0Extended")
                .as_deref(),
            Some("Claude Opus 4.7 | Extended")
        );
        assert_eq!(
            parse_cowork_model_selector_text(
                "cowork-sticky-model-selector\u{0001}c\u{0005}0h-opus-4-7"
            )
            .as_deref(),
            Some("Claude Opus 4.7")
        );
        assert_eq!(
            parse_desktop_effort_text("ccd-effort-level\u{0007}\u{0001}medium").as_deref(),
            Some("Medium")
        );
        assert_eq!(
            desktop_model_from_local_agent_session(
                r#"{"model":"claude-sonnet-4-6","title":"Organize files"}"#
            )
            .as_deref(),
            Some("Claude Sonnet 4.6")
        );
    }

    #[test]
    fn maps_current_macos_desktop_modes() {
        assert_eq!(map_desktop_mode("chat").as_deref(), Some("Chat"));
        assert_eq!(map_desktop_mode("cowork").as_deref(), Some("Cowork"));
        assert_eq!(map_desktop_mode("task").as_deref(), Some("Cowork"));
        assert_eq!(map_desktop_mode("epitaxy").as_deref(), Some("Code"));
    }

    #[test]
    fn parses_desktop_model_labels() {
        let candidate = parse_desktop_model_name("Claude Opus 4.7 1M Adaptive Extended Extra high")
            .expect("desktop model");
        assert_eq!(candidate.model, "Claude Opus 4.7 (1M)");
        assert!(candidate.adaptive);
        assert!(candidate.extended);
        assert_eq!(candidate.effort.as_deref(), Some("Extra high"));

        let label = format_desktop_model(&DesktopInfo {
            model: Some(candidate.model),
            adaptive: candidate.adaptive,
            extended: candidate.extended,
            effort: candidate.effort,
            ..DesktopInfo::default()
        });
        assert_eq!(
            label.as_deref(),
            Some("Claude Opus 4.7 (1M) | Adaptive | Extended | Extra high")
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
        ];
        let limits = parse_usage_limits(&names);
        assert_eq!(limits.len(), 3);
        assert_eq!(
            limits_line(
                &limits,
                LimitVisibility {
                    enabled: true,
                    show_5h: true,
                    show_all: true,
                    show_sonnet: true,
                }
            )
            .as_deref(),
            Some("Limits (3): 5h 1% | All 18% | Sonnet only 13%")
        );
        assert_eq!(
            limits_line(
                &limits,
                LimitVisibility {
                    enabled: true,
                    show_5h: false,
                    show_all: true,
                    show_sonnet: false,
                }
            )
            .as_deref(),
            Some("Limits (1): All 18%")
        );
        assert_eq!(
            limits_line(
                &[],
                LimitVisibility {
                    enabled: true,
                    show_5h: true,
                    show_all: false,
                    show_sonnet: false,
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
                updated_at_ms: 1_000,
            },
            UsageLimitEntry {
                label: "all".into(),
                used_percent: 18,
                reset: None,
                updated_at_ms: 1_000,
            },
        ];
        let detected = vec![UsageLimitEntry {
            label: "5h".into(),
            used_percent: 3,
            reset: None,
            updated_at_ms: 0,
        }];

        let limits = merge_limit_entries(&cached, &detected, 2_000);
        assert!(limits.iter().all(|entry| entry.updated_at_ms != 0));
        assert_eq!(
            limits
                .iter()
                .find(|entry| entry.label == "5h")
                .unwrap()
                .updated_at_ms,
            2_000
        );
        assert_eq!(
            limits_line(
                &limits,
                LimitVisibility {
                    enabled: true,
                    show_5h: true,
                    show_all: true,
                    show_sonnet: true,
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
    fn formats_fable_model() {
        assert_eq!(
            format_model_name("claude-fable-5").as_deref(),
            Some("Claude Fable 5")
        );
        assert_eq!(
            format_model_name("anthropic.claude-fable-5").as_deref(),
            Some("Claude Fable 5")
        );
        assert_eq!(
            format_model_name("claude-fable-5[1m]").as_deref(),
            Some("Claude Fable 5 (1M)")
        );
        // Mythos is intentionally left unformatted (enterprise / invite-only).
        assert_eq!(
            format_model_name("claude-mythos-5").as_deref(),
            Some("claude-mythos-5")
        );
    }

    #[test]
    fn parses_effort_command_output() {
        assert_eq!(
            parse_command_effort_text(
                "<local-command-stdout>Set effort level to ultracode (this session only): xhigh + dynamic workflow orchestration</local-command-stdout>"
            )
            .as_deref(),
            Some("Ultracode")
        );
        assert_eq!(
            parse_command_effort_text(
                "<local-command-stdout>Set effort level to high</local-command-stdout>"
            )
            .as_deref(),
            Some("High")
        );
        // Prose without the command-output tag must not match (e.g. chat text).
        assert_eq!(parse_command_effort_text("set effort level to max"), None);
        assert_eq!(effort_label("xhigh").as_deref(), Some("Extra high"));
        assert_eq!(effort_label("nonsense"), None);
    }

    #[test]
    fn parses_iso_timestamps_as_utc() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:01Z"), Some(1000));
    }
}
