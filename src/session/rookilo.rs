/// RooCode / KiloCode 自定义解析模块
/// 数据格式：VS Code globalStorage 目录下的双文件结构
/// - ui_messages.json: 主会话文件，含双编码 JSON（text 字段内含转义 JSON 字符串）
/// - api_conversation_history.json: 伴随文件，含模型/agent 信息（XML 标签提取）
///
/// 关键特征：
/// - text 字段是双编码 JSON：外层是字符串，内层是 JSON 对象
/// - token/cost 数据在 api_req_started 事件的 text 字段中
/// - model 来自伴随文件中的 <model> XML 标签
/// - 两个工具（RooCode/KiloCode）共享相同解析逻辑，仅 client 名不同

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

    super::scan_sessions_with_filter(
        &session_dir,
        min_mtime_ms,
        |e| {
            e.file_name()
                .is_some_and(|name| name == "ui_messages.json")
        },
        |path| parse_session_meta(path, mapping),
    )
}

fn parse_session_meta(
    path: &Path,
    mapping: &crate::adapter::mapping::ToolMapping,
) -> Option<SessionMeta> {
    // Session ID 来自目录名（tasks/<taskId>/ui_messages.json）
    let sid = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|f| f.to_string_lossy().to_string())?;

    let created_at = super::meta_mtime_ms(
        &std::fs::metadata(path).ok()?,
    );
    if created_at == 0 {
        return None;
    }

    // 尝试从伴随文件提取 model
    let companion = path.parent()?.join("api_conversation_history.json");
    let (_model, _agent) = read_task_metadata(&companion);

    Some(super::build_session_meta(
        mapping,
        path,
        sid,
        None,       // title
        None,       // summary
        None,       // cwd
        Some(created_at),
        Some(created_at),
    ))
}

pub(crate) fn extract_usage(
    session: &SessionMeta,
) -> Result<Vec<UsageSummary>> {
    let path = &session.source_path;

    // 读取伴随文件获取 model
    let companion = path
        .parent()
        .map(|p| p.join("api_conversation_history.json"));
    let (model, _agent) = match &companion {
        Some(p) if p.exists() => read_task_metadata(p),
        _ => ("unknown".to_string(), None),
    };

    // 读取 ui_messages.json
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()),
    };

    let entries: Vec<serde_json::Value> = match sonic_rs::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Ok(Vec::new()),
    };

    let mut usages: Vec<UsageSummary> = Vec::new();

    for entry in &entries {
        let msg_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let say = entry.get("say").and_then(|v| v.as_str()).unwrap_or("");

        if msg_type != "say" || say != "api_req_started" {
            continue;
        }

        let text = match entry.get("text").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };

        // 双编码 JSON：text 字段是转义的 JSON 字符串
        let payload: serde_json::Value = match sonic_rs::from_str(text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let usage = TokenUsage {
            input_tokens: extract_i64(&payload, "tokensIn"),
            output_tokens: extract_i64(&payload, "tokensOut"),
            cache_read_tokens: extract_i64(&payload, "cacheReads"),
            cache_creation_tokens: extract_i64(&payload, "cacheWrites"),
            ..Default::default()
        };

        if usage.is_empty() {
            continue;
        }

        let cost_usd = extract_f64(&payload, "cost").map(|v| v.max(0.0));

        // 提取时间戳
        let timestamp_ms = entry
            .get("ts")
            .and_then(|v| v.as_str())
            .and_then(super::parse_iso_timestamp)
            .unwrap_or(0);

        let date = if timestamp_ms > 0 {
            model::ms_to_date(timestamp_ms)
        } else {
            session
                .last_active_at
                .map(model::ms_to_date)
                .unwrap_or_default()
        };

        usages.push(UsageSummary {
            tool: session.tool.clone(),
            model: model.clone(),
            usage,
            request_count: 1,
            date: if date.is_empty() { None } else { Some(date) },
            cost_usd,
        });
    }

    // 按 (model, date) 聚合
    let merged = merge_usages(&usages);
    Ok(merged)
}

/// 从伴随文件 api_conversation_history.json 提取 model 和 agent
/// 方法：扫描原始文本中的 <model> 和 <slug>/<name> XML 标签
fn read_task_metadata(path: &Path) -> (String, Option<String>) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return ("unknown".to_string(), None),
    };

    let mut last_model = None;
    let mut last_slug = None;
    let mut last_name = None;

    // 查找所有 <environment_details>...</environment_details> 块
    let mut pos = 0;
    while let Some(start) = content[pos..].find("<environment_details>") {
        pos += start;
        if let Some(end) = content[pos..].find("</environment_details>") {
            let block = &content[pos..pos + end];
            if let Some(m) = extract_xml_tag(block, "model") {
                last_model = Some(m);
            }
            if let Some(s) = extract_xml_tag(block, "slug") {
                last_slug = Some(s);
            }
            if let Some(n) = extract_xml_tag(block, "name") {
                last_name = Some(n);
            }
            pos += end + 22; // 跳过 </environment_details>
        } else {
            pos += 20;
        }
    }

    let model = last_model.unwrap_or_else(|| "unknown".to_string());
    let agent = last_slug.or(last_name);
    (model, agent)
}

/// 从文本中提取 XML 标签内容
fn extract_xml_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = text.find(&open)?;
    let content_start = start + open.len();
    let end = text[content_start..].find(&close)?;
    Some(text[content_start..content_start + end].trim().to_string())
}

/// 从 JSON 值提取 i64（支持 i64/u64/f64/string）
fn extract_i64(val: &serde_json::Value, key: &str) -> i64 {
    match val.get(key) {
        Some(v) => v
            .as_i64()
            .or_else(|| v.as_f64().map(|f| f as i64))
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0)
            .max(0),
        None => 0,
    }
}

/// 从 JSON 值提取 f64（支持 f64/i64/u64/string）
fn extract_f64(val: &serde_json::Value, key: &str) -> Option<f64> {
    match val.get(key) {
        Some(v) => v
            .as_f64()
            .or_else(|| v.as_i64().map(|i| i as f64))
            .or_else(|| v.as_str().and_then(|s| s.parse().ok())),
        None => None,
    }
}

/// 按 (model, date) 聚合 UsageSummary
fn merge_usages(usages: &[UsageSummary]) -> Vec<UsageSummary> {
    struct Agg {
        usage: TokenUsage,
        count: i64,
        cost: f64,
    }
    let mut agg: HashMap<(String, String), Agg> = HashMap::new();

    for u in usages {
        let date = u.date.as_deref().unwrap_or("");
        let key = (u.model.clone(), date.to_string());
        let a = agg.entry(key).or_insert_with(|| Agg {
            usage: TokenUsage::default(),
            count: 0,
            cost: 0.0,
        });
        a.usage.add_assign_from(&u.usage);
        a.count += 1;
        if let Some(c) = u.cost_usd {
            a.cost += c;
        }
    }

    agg.into_iter()
        .map(|((model, date), a)| UsageSummary {
            tool: usages.first().map(|u| u.tool.clone()).unwrap_or_default(),
            model,
            usage: a.usage,
            request_count: a.count,
            date: if date.is_empty() { None } else { Some(date) },
            cost_usd: if a.cost > 0.0 { Some(a.cost) } else { None },
        })
        .collect()
}
