/// 通用 JSON 解析管线
/// 由 SessionMappingConfig.json 配置驱动，支持 Droid, Mux, Amp 等工具
/// 三种策略：flat（单条记录）, messages（迭代消息数组）, by_model（迭代模型哈希表）

use crate::adapter::mapping::JsonParseConfig;
use crate::session::model::{self, SessionMeta, TokenUsage, UsageSummary};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

pub(crate) fn scan_sessions(
    config_dir: &Path,
    mapping: &crate::adapter::mapping::ToolMapping,
    min_mtime_ms: Option<i64>,
) -> Result<Vec<SessionMeta>> {
    let session = &mapping.session;
    let session_dir = config_dir.join(session.path());

    let json_config = match &session.json {
        Some(c) => c,
        None => return Ok(Vec::new()),
    };

    let glob = &session.glob;
    let pattern = if glob.is_empty() {
        "*.json"
    } else {
        glob.as_str()
    };

    super::scan_sessions_with_filter(
        &session_dir,
        min_mtime_ms,
        |e| {
            let name = e.file_name().unwrap_or_default().to_string_lossy();
            name.ends_with(".json") && match pattern {
                "*.json" => true,
                p => name.contains(p.trim_start_matches('*').trim_end_matches('*')),
            }
        },
        |path| parse_session_meta(path, mapping, json_config),
    )
}

fn parse_session_meta(
    path: &Path,
    mapping: &crate::adapter::mapping::ToolMapping,
    config: &JsonParseConfig,
) -> Option<SessionMeta> {
    let val: serde_json::Value = super::read_json(path)?;

    let mut sid = None;
    let mut title = None;
    let mut cwd = None;
    let mut created_at = None;
    let mut last_active_at = None;

    // Session ID
    if let Some(p) = &config.session_id_path {
        sid = json_get_str(&val, p).map(|s| s.to_string());
    }
    if sid.is_none() {
        sid = super::fallback_session_id(path);
    }

    // Title
    if let Some(p) = &config.title_path {
        title = json_get_str(&val, p).map(|s| s.to_string());
    }

    // Project dir
    if let Some(p) = &config.project_dir_path {
        cwd = json_get_str(&val, p).map(|s| s.to_string());
    }

    // Timestamps
    if let Some(p) = &config.created_at_path {
        if let Some(ts_str) = json_get_str(&val, p) {
            created_at = super::parse_iso_timestamp(ts_str);
        } else if let Some(ts_ms) = json_get_i64(&val, p) {
            if ts_ms > 0 {
                created_at = Some(normalize_ts(ts_ms));
            }
        }
    }
    if let Some(p) = &config.last_active_at_path {
        if let Some(ts_str) = json_get_str(&val, p) {
            last_active_at = super::parse_iso_timestamp(ts_str);
        } else if let Some(ts_ms) = json_get_i64(&val, p) {
            if ts_ms > 0 {
                last_active_at = Some(normalize_ts(ts_ms));
            }
        }
    }

    let sid = sid?;
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

pub(crate) struct ExtractResult {
    pub usages: Vec<UsageSummary>,
}

pub(crate) fn extract_usage(
    session: &SessionMeta,
    config: &JsonParseConfig,
) -> Result<ExtractResult> {
    let val: serde_json::Value = match super::read_json(&session.source_path) {
        Some(v) => v,
        None => return Ok(ExtractResult { usages: Vec::new() }),
    };

    let usages = match config.strategy.as_str() {
        "flat" => extract_flat(&val, config, session),
        "messages" => extract_messages(&val, config, session),
        "by_model" => extract_by_model(&val, config, session),
        _ => Vec::new(),
    };

    Ok(ExtractResult { usages })
}

/// flat 策略：整个文件就是一条记录（Droid）
fn extract_flat(
    val: &serde_json::Value,
    config: &JsonParseConfig,
    session: &SessionMeta,
) -> Vec<UsageSummary> {
    let usage = extract_token_usage(val, &config.token_map);
    if usage.is_empty() {
        return Vec::new();
    }

    let model = config
        .model_path
        .as_ref()
        .and_then(|p| json_get_str(val, p))
        .unwrap_or(&config.default_model);
    let model = normalize_model(model, &config.model_normalize);

    if model.is_empty() || model.starts_with('<') {
        return Vec::new();
    }

    let cost_usd = config
        .cost_path
        .as_ref()
        .and_then(|p| json_get_f64(val, p))
        .map(|v| v.max(0.0));

    let timestamp_ms = extract_timestamp_ms(val, &config.timestamp_path)
        .or(session.last_active_at)
        .unwrap_or(0);

    let date = if timestamp_ms > 0 {
        model::ms_to_date(timestamp_ms)
    } else {
        session
            .last_active_at
            .map(model::ms_to_date)
            .unwrap_or_default()
    };

    vec![UsageSummary {
        tool: session.tool.clone(),
        model,
        usage,
        request_count: 1,
        date: if date.is_empty() { None } else { Some(date) },
        cost_usd,
    }]
}

/// messages 策略：迭代消息数组，每条按 filter 过滤后提取（Amp）
fn extract_messages(
    val: &serde_json::Value,
    config: &JsonParseConfig,
    session: &SessionMeta,
) -> Vec<UsageSummary> {
    let messages = match &config.messages_path {
        Some(p) => val.pointer(&json_pointer(p)),
        None => val.get("messages"),
    };
    let messages = match messages.and_then(|v| v.as_array()) {
        Some(m) => m,
        None => return Vec::new(),
    };

    struct MsgEntry {
        model: String,
        usage: TokenUsage,
        cost_usd: Option<f64>,
        timestamp_ms: i64,
    }

    let mut entries: Vec<MsgEntry> = Vec::new();

    for msg in messages {
        // 应用 message_filter
        if !config.message_filter.is_empty() && !matches_filter(msg, &config.message_filter) {
            continue;
        }

        let usage = extract_token_usage(msg, &config.token_map);
        if usage.is_empty() {
            continue;
        }

        let model = config
            .model_path
            .as_ref()
            .and_then(|p| json_get_str(msg, p))
            .unwrap_or(&config.default_model);
        let model = normalize_model(model, &config.model_normalize);

        if model.is_empty() || model.starts_with('<') {
            continue;
        }

        let cost_usd = config
            .cost_path
            .as_ref()
            .and_then(|p| json_get_f64(msg, p))
            .map(|v| v.max(0.0));

        let timestamp_ms = extract_timestamp_ms(msg, &config.timestamp_path)
            .or(session.last_active_at)
            .unwrap_or(0);

        entries.push(MsgEntry {
            model,
            usage,
            cost_usd,
            timestamp_ms,
        });
    }

    // 聚合
    struct Agg {
        usage: TokenUsage,
        count: i64,
        cost: f64,
    }
    let mut agg: HashMap<(String, String), Agg> = HashMap::new();

    for entry in &entries {
        let date = if entry.timestamp_ms > 0 {
            model::ms_to_date(entry.timestamp_ms)
        } else {
            session
                .last_active_at
                .map(model::ms_to_date)
                .unwrap_or_default()
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
        .map(|((model, date), a)| UsageSummary {
            tool: session.tool.clone(),
            model,
            usage: a.usage,
            request_count: a.count,
            date: if date.is_empty() { None } else { Some(date) },
            cost_usd: if a.cost > 0.0 { Some(a.cost) } else { None },
        })
        .collect()
}

/// by_model 策略：迭代 byModel 哈希表，每个 key 为模型名（Mux）
fn extract_by_model(
    val: &serde_json::Value,
    config: &JsonParseConfig,
    session: &SessionMeta,
) -> Vec<UsageSummary> {
    let by_model = match &config.by_model_path {
        Some(p) => val.pointer(&json_pointer(p)),
        None => return Vec::new(),
    };
    let by_model = match by_model.and_then(|v| v.as_object()) {
        Some(m) => m,
        None => return Vec::new(),
    };

    let timestamp_ms = config
        .by_model_timestamp_path
        .as_ref()
        .and_then(|p| json_get_i64(val, p))
        .map(normalize_ts)
        .or(session.last_active_at)
        .unwrap_or(0);

    let date = if timestamp_ms > 0 {
        model::ms_to_date(timestamp_ms)
    } else {
        session
            .last_active_at
            .map(model::ms_to_date)
            .unwrap_or_default()
    };

    let mut results = Vec::new();

    for (model_key, bucket) in by_model {
        let model_name = if config.by_model_provider_prefix {
            // "anthropic:claude-opus-4-6" → 取第一个冒号之后的部分
            model_key.split_once(':').map(|(_, m)| m).unwrap_or(model_key)
        } else {
            model_key
        };
        let model_name = normalize_model(model_name, &config.model_normalize);

        if model_name.is_empty() || model_name.starts_with('<') {
            continue;
        }

        // 使用 by_model_token_map 提取 token
        let usage = if !config.by_model_token_map.is_empty() {
            extract_token_usage_by_model(bucket, &config.by_model_token_map)
        } else {
            extract_token_usage(bucket, &config.token_map)
        };

        if usage.is_empty() {
            continue;
        }

        // 提取 cost
        let cost_usd = if let Some(cost_path) = &config.by_model_cost_path {
            // cost_path 可能指向一个含 cost_usd 的子对象
            let cost_val = bucket.pointer(&json_pointer(cost_path));
            cost_val.and_then(|v| v.as_f64()).map(|v| v.max(0.0))
        } else {
            // 对 by_model，尝试累加所有 bucket 的 cost_usd
            let mut total_cost = 0.0;
            if let Some(obj) = bucket.as_object() {
                for sub in obj.values() {
                    if let Some(sub_obj) = sub.as_object() {
                        if let Some(c) = sub_obj.get("cost_usd").and_then(|v| v.as_f64()) {
                            total_cost += c.max(0.0);
                        }
                    }
                }
            }
            if total_cost > 0.0 { Some(total_cost) } else { None }
        };

        results.push(UsageSummary {
            tool: session.tool.clone(),
            model: model_name.to_string(),
            usage,
            request_count: 1,
            date: if date.is_empty() { None } else { Some(date.clone()) },
            cost_usd,
        });
    }

    results
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
    current
        .as_i64()
        .or_else(|| current.as_f64().map(|f| f as i64))
        .or_else(|| current.as_str().and_then(|s| s.parse().ok()))
}

/// 按 token_map 从 JSON 对象提取 TokenUsage
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
            "reasoning" => {} // TokenUsage 没有直接的 reasoning 字段
            _ => {}
        }
    }
    usage
}

/// 按 by_model_token_map 从 byModel 子对象提取 TokenUsage
/// 每个 bucket 可能是 {tokens: N, cost_usd: F} 格式
fn extract_token_usage_by_model(
    bucket: &serde_json::Value,
    token_map: &HashMap<String, String>,
) -> TokenUsage {
    let mut usage = TokenUsage::default();
    for (field, path) in token_map {
        // path 格式: "input.tokens" 表示 bucket.input.tokens
        let v = match json_get_i64(bucket, path) {
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

/// 提取时间戳（支持 ISO 字符串和毫秒整数）
fn extract_timestamp_ms(val: &serde_json::Value, path: &Option<String>) -> Option<i64> {
    let p = path.as_ref()?;
    if let Some(ts_str) = json_get_str(val, p) {
        super::parse_iso_timestamp(ts_str)
    } else {
        json_get_i64(val, p).map(normalize_ts)
    }
}

/// 时间戳归一化：如果 < 1e12 视为秒级，否则为毫秒级
fn normalize_ts(ts: i64) -> i64 {
    if ts > 0 && ts < 1_000_000_000_000 {
        ts * 1000
    } else {
        ts
    }
}

/// 模型名归一化
fn normalize_model(model: &str, rules: &str) -> String {
    if rules.is_empty() {
        return model.to_string();
    }
    let mut m = model.to_string();
    for rule in rules.split(',') {
        let rule = rule.trim();
        match rule {
            "strip_prefix" => {
                // 去掉 "custom:" 等前缀
                if let Some(idx) = m.find(':') {
                    m = m[idx + 1..].to_string();
                }
            }
            "strip_brackets" => {
                // 去掉 [...] 内容
                let mut result = String::with_capacity(m.len());
                let mut in_bracket = false;
                for ch in m.chars() {
                    match ch {
                        '[' => in_bracket = true,
                        ']' => in_bracket = false,
                        _ if !in_bracket => result.push(ch),
                        _ => {}
                    }
                }
                m = result;
            }
            "lowercase" => {
                m = m.to_lowercase();
            }
            "dot_to_dash" => {
                m = m.replace('.', "-");
            }
            "collapse_dashes" => {
                while m.contains("--") {
                    m = m.replace("--", "-");
                }
            }
            _ => {}
        }
    }
    m.trim_matches('-').to_string()
}

/// 将点分路径转为 JSON Pointer（"a.b.c" → "/a/b/c"）
fn json_pointer(path: &str) -> String {
    let mut ptr = String::with_capacity(path.len() + 16);
    for part in path.split('.') {
        ptr.push('/');
        ptr.push_str(part);
    }
    ptr
}
