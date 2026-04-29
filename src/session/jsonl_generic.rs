/// 通用 JSONL 解析管线
/// 由 SessionMappingConfig.jsonl 配置驱动，支持 Qwen, Pi, OpenClaw, Copilot 等工具
/// 核心流程：逐行读取 JSONL → 按 filter 过滤 → 按 token_map 提取字段 → 按 (model, date) 聚合
/// 支持增量解析：利用缓存的 last_byte_offset 只解析新增部分

use crate::adapter::mapping::JsonlParseConfig;
use crate::session::cache::SubagentFileState;
use crate::session::model::{self, SessionMeta, TokenUsage, UsageSummary};
use anyhow::Result;
use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::Path;

pub(crate) fn scan_sessions(
    config_dir: &Path,
    mapping: &crate::adapter::mapping::ToolMapping,
    min_mtime_ms: Option<i64>,
) -> Result<Vec<SessionMeta>> {
    let session = &mapping.session;
    let session_dir = config_dir.join(session.path());

    let jsonl_config = match &session.jsonl {
        Some(c) => c,
        None => return Ok(Vec::new()),
    };

    super::scan_sessions_with_filter(
        &session_dir,
        min_mtime_ms,
        |e| {
            let ext = e.extension().map_or(false, |e| e == "jsonl");
            ext
        },
        |path| parse_session_meta(path, mapping, jsonl_config),
    )
}

fn parse_session_meta(
    path: &Path,
    mapping: &crate::adapter::mapping::ToolMapping,
    config: &JsonlParseConfig,
) -> Option<SessionMeta> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();

    let mut sid = None;
    let mut title = None;
    let mut cwd = None;
    let mut created_at = None;
    let mut last_active_at = None;
    let mut user_texts: Vec<String> = Vec::new();

    // 读前 30 行提取元数据
    for _ in 0..30 {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let val: serde_json::Value = sonic_rs::from_str(trimmed).ok()?;

        // Session ID
        if sid.is_none() {
            if let Some(p) = &config.session_id_path {
                sid = json_get_str(&val, p).map(|s| s.to_string());
            }
        }

        // Title
        if title.is_none() {
            if let Some(p) = &config.title_path {
                title = json_get_str(&val, p).map(|s| s.to_string());
            }
        }

        // CWD / project_dir
        if cwd.is_none() {
            if let Some(p) = &config.cwd_path {
                cwd = json_get_str(&val, p).map(|s| s.to_string());
            }
        }

        // Timestamps
        if let Some(p) = &config.timestamp_path {
            if let Some(ts_str) = json_get_str(&val, p) {
                if let Some(ts) = super::parse_iso_timestamp(ts_str) {
                    if created_at.is_none() {
                        created_at = Some(ts);
                    }
                    last_active_at = Some(ts);
                }
            }
        }

        // Extract user messages for fallback title
        if user_texts.len() < 3 {
            let role = json_get_str(&val, "role").unwrap_or("");
            let line_type = json_get_str(&val, "type").unwrap_or("");
            if role == "user" || line_type == "user" {
                if let Some(text) = extract_user_text(&val) {
                    if !text.starts_with('<') && !text.starts_with('[') {
                        user_texts.push(text);
                    }
                }
            }
        }
    }

    if sid.is_none() {
        sid = super::fallback_session_id(path);
    }
    let sid = sid?;
    let title = title.or_else(|| super::fallback_title(&user_texts, cwd.as_deref()));

    Some(super::build_session_meta(
        mapping,
        path,
        sid,
        title,
        None,
        cwd,
        created_at,
        last_active_at,
    ))
}

fn extract_user_text(val: &serde_json::Value) -> Option<String> {
    let content = val.get("message")?.get("content")?;
    super::extract_text_from_json(Some(content), "text", |s| {
        super::truncate_str(s.trim(), 80)
    })
}

pub(crate) struct ExtractResult {
    pub usages: Vec<UsageSummary>,
    pub first_byte_offset: u64,
    pub subagent_files: HashMap<String, SubagentFileState>,
    pub tool_state: Option<serde_json::Value>,
}

/// JSONL 增量状态，序列化存入 CachedUsageData.tool_state
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
pub(crate) struct JsonlIncrementalState {
    /// 当前模型名（用于 model_change 跟踪）
    pub current_model: String,
}

pub(crate) fn extract_usage(
    session: &SessionMeta,
    config: &JsonlParseConfig,
) -> Result<ExtractResult> {
    let file = std::fs::File::open(&session.source_path)?;
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);

    let (messages, _first_ts, state) =
        parse_lines(&mut reader, 0, config)?;

    let usages = summarize_entries(
        &session.tool,
        &messages,
        session.last_active_at,
    );

    Ok(ExtractResult {
        usages,
        first_byte_offset: 0,
        subagent_files: HashMap::new(),
        tool_state: serde_json::to_value(&state).ok(),
    })
}

/// 增量解析：从 from_byte 开始读取新增内容，与缓存 daily 数据合并
pub(crate) fn extract_usage_incremental(
    session: &SessionMeta,
    config: &JsonlParseConfig,
    from_byte: u64,
    prev_state: &JsonlIncrementalState,
) -> Result<(Vec<UsageSummary>, u64, JsonlIncrementalState)> {
    let file = std::fs::File::open(&session.source_path)?;
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
    if from_byte > 0 {
        reader.seek(SeekFrom::Start(from_byte))?;
    }

    // 从 prev_state 恢复 current_model
    let mut patched_config = config.clone();
    if !prev_state.current_model.is_empty() {
        patched_config.default_model = prev_state.current_model.clone();
    }

    let (messages, _first_ts, state) =
        parse_lines(&mut reader, from_byte, &patched_config)?;

    // 只聚合增量部分的结果
    let incremental_usages = summarize_entries(
        &session.tool,
        &messages,
        session.last_active_at,
    );

    Ok((incremental_usages, from_byte, state))
}

/// 核心：逐行解析 JSONL 文件（支持从指定偏移开始）
fn parse_lines(
    reader: &mut std::io::BufReader<std::fs::File>,
    from_byte: u64,
    config: &JsonlParseConfig,
) -> Result<(HashMap<String, MsgEntry>, Option<i64>, JsonlIncrementalState)> {
    let mut messages: HashMap<String, MsgEntry> = HashMap::new();
    let mut line_buf = String::new();
    let mut current_model = config.default_model.clone();
    let mut first_timestamp_ms: Option<i64> = None;
    let mut skip_first = from_byte > 0; // 增量时跳过第一行（可能不完整）

    loop {
        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        if skip_first {
            skip_first = false;
            continue;
        }
        let line = line_buf.trim();
        if line.is_empty() {
            continue;
        }

        let val: serde_json::Value = match sonic_rs::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // 检查是否为 model_change 事件
        if !config.model_change_filter.is_empty() {
            if matches_filter(&val, &config.model_change_filter) {
                if let Some(p) = &config.model_change_path {
                    if let Some(m) = json_get_str(&val, p) {
                        current_model = m.to_string();
                    }
                }
                continue;
            }
        }

        // 检查是否匹配 filter
        if !config.filter.is_empty() && !matches_filter(&val, &config.filter) {
            continue;
        }

        // 提取模型名
        let model = config
            .model_path
            .as_ref()
            .and_then(|p| json_get_str(&val, p))
            .unwrap_or(&current_model)
            .to_string();

        if model.is_empty() || model.starts_with('<') {
            continue;
        }

        // 提取 token 字段
        let usage = extract_token_usage(&val, &config.token_map);

        // 跳过全零条目
        if usage.is_empty() {
            continue;
        }

        // 提取 cost
        let cost_usd = config
            .cost_path
            .as_ref()
            .and_then(|p| json_get_f64(&val, p))
            .map(|v| v.max(0.0));

        // 提取时间戳
        let timestamp_ms = config
            .timestamp_path
            .as_ref()
            .and_then(|p| json_get_str(&val, p))
            .and_then(super::parse_iso_timestamp)
            .unwrap_or(0);

        if first_timestamp_ms.is_none() && timestamp_ms > 0 {
            first_timestamp_ms = Some(timestamp_ms);
        }

        // 去重键
        let dedup_key = config
            .dedup_key_paths
            .as_ref()
            .and_then(|paths| build_dedup_key(&val, paths));

        // Cache read 归一化（Copilot OTEL: input 包含 cache_read）
        let usage = if config.normalize_cache_read {
            let mut u = usage;
            u.input_tokens = (u.input_tokens - u.cache_read_tokens).max(0);
            u
        } else {
            usage
        };

        let entry = MsgEntry {
            model,
            usage,
            cost_usd,
            timestamp_ms,
        };

        if let Some(key) = dedup_key {
            let old = messages.get(&key);
            if should_replace(old, &entry) {
                messages.insert(key, entry);
            }
        } else {
            // 无 dedup key 时按 model+timestamp 去重
            let key = format!(
                "{}:{}",
                entry.model,
                entry.timestamp_ms
            );
            let old = messages.get(&key);
            if should_replace(old, &entry) {
                messages.insert(key, entry);
            }
        }
    }

    let state = JsonlIncrementalState {
        current_model,
    };

    Ok((messages, first_timestamp_ms, state))
}

// ── 内部类型 ──

struct MsgEntry {
    model: String,
    usage: TokenUsage,
    cost_usd: Option<f64>,
    timestamp_ms: i64,
}

// ── 辅助函数 ──

/// 检查 JSON 值是否匹配所有 filter 条件
fn matches_filter(val: &serde_json::Value, filter: &HashMap<String, String>) -> bool {
    for (key, expected) in filter {
        let actual = json_get_str(val, key).unwrap_or("");
        if actual != expected.as_str() {
            return false;
        }
    }
    true
}

/// 从 JSON 值按点分路径提取字符串
/// 支持 "model", "message.model", "attributes.gen_ai.request.model" 等
fn json_get_str<'a>(val: &'a serde_json::Value, path: &str) -> Option<&'a str> {
    let mut current = val;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    current.as_str()
}

/// 从 JSON 值按点分路径提取 f64
fn json_get_f64(val: &serde_json::Value, path: &str) -> Option<f64> {
    let mut current = val;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    current.as_f64()
}

/// 从 JSON 值按点分路径提取 i64
fn json_get_i64(val: &serde_json::Value, path: &str) -> Option<i64> {
    let mut current = val;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    // 某些工具的 token 数是字符串编码的数字
    current
        .as_i64()
        .or_else(|| current.as_f64().map(|f| f as i64))
        .or_else(|| current.as_str().and_then(|s| s.parse().ok()))
}

/// 按 token_map 从 JSON 行提取 TokenUsage
fn extract_token_usage(val: &serde_json::Value, token_map: &HashMap<String, String>) -> TokenUsage {
    let mut usage = TokenUsage::default();
    for (field, path) in token_map {
        let v = match json_get_i64(val, path) {
            Some(v) => v,
            None => continue,
        };
        match field.as_str() {
            "input" => usage.input_tokens = v,
            "output" => usage.output_tokens = v,
            "cache_read" => usage.cache_read_tokens = v,
            "cache_creation" => usage.cache_creation_tokens = v,
            "reasoning" => {}
            _ => {}
        }
    }
    usage
}

/// 构建去重键（如 "traceId:spanId"）
fn build_dedup_key(val: &serde_json::Value, paths: &str) -> Option<String> {
    let parts: Vec<&str> = paths.split(':').collect();
    let mut key_parts = Vec::with_capacity(parts.len());
    for part in parts {
        key_parts.push(json_get_str(val, part)?.to_string());
    }
    Some(key_parts.join(":"))
}

/// 判断新 entry 是否应替换旧 entry
fn should_replace(old: Option<&MsgEntry>, new: &MsgEntry) -> bool {
    match old {
        None => true,
        Some(old) => new.usage.output_tokens > old.usage.output_tokens,
    }
}

/// 将 MsgEntry 聚合为 Vec<UsageSummary>
fn summarize_entries(
    tool: &str,
    messages: &HashMap<String, MsgEntry>,
    last_active_at: Option<i64>,
) -> Vec<UsageSummary> {
    struct Agg {
        usage: TokenUsage,
        count: i64,
        cost: f64,
    }
    let mut agg: HashMap<(String, String), Agg> = HashMap::new();

    for entry in messages.values() {
        let date = if entry.timestamp_ms > 0 {
            model::ms_to_date(entry.timestamp_ms)
        } else {
            last_active_at.map(model::ms_to_date).unwrap_or_default()
        };
        let key = (entry.model.clone(), date);
        let a = agg.entry(key).or_insert_with(|| Agg {
            usage: TokenUsage::default(),
            count: 0,
            cost: 0.0,
        });
        a.usage.add_assign_from(&entry.usage);
        a.count += 1;
        if let Some(c) = entry.cost_usd {
            a.cost += c;
        }
    }

    agg.into_iter()
        .map(|((model, date), a)| {
            let date_opt = if date.is_empty() {
                None
            } else {
                Some(date)
            };
            UsageSummary {
                tool: tool.to_string(),
                model,
                usage: a.usage,
                request_count: a.count,
                date: date_opt,
                cost_usd: if a.cost > 0.0 { Some(a.cost) } else { None },
            }
        })
        .collect()
}
