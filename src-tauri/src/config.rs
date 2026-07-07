// Shared settings model for the tray app (main.rs) and the daemon. Single
// source of truth for ClaudeConfig so the two sides can never drift apart
// (a field missing on one side used to silently wipe saved settings).

use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RpcButton {
    pub(crate) label: String,
    pub(crate) url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClaudeConfig {
    #[serde(default)]
    pub(crate) dnd: bool,
    #[serde(default = "default_show_limits")]
    pub(crate) show_limits: bool,
    #[serde(default = "default_show_limits")]
    pub(crate) show_limit_5h: bool,
    #[serde(default = "default_show_limits")]
    pub(crate) show_limit_all: bool,
    #[serde(default = "default_show_limits")]
    pub(crate) show_limit_sonnet: bool,
    #[serde(default = "default_show_limits")]
    pub(crate) show_provider: bool,
    #[serde(default = "default_show_limits")]
    pub(crate) show_effort: bool,
    #[serde(default = "default_show_limits")]
    pub(crate) show_session_title: bool,
    #[serde(default)]
    pub(crate) show_cost: bool,
    #[serde(default)]
    pub(crate) show_cost_total: bool,
    #[serde(default)]
    pub(crate) show_project_tokens: bool,
    #[serde(default)]
    pub(crate) show_all_tokens: bool,
    #[serde(default)]
    pub(crate) show_idle: bool,
    #[serde(default)]
    pub(crate) verbose: bool,
    #[serde(default = "default_rpc_mode")]
    pub(crate) rpc_mode: String,
    #[serde(default = "default_buttons")]
    pub(crate) buttons: Vec<RpcButton>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            dnd: false,
            show_limits: default_show_limits(),
            show_limit_5h: default_show_limits(),
            show_limit_all: default_show_limits(),
            show_limit_sonnet: default_show_limits(),
            show_provider: default_show_limits(),
            show_effort: default_show_limits(),
            show_session_title: default_show_limits(),
            show_cost: false,
            show_cost_total: false,
            show_project_tokens: false,
            show_all_tokens: false,
            show_idle: false,
            verbose: false,
            rpc_mode: default_rpc_mode(),
            buttons: default_buttons(),
        }
    }
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
            url: "https://github.com/stealthsrc/claude-rpc".into(),
        },
    ]
}

pub(crate) fn read_config(path: &Path) -> ClaudeConfig {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return ClaudeConfig::default(),
    };
    normalize_config(
        serde_json::from_str::<ClaudeConfig>(raw.trim_start_matches('\u{feff}'))
            .unwrap_or_default(),
    )
}

pub(crate) fn normalize_config(mut config: ClaudeConfig) -> ClaudeConfig {
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

pub(crate) fn normalize_mode(value: &str) -> String {
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
