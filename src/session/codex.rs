use anyhow::Result;
use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::Path;

use crate::adapter::mapping::ToolMapping;
use crate::session::cache::{CachedCodexTotal, CachedDailyUsage};
use crate::session::model::{SessionMeta, TokenUsage, UsageSummary};
use serde::Deserialize;

#[derive(Deserialize, Default, Clone)]
struct CodexTokenUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cached_input_tokens: Option<i64>,
    reasoning_output_tokens: Option<i64>,
}

impl CodexTokenUsage {
    fn into_token_usage(self) -> TokenUsage {
        let cached = self
            .cached_input_tokens
            .unwrap_or(0)
            .min(self.input_tokens.unwrap_or(0));
        TokenUsage {
            input_tokens: self.input_tokens.unwrap_or(0),
            output_tokens: self.output_tokens.unwrap_or(0)
                + self.reasoning_output_tokens.unwrap_or(0),
            cache_read_tokens: cached,
            cache_creation_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            web_search_requests: 0,
        }
    }
    fn to_cached_total(&self) -> CachedCodexTotal {
        CachedCodexTotal {
            input_tokens: self.input_tokens.unwrap_or(0),
            output_tokens: self.output_tokens.unwrap_or(0),
            cached_input_tokens: self.cached_input_tokens.unwrap_or(0),
            reasoning_output_tokens: self.reasoning_output_tokens.unwrap_or(0),
        }
    }
}

pub(crate) fn scan_sessions(
    config_dir: &Path,
    mapping: &ToolMapping,
    min_mtime_ms: Option<i64>,
) -> Result<Vec<SessionMeta>> {
    let session_dir = config_dir.join(mapping.session.path());
    super::scan_sessions_with_filter(
        &session_dir,
        min_mtime_ms,
        |e| e.extension().is_some_and(|ext| ext == "jsonl"),
        |p| parse_session_meta(p, mapping),
    )
}

fn parse_session_meta(path: &Path, mapping: &ToolMapping) -> Option<SessionMeta> {
    let reader = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let mut session_id = None;
    let mut project_dir = None;
    let mut created_at: Option<i64> = None;
    let mut last_active_at: Option<i64> = None;
    let mut user_texts: Vec<String> = Vec::new();
    for line in reader.lines() {
        let line = line.ok()?;
        if !line.contains("session_meta") && !line.contains("response_item") {
            continue;
        }
        let value: serde_json::Value = match sonic_rs::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match value.get("type").and_then(|v| v.as_str()) {
            Some("session_meta") => {
                if let Some(p) = value.get("payload") {
                    if session_id.is_none() {
                        session_id = p
                            .get("id")
                            .or_else(|| p.get("session_id"))
                            .or_else(|| p.get("sessionId"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    if project_dir.is_none() {
                        project_dir = p.get("cwd").and_then(|v| v.as_str()).map(|s| s.to_string());
                    }
                    if created_at.is_none() {
                        created_at = p
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .and_then(super::parse_iso_timestamp);
                    }
                }
                if created_at.is_none() {
                    created_at = value
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .and_then(super::parse_iso_timestamp);
                }
            }
            Some("response_item") if user_texts.len() < 3 => {
                if let Some(p) = value.get("payload") {
                    if p.get("role").and_then(|v| v.as_str()) == Some("user") {
                        if let Some(text) = extract_codex_user_text(p.get("content")) {
                            if !text.starts_with("# AGENTS.md")
                                && !text.starts_with("# INSTRUCTIONS")
                                && !text.starts_with("<permissions")
                                && !text.starts_with('<')
                                && !text.starts_with('[')
                            {
                                user_texts.push(text);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if session_id.is_none() {
        session_id = super::fallback_session_id(path);
    }
    let sid = session_id?;
    if last_active_at.is_none() {
        last_active_at = super::mtime_ms_nonzero(path);
    }
    let title = super::fallback_title(&user_texts, project_dir.as_deref());
    Some(super::build_session_meta(
        mapping,
        path,
        sid,
        title,
        None,
        project_dir,
        created_at,
        last_active_at,
    ))
}

pub(crate) fn extract_usage(session: &SessionMeta) -> Result<Vec<UsageSummary>> {
    let mut state = CodexIncrementalState::default();
    extract_usage_from(session, 0, &mut state)
}

pub(crate) fn extract_usage_incremental(
    session: &SessionMeta,
    from_byte: u64,
    prev_total: Option<CachedCodexTotal>,
    current_model: Option<String>,
    cached_usages: &[CachedDailyUsage],
) -> Result<(Vec<UsageSummary>, CodexIncrementalState)> {
    let mut state = CodexIncrementalState {
        prev_total,
        current_model: current_model.unwrap_or_default(),
        model_usages: HashMap::new(),
        model_counts: HashMap::new(),
    };
    for cu in cached_usages {
        state.model_usages.insert(
            cu.model.clone(),
            TokenUsage {
                input_tokens: cu.input_tokens,
                output_tokens: cu.output_tokens,
                cache_read_tokens: cu.cache_read_tokens,
                cache_creation_tokens: cu.cache_creation_tokens,
                cache_creation_5m_tokens: cu.cache_creation_5m_tokens,
                cache_creation_1h_tokens: cu.cache_creation_1h_tokens,
                web_search_requests: cu.web_search_requests,
            },
        );
        state
            .model_counts
            .insert(cu.model.clone(), cu.request_count);
    }
    let usages = extract_usage_from(session, from_byte, &mut state)?;
    Ok((usages, state))
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CodexIncrementalState {
    pub prev_total: Option<CachedCodexTotal>,
    pub current_model: String,
    pub model_usages: HashMap<String, TokenUsage>,
    pub model_counts: HashMap<String, i64>,
}

fn extract_usage_from(
    session: &SessionMeta,
    from_byte: u64,
    state: &mut CodexIncrementalState,
) -> Result<Vec<UsageSummary>> {
    let mut reader = std::io::BufReader::new(std::fs::File::open(&session.source_path)?);
    if from_byte > 0 {
        reader.seek(SeekFrom::Start(from_byte))?;
    }
    let mut prev_total: Option<CodexTokenUsage> =
        state.prev_total.as_ref().map(|ct| CodexTokenUsage {
            input_tokens: Some(ct.input_tokens),
            output_tokens: Some(ct.output_tokens),
            cached_input_tokens: Some(ct.cached_input_tokens),
            reasoning_output_tokens: Some(ct.reasoning_output_tokens),
        });
    let mut current_model = if state.current_model.is_empty() {
        "unknown".to_string()
    } else {
        state.current_model.clone()
    };
    for line in reader.lines() {
        let line = line?;
        if !line.contains("turn_context") && !line.contains("token_count") {
            continue;
        }
        let value: serde_json::Value = match sonic_rs::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let event_type = match value.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };
        match event_type {
            "turn_context" => {
                if let Some(p) = value.get("payload") {
                    if let Some(model) = p
                        .get("model")
                        .or_else(|| p.get("info").and_then(|i| i.get("model")))
                        .and_then(|v| v.as_str())
                    {
                        current_model = normalize_codex_model(model);
                    }
                }
            }
            "event_msg" => {
                let payload = match value.get("payload") {
                    Some(p) => p,
                    None => continue,
                };
                if payload.get("type").and_then(|v| v.as_str()) != Some("token_count") {
                    continue;
                }
                let info = match payload.get("info") {
                    Some(i) if !i.is_null() => i,
                    _ => continue,
                };
                if let Some(model) = info
                    .get("model")
                    .or_else(|| info.get("model_name"))
                    .or_else(|| payload.get("model"))
                    .and_then(|v| v.as_str())
                {
                    current_model = normalize_codex_model(model);
                }
                let (token_usage, is_total) = if let Some(total) = info.get("total_token_usage") {
                    (parse_token_usage_from_json(total), true)
                } else if let Some(last) = info.get("last_token_usage") {
                    (parse_token_usage_from_json(last), false)
                } else {
                    continue;
                };
                let current = match token_usage {
                    Some(c) => c,
                    None => continue,
                };
                let delta = if is_total {
                    let d = compute_delta_from_prev(&prev_total, &current);
                    prev_total = Some(current);
                    d
                } else {
                    current.into_token_usage()
                };
                if delta.is_empty() {
                    continue;
                }
                *state.model_usages.entry(current_model.clone()).or_default() += delta;
                *state.model_counts.entry(current_model.clone()).or_default() += 1;
            }
            _ => {}
        }
    }
    state.prev_total = prev_total.as_ref().map(|p| p.to_cached_total());
    state.current_model = current_model;
    Ok(super::usages_from_maps(
        &session.tool,
        state.model_usages.clone(),
        &state.model_counts,
        session.last_active_at,
    ))
}

fn parse_token_usage_from_json(value: &serde_json::Value) -> Option<CodexTokenUsage> {
    if value.is_null() || !value.is_object() {
        return None;
    }
    Some(CodexTokenUsage {
        input_tokens: value.get("input_tokens").and_then(|v| v.as_i64()),
        output_tokens: value.get("output_tokens").and_then(|v| v.as_i64()),
        cached_input_tokens: value
            .get("cached_input_tokens")
            .or_else(|| value.get("cache_read_input_tokens"))
            .and_then(|v| v.as_i64()),
        reasoning_output_tokens: value
            .get("reasoning_output_tokens")
            .and_then(|v| v.as_i64()),
    })
}

fn compute_delta_from_prev(
    prev: &Option<CodexTokenUsage>,
    current: &CodexTokenUsage,
) -> TokenUsage {
    match prev {
        None => TokenUsage {
            input_tokens: current.input_tokens.unwrap_or(0),
            output_tokens: current.output_tokens.unwrap_or(0)
                + current.reasoning_output_tokens.unwrap_or(0),
            cache_read_tokens: current.cached_input_tokens.unwrap_or(0),
            cache_creation_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            web_search_requests: 0,
        },
        Some(p) => {
            let input = current
                .input_tokens
                .unwrap_or(0)
                .saturating_sub(p.input_tokens.unwrap_or(0));
            let cached = current
                .cached_input_tokens
                .unwrap_or(0)
                .saturating_sub(p.cached_input_tokens.unwrap_or(0))
                .min(input);
            TokenUsage {
                input_tokens: input,
                output_tokens: current
                    .output_tokens
                    .unwrap_or(0)
                    .saturating_sub(p.output_tokens.unwrap_or(0))
                    + current
                        .reasoning_output_tokens
                        .unwrap_or(0)
                        .saturating_sub(p.reasoning_output_tokens.unwrap_or(0)),
                cache_read_tokens: cached,
                cache_creation_tokens: 0,
                cache_creation_5m_tokens: 0,
                cache_creation_1h_tokens: 0,
                web_search_requests: 0,
            }
        }
    }
}

fn normalize_codex_model(raw: &str) -> String {
    let mut name = raw.to_lowercase();
    if let Some(pos) = name.rfind('/') {
        name = name[pos + 1..].to_string();
    }
    // Only strip date-like suffixes from ASCII model names
    if name.is_ascii() && name.len() > 11 {
        let suffix = &name[name.len() - 11..];
        if suffix.as_bytes()[0] == b'-'
            && suffix[1..5].chars().all(|c| c.is_ascii_digit())
            && suffix.as_bytes()[5] == b'-'
            && suffix[6..8].chars().all(|c| c.is_ascii_digit())
            && suffix.as_bytes()[8] == b'-'
            && suffix[9..11].chars().all(|c| c.is_ascii_digit())
        {
            name.truncate(name.len() - 11);
        }
    }
    if name.len() > 9 {
        let parts: Vec<&str> = name.rsplitn(2, '-').collect();
        if parts.len() == 2 {
            if let Some(suffix) = parts.first() {
                if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
                    name = parts[1].to_string();
                }
            }
        }
    }
    name
}

fn extract_codex_user_text(content: Option<&serde_json::Value>) -> Option<String> {
    super::extract_text_from_json(content, "input_text", |s| super::simple_clean_text(s, 80))
}
