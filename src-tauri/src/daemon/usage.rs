// Usage & cost tracking: per-model cost rollups (live session + ~/.claude.json),
// usage-limit buckets (Desktop UI scrape + OAuth usage API), their caches, and
// the Discord/status lines built from them.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, fs, path::Path};

use super::{
    app_dir, claude_dir, home_dir, modified_ms, normalize_ui_label, now_ms, truncate, write_status,
    StateMachine,
};
use crate::config::ClaudeConfig;

const LIMITS_CACHE_MS: u64 = 6 * 60 * 60 * 1_000;
// A per-bucket percentage is only shown as current if it was refreshed within
// this window. Kept below the shortest usage window (5h) so a stale value can
// never outlive its own reset; OAuth re-polls well within it (<=10 min idle).
const LIMITS_DISPLAY_TTL_MS: u64 = 60 * 60 * 1_000;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UsageLimitEntry {
    pub(crate) label: String,
    pub(crate) used_percent: u8,
    pub(crate) reset: Option<String>,
    // When this bucket's percentage was last observed. Stamped per-entry in
    // merge_limit_entries so a bucket absent from a later (partial) detection is
    // not re-marked fresh, and can be expired individually past its window.
    #[serde(default)]
    pub(crate) updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LimitsCache {
    updated_at: u64,
    limits: Vec<UsageLimitEntry>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LimitVisibility {
    pub(crate) enabled: bool,
    pub(crate) show_5h: bool,
    pub(crate) show_all: bool,
    pub(crate) show_sonnet: bool,
}

// Per-model usage rolled up by model family (Opus/Sonnet/Haiku/Fable). `cost_usd`
// is Claude Code's own figure (cache-aware) summed across the family's snapshots;
// `input_cost`/`output_cost` are the table-rate breakdown of the input/output
// tokens (no cache), so the UI can show both the real spend and the in/out split.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCost {
    pub(crate) label: String,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) cache_read_tokens: u64,
    pub(crate) cache_creation_tokens: u64,
    pub(crate) input_cost: f64,
    pub(crate) output_cost: f64,
    pub(crate) cost_usd: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectUsage {
    path_norm: String,
    models: Vec<ModelCost>,
}
pub(crate) fn limit_visibility(config: &ClaudeConfig) -> LimitVisibility {
    LimitVisibility {
        enabled: config.show_limits,
        show_5h: config.show_limit_5h,
        show_all: config.show_limit_all,
        show_sonnet: config.show_limit_sonnet,
    }
}

// Cost/token data must be gathered whenever any cost- or token-related Discord
// label (or the Settings panel) is on.
pub(crate) fn cost_enabled(config: &ClaudeConfig) -> bool {
    config.show_cost
        || config.show_cost_total
        || config.show_project_tokens
        || config.show_all_tokens
}

// Per-million input/output rates from the published model pricing
// (platform.claude.com/docs/.../models/overview). Returns the family label used
// to roll up snapshots and to match the active model for the Discord summary.
pub(crate) fn model_pricing(model_id: &str) -> Option<(&'static str, f64, f64)> {
    let id = model_id.to_ascii_lowercase();
    // Snapshot ids carry suffixes like "[1m]"; classify on the bare id.
    let base = id.split('[').next().unwrap_or(id.as_str());
    if base.contains("fable") || base.contains("mythos") {
        Some(("Fable", 10.0, 50.0))
    } else if base.contains("haiku") {
        Some(("Haiku", 1.0, 5.0))
    } else if base.contains("sonnet") {
        Some(("Sonnet", 3.0, 15.0))
    } else if base.contains("opus") {
        // Opus 4.1 is the lone $15/$75 tier; 4.5/4.6/4.7/4.8 are all $5/$25.
        if base.contains("opus-4-1-") || base.ends_with("opus-4-1") {
            Some(("Opus", 15.0, 75.0))
        } else {
            Some(("Opus", 5.0, 25.0))
        }
    } else {
        None
    }
}

pub(crate) fn normalize_project_path(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

pub(crate) fn format_cost(value: f64) -> String {
    format!("${value:.2}")
}

pub(crate) fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.0}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

pub(crate) fn sort_costs(models: &mut [ModelCost]) {
    models.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

pub(crate) fn add_cost(bucket: &mut ModelCost, model: &ModelCost) {
    bucket.input_tokens += model.input_tokens;
    bucket.output_tokens += model.output_tokens;
    bucket.cache_read_tokens += model.cache_read_tokens;
    bucket.cache_creation_tokens += model.cache_creation_tokens;
    bucket.input_cost += model.input_cost;
    bucket.output_cost += model.output_cost;
    bucket.cost_usd += model.cost_usd;
}

// Inverse of add_cost: remove a snapshot already folded into the bucket. Tokens
// use saturating_sub and costs are floored at 0 since the subtrahend is always a
// subset of the bucket, so the result can't legitimately go negative.
pub(crate) fn sub_cost(bucket: &mut ModelCost, model: &ModelCost) {
    bucket.input_tokens = bucket.input_tokens.saturating_sub(model.input_tokens);
    bucket.output_tokens = bucket.output_tokens.saturating_sub(model.output_tokens);
    bucket.cache_read_tokens = bucket
        .cache_read_tokens
        .saturating_sub(model.cache_read_tokens);
    bucket.cache_creation_tokens = bucket
        .cache_creation_tokens
        .saturating_sub(model.cache_creation_tokens);
    bucket.input_cost = (bucket.input_cost - model.input_cost).max(0.0);
    bucket.output_cost = (bucket.output_cost - model.output_cost).max(0.0);
    bucket.cost_usd = (bucket.cost_usd - model.cost_usd).max(0.0);
}

// Merge the live in-progress session into the all-projects rollup. The active
// project's stored lastModelUsage (`current_stored`, already inside `all`) is
// subtracted before the live session is added, so a running project that also
// had a prior completed session isn't counted twice (prior + live). With no live
// session the rollup is returned unchanged, so a just-opened session keeps
// showing its prior spend until the first turn lands.
pub(crate) fn fold_live_session(
    all: Vec<ModelCost>,
    current_stored: &[ModelCost],
    current: &[ModelCost],
) -> Vec<ModelCost> {
    if current.is_empty() {
        return all;
    }
    let mut merged: HashMap<String, ModelCost> = HashMap::new();
    for model in &all {
        let bucket = merged
            .entry(model.label.clone())
            .or_insert_with(|| ModelCost {
                label: model.label.clone(),
                ..ModelCost::default()
            });
        add_cost(bucket, model);
    }
    for model in current_stored {
        if let Some(bucket) = merged.get_mut(&model.label) {
            sub_cost(bucket, model);
        }
    }
    for model in current {
        let bucket = merged
            .entry(model.label.clone())
            .or_insert_with(|| ModelCost {
                label: model.label.clone(),
                ..ModelCost::default()
            });
        add_cost(bucket, model);
    }
    // Drop families that netted to exactly zero: a prior-session family of the
    // current project that the live session no longer uses.
    let mut all: Vec<ModelCost> = merged
        .into_values()
        .filter(|model| {
            model.input_tokens > 0
                || model.output_tokens > 0
                || model.cache_read_tokens > 0
                || model.cache_creation_tokens > 0
        })
        .collect();
    sort_costs(&mut all);
    all
}

// Parse ~/.claude.json once into per-project, per-family rollups. Cheap to
// aggregate afterwards; the parse itself is gated behind an mtime cache.
pub(crate) fn read_project_usages() -> Vec<ProjectUsage> {
    let raw = match fs::read_to_string(home_dir().join(".claude.json")) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let value: Value = match serde_json::from_str(raw.trim_start_matches('\u{feff}')) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let Some(projects) = value.get("projects").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (path, project) in projects {
        let Some(usage) = project.get("lastModelUsage").and_then(Value::as_object) else {
            continue;
        };
        let mut by_family: HashMap<&'static str, ModelCost> = HashMap::new();
        for (model_id, entry) in usage {
            let Some((label, input_rate, output_rate)) = model_pricing(model_id) else {
                continue;
            };
            let input = entry
                .get("inputTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let output = entry
                .get("outputTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let cache_read = entry
                .get("cacheReadInputTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let cache_creation = entry
                .get("cacheCreationInputTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let cost = entry.get("costUSD").and_then(Value::as_f64).unwrap_or(0.0);
            let bucket = by_family.entry(label).or_insert_with(|| ModelCost {
                label: label.to_string(),
                ..ModelCost::default()
            });
            bucket.input_tokens += input;
            bucket.output_tokens += output;
            bucket.cache_read_tokens += cache_read;
            bucket.cache_creation_tokens += cache_creation;
            bucket.input_cost += input as f64 / 1_000_000.0 * input_rate;
            bucket.output_cost += output as f64 / 1_000_000.0 * output_rate;
            bucket.cost_usd += cost;
        }
        if by_family.is_empty() {
            continue;
        }
        let mut models: Vec<ModelCost> = by_family.into_values().collect();
        sort_costs(&mut models);
        out.push(ProjectUsage {
            path_norm: normalize_project_path(path),
            models,
        });
    }
    out
}

pub(crate) fn project_usages(machine: &mut StateMachine) -> &[ProjectUsage] {
    let path = home_dir().join(".claude.json");
    let mtime = modified_ms(&path).unwrap_or(0);
    if machine.cached_project_usages.is_none() || machine.cached_project_usages_mtime != mtime {
        machine.cached_project_usages = Some(read_project_usages());
        machine.cached_project_usages_mtime = mtime;
    }
    machine.cached_project_usages.as_deref().unwrap_or(&[])
}

// Returns (all projects combined, current project) rolled up per model family.
pub(crate) fn aggregate_costs(
    usages: &[ProjectUsage],
    cwd: Option<&str>,
) -> (Vec<ModelCost>, Vec<ModelCost>) {
    let mut all: HashMap<String, ModelCost> = HashMap::new();
    for usage in usages {
        for model in &usage.models {
            let bucket = all.entry(model.label.clone()).or_insert_with(|| ModelCost {
                label: model.label.clone(),
                ..ModelCost::default()
            });
            add_cost(bucket, model);
        }
    }
    let mut all: Vec<ModelCost> = all.into_values().collect();
    sort_costs(&mut all);

    let current = cwd
        .map(normalize_project_path)
        .and_then(|target| usages.iter().find(|usage| usage.path_norm == target))
        .map(|usage| usage.models.clone())
        .unwrap_or_default();

    (all, current)
}

// Standard prompt-caching multipliers over the model's base input rate:
// reads are 0.1x, 5-minute cache writes 1.25x, 1-hour cache writes 2x.
const CACHE_READ_MULT: f64 = 0.1;
const CACHE_WRITE_5M_MULT: f64 = 1.25;
const CACHE_WRITE_1H_MULT: f64 = 2.0;

// Live per-model cost for the in-progress session, summed straight from the
// session .jsonl. Needed because ~/.claude.json only records per-project usage
// (lastModelUsage / lastCost) at session end, so a running session shows nothing
// there. Each assistant turn carries message.usage; cost_usd is computed locally
// (input/output at table rate plus cache reads/writes at the standard multipliers)
// since Claude's own costUSD isn't written until the session closes.
pub(crate) fn session_model_costs(path: &Path) -> Vec<ModelCost> {
    match fs::read_to_string(path) {
        Ok(raw) => session_model_costs_from_str(&raw),
        Err(_) => Vec::new(),
    }
}

pub(crate) fn session_model_costs_from_str(raw: &str) -> Vec<ModelCost> {
    let mut by_family: HashMap<&'static str, ModelCost> = HashMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if entry.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(message) = entry.get("message") else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };
        let model_id = message.get("model").and_then(Value::as_str).unwrap_or("");
        let Some((label, input_rate, output_rate)) = model_pricing(model_id) else {
            continue;
        };
        let tok = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let input = tok("input_tokens");
        let output = tok("output_tokens");
        let cache_read = tok("cache_read_input_tokens");
        let cache_creation = tok("cache_creation_input_tokens");
        // Split cache writes into 5m vs 1h when the breakdown is present; otherwise
        // treat the whole creation bucket as 5m.
        let (write_5m, write_1h) = match usage.get("cache_creation") {
            Some(detail) => {
                let f = |key: &str| detail.get(key).and_then(Value::as_u64).unwrap_or(0);
                let (w5, w1) = (
                    f("ephemeral_5m_input_tokens"),
                    f("ephemeral_1h_input_tokens"),
                );
                if w5 + w1 == 0 {
                    (cache_creation, 0)
                } else {
                    (w5, w1)
                }
            }
            None => (cache_creation, 0),
        };

        let input_cost = input as f64 / 1_000_000.0 * input_rate;
        let output_cost = output as f64 / 1_000_000.0 * output_rate;
        let cache_cost = (cache_read as f64 * CACHE_READ_MULT
            + write_5m as f64 * CACHE_WRITE_5M_MULT
            + write_1h as f64 * CACHE_WRITE_1H_MULT)
            / 1_000_000.0
            * input_rate;

        let bucket = by_family.entry(label).or_insert_with(|| ModelCost {
            label: label.to_string(),
            ..ModelCost::default()
        });
        bucket.input_tokens += input;
        bucket.output_tokens += output;
        bucket.cache_read_tokens += cache_read;
        bucket.cache_creation_tokens += cache_creation;
        bucket.input_cost += input_cost;
        bucket.output_cost += output_cost;
        bucket.cost_usd += input_cost + output_cost + cache_cost;
    }
    let mut models: Vec<ModelCost> = by_family.into_values().collect();
    sort_costs(&mut models);
    models
}

pub(crate) fn session_costs(machine: &mut StateMachine, path: &Path) -> Vec<ModelCost> {
    let mtime = modified_ms(path).unwrap_or(0);
    if machine.cached_session_costs.is_none()
        || machine.cached_session_costs_file.as_deref() != Some(path)
        || machine.cached_session_costs_mtime != mtime
    {
        machine.cached_session_costs = Some(session_model_costs(path));
        machine.cached_session_costs_file = Some(path.to_path_buf());
        machine.cached_session_costs_mtime = mtime;
    }
    machine.cached_session_costs.clone().unwrap_or_default()
}

// Compact per-model price summary for the current project/session only, e.g.
// "Opus $81.57 · Sonnet $0.55 · +1". Price only — token counts come from the
// separate Proj/All tokens toggles, so Cost never duplicates the token labels.
pub(crate) fn build_cost_line(current: &[ModelCost]) -> Option<String> {
    let positives: Vec<&ModelCost> = current
        .iter()
        .filter(|model| model.cost_usd > 0.0)
        .collect();
    if positives.is_empty() {
        return None;
    }
    const TOP: usize = 3;
    let mut parts: Vec<String> = positives
        .iter()
        .take(TOP)
        .map(|model| format!("{} {}", model.label, format_cost(model.cost_usd)))
        .collect();
    if positives.len() > TOP {
        parts.push(format!("+{}", positives.len() - TOP));
    }
    Some(parts.join(" · "))
}

// All-projects grand total in parentheses for the Discord line, e.g. "($321.99)".
// Gated by its own toggle so it can be shown independently of the per-model line.
pub(crate) fn build_total_line(all: &[ModelCost]) -> Option<String> {
    let total: f64 = all.iter().map(|model| model.cost_usd).sum();
    (total > 0.0).then(|| format!("({})", format_cost(total)))
}

pub(crate) fn sum_tokens(models: &[ModelCost]) -> (u64, u64) {
    models.iter().fold((0, 0), |(input, output), model| {
        (input + model.input_tokens, output + model.output_tokens)
    })
}

// Current project's total input/output token counts (across models), e.g.
// "84K/451K tok". Token-only view, independent of the cost labels.
pub(crate) fn build_project_tokens_line(current: &[ModelCost]) -> Option<String> {
    let (input, output) = sum_tokens(current);
    (input > 0 || output > 0)
        .then(|| format!("{}/{} tok", format_tokens(input), format_tokens(output)))
}

// All-projects total input/output token counts, e.g. "Σ 6.5M/13.2M tok". Summed
// over every project found in ~/.claude.json (plus the live session), so it
// adapts to whatever projects each user actually has.
pub(crate) fn build_all_tokens_line(all: &[ModelCost]) -> Option<String> {
    let (input, output) = sum_tokens(all);
    (input > 0 || output > 0).then(|| {
        format!(
            "\u{03a3} {}/{} tok",
            format_tokens(input),
            format_tokens(output)
        )
    })
}
#[cfg(any(windows, test))]
pub(crate) fn parse_usage_limits(names: &[String]) -> Vec<UsageLimitEntry> {
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
            updated_at_ms: 0,
        });
    }

    sort_limit_entries(&mut entries);
    entries
}

#[cfg(any(windows, test))]
pub(crate) fn parse_used_percent(value: &str) -> Option<u8> {
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
    // Parse wide then clamp to 100: an over-limit/glitched scrape (>100, or >255
    // which would overflow u8 -> None) must not silently drop the row and skew the
    // "Limits (N)" count — mirror the OAuth path's clamp(0,100).
    digits.parse::<u32>().ok().map(|value| value.min(100) as u8)
}

#[cfg(any(windows, test))]
pub(crate) fn find_limit_label(names: &[String], usage_index: usize) -> Option<(String, usize)> {
    let start = usage_index.saturating_sub(12);
    for index in (start..usage_index).rev() {
        let label = match normalize_ui_label(&names[index]).as_str() {
            "current session" => "5h",
            "all models" => "All",
            "sonnet only" => "Sonnet only",
            _ => continue,
        };
        return Some((label.into(), index));
    }
    None
}

#[cfg(any(windows, test))]
pub(crate) fn find_limit_reset(
    names: &[String],
    label_index: usize,
    usage_index: usize,
) -> Option<String> {
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

pub(crate) fn limits_line(
    entries: &[UsageLimitEntry],
    visibility: LimitVisibility,
) -> Option<String> {
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

pub(crate) fn visible_limit_labels(visibility: LimitVisibility) -> Vec<&'static str> {
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
    labels
}

pub(crate) fn current_limits(
    machine: &mut StateMachine,
    detected_limits: &[UsageLimitEntry],
    verbose: bool,
) -> Vec<UsageLimitEntry> {
    let now = now_ms();
    if machine.cached_limits.is_empty() {
        if let Some(cache) = read_limits_cache(now) {
            machine.cached_limits = cache.limits;
        }
    }

    if !detected_limits.is_empty() {
        machine.cached_limits = merge_limit_entries(&machine.cached_limits, detected_limits, now);
        write_limits_cache(now, &machine.cached_limits);
        return fresh_limit_entries(&machine.cached_limits, now);
    }

    if let Some(oauth_limits) = maybe_fetch_oauth_limits(machine, now, verbose) {
        if !oauth_limits.is_empty() {
            machine.cached_limits = merge_limit_entries(&machine.cached_limits, &oauth_limits, now);
            write_limits_cache(now, &machine.cached_limits);
            return fresh_limit_entries(&machine.cached_limits, now);
        }
    }

    fresh_limit_entries(&machine.cached_limits, now)
}

// Drop buckets not refreshed within LIMITS_DISPLAY_TTL_MS so an individual stale
// percentage (e.g. one OAuth omitted, or all sources down) is never shown as
// current, while still-fresh siblings keep displaying.
pub(crate) fn fresh_limit_entries(entries: &[UsageLimitEntry], now: u64) -> Vec<UsageLimitEntry> {
    entries
        .iter()
        .filter(|entry| now.saturating_sub(entry.updated_at_ms) <= LIMITS_DISPLAY_TTL_MS)
        .cloned()
        .collect()
}

const OAUTH_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_USAGE_BETA: &str = "oauth-2025-04-20";
const OAUTH_USAGE_IDLE_POLL_MS: u64 = 10 * 60 * 1000;
const OAUTH_USAGE_ACTIVITY_POLL_MS: u64 = 60 * 1000;
const OAUTH_USAGE_BACKOFF_MS: u64 = 5 * 60 * 1000;

pub(crate) fn maybe_fetch_oauth_limits(
    machine: &mut StateMachine,
    now: u64,
    verbose: bool,
) -> Option<Vec<UsageLimitEntry>> {
    if now < machine.oauth_backoff_until_ms {
        machine.pending_activity_refresh = false;
        return None;
    }
    let min_interval = if machine.pending_activity_refresh {
        OAUTH_USAGE_ACTIVITY_POLL_MS
    } else {
        OAUTH_USAGE_IDLE_POLL_MS
    };
    if machine.oauth_last_attempt_ms != 0
        && now.saturating_sub(machine.oauth_last_attempt_ms) < min_interval
    {
        return None;
    }
    machine.oauth_last_attempt_ms = now;
    machine.pending_activity_refresh = false;
    match fetch_oauth_usage(verbose) {
        Ok(entries) => Some(entries),
        Err(OAuthFetchError::RateLimited) => {
            machine.oauth_backoff_until_ms = now + OAUTH_USAGE_BACKOFF_MS;
            None
        }
        Err(_) => None,
    }
}

pub(crate) enum OAuthFetchError {
    NoToken,
    Network,
    RateLimited,
    Parse,
}

pub(crate) fn fetch_oauth_usage(verbose: bool) -> Result<Vec<UsageLimitEntry>, OAuthFetchError> {
    let token = read_oauth_access_token().ok_or(OAuthFetchError::NoToken)?;
    let response = ureq::get(OAUTH_USAGE_URL)
        .timeout(std::time::Duration::from_secs(8))
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", OAUTH_USAGE_BETA)
        .set("User-Agent", "claude-rpc")
        .call();
    let body = match response {
        Ok(resp) => resp.into_string().map_err(|_| OAuthFetchError::Parse)?,
        Err(ureq::Error::Status(429, _)) => return Err(OAuthFetchError::RateLimited),
        Err(_) => return Err(OAuthFetchError::Network),
    };
    let value: Value = serde_json::from_str(&body).map_err(|_| OAuthFetchError::Parse)?;
    if verbose {
        write_oauth_debug(&value);
    }
    Ok(parse_oauth_usage_response(&value))
}

pub(crate) fn write_oauth_debug(body: &Value) {
    let path = app_dir().join("oauth-usage-debug.json");
    write_status(
        &path,
        &json!({
            "fetchedAt": now_ms(),
            "body": body,
        }),
    );
}

pub(crate) fn read_oauth_access_token() -> Option<String> {
    let path = claude_dir().join(".credentials.json");
    let raw = fs::read_to_string(&path).ok()?;
    let value: Value = serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()?;
    let oauth = value.get("claudeAiOauth")?;
    let token = oauth.get("accessToken").and_then(Value::as_str)?;
    if let Some(expires_at) = oauth.get("expiresAt").and_then(Value::as_u64) {
        if expires_at < now_ms() {
            return None;
        }
    }
    Some(token.to_string())
}

pub(crate) fn parse_oauth_usage_response(body: &Value) -> Vec<UsageLimitEntry> {
    let mut entries = Vec::new();
    let buckets = [
        ("five_hour", "5h"),
        ("seven_day", "All"),
        ("seven_day_sonnet", "Sonnet only"),
    ];
    for (key, label) in buckets {
        let Some(bucket) = body.get(key) else {
            continue;
        };
        let Some(percent) = extract_oauth_usage_percent(bucket) else {
            continue;
        };
        let reset = bucket
            .get("resets_at")
            .and_then(Value::as_str)
            .or_else(|| bucket.get("reset_at").and_then(Value::as_str))
            .map(String::from);
        entries.push(UsageLimitEntry {
            label: label.into(),
            used_percent: percent,
            reset,
            updated_at_ms: 0,
        });
    }
    entries
}

pub(crate) fn extract_oauth_usage_percent(bucket: &Value) -> Option<u8> {
    // `utilization` is always a 0..100 percentage in the OAuth usage API.
    if let Some(raw) = bucket.get("utilization").and_then(Value::as_f64) {
        return Some(raw.round().clamp(0.0, 100.0) as u8);
    }
    for key in ["percent_used", "used_percent", "usage", "value"] {
        let Some(raw) = bucket.get(key).and_then(Value::as_f64) else {
            continue;
        };
        // Auto-detect: ratio (0..1) gets multiplied; percentage (>1.5) used directly
        let pct = if raw <= 1.5 { raw * 100.0 } else { raw };
        return Some(pct.round().clamp(0.0, 100.0) as u8);
    }
    None
}

pub(crate) fn write_limits_cache(updated_at: u64, limits: &[UsageLimitEntry]) {
    let path = app_dir().join("limits-cache.json");
    write_status(
        &path,
        &json!({
            "updatedAt": updated_at,
            "limits": limits,
        }),
    );
}

pub(crate) fn read_limits_cache(now: u64) -> Option<LimitsCache> {
    let raw = fs::read_to_string(app_dir().join("limits-cache.json")).ok()?;
    let value: Value = serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()?;
    let updated_at = value.get("updatedAt").and_then(Value::as_u64)?;
    if now.saturating_sub(updated_at) > LIMITS_CACHE_MS {
        return None;
    }
    let mut limits =
        normalize_limit_entries(serde_json::from_value(value.get("limits")?.clone()).ok()?);
    // Cache files written before per-entry stamps carry updated_at_ms == 0; treat
    // them as having the file's age so they expire correctly rather than instantly.
    for entry in &mut limits {
        if entry.updated_at_ms == 0 {
            entry.updated_at_ms = updated_at;
        }
    }
    Some(LimitsCache { updated_at, limits })
}

pub(crate) fn merge_limit_entries(
    cached: &[UsageLimitEntry],
    detected: &[UsageLimitEntry],
    now: u64,
) -> Vec<UsageLimitEntry> {
    let mut merged = normalize_limit_entries(cached.to_vec());
    for mut entry in normalize_limit_entries(detected.to_vec()) {
        entry.updated_at_ms = now;
        if let Some(existing) = merged.iter_mut().find(|item| item.label == entry.label) {
            *existing = entry;
        } else {
            merged.push(entry);
        }
    }
    sort_limit_entries(&mut merged);
    merged
}

pub(crate) fn normalize_limit_entries(entries: Vec<UsageLimitEntry>) -> Vec<UsageLimitEntry> {
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

pub(crate) fn normalize_limit_label(label: &str) -> Option<&'static str> {
    match normalize_ui_label(label).as_str() {
        "5h" | "session" | "current session" => Some("5h"),
        "all" | "all models" => Some("All"),
        "sonnet" | "sonnet only" | "max only" => Some("Sonnet only"),
        _ => None,
    }
}

pub(crate) fn sort_limit_entries(entries: &mut [UsageLimitEntry]) {
    entries.sort_by_key(|entry| match entry.label.as_str() {
        "5h" => 0,
        "All" => 1,
        "Sonnet only" => 2,
        _ => 9,
    });
}
