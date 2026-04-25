use anyhow::Result;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use crate::adapter::mapping::ToolMapping;
use crate::session::model::{DeleteOutcome, SessionMeta, TokenUsage, UsageSummary};

#[derive(serde::Deserialize, Default)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
struct KimiState {
    custom_title: Option<String>,
}

#[derive(serde::Deserialize, Default)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
struct KimiMetadata {
    #[serde(rename = "session_id")]
    session_id: Option<String>,
    title: Option<String>,
}

#[derive(serde::Deserialize, Default)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
struct KimiWireLine {
    timestamp: Option<f64>,
    message: Option<KimiWireMessage>,
}

#[derive(serde::Deserialize, Default)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
struct KimiWireMessage {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    payload: Option<serde_json::Value>,
}

pub(crate) fn scan_sessions(
    config_dir: &Path,
    mapping: &ToolMapping,
    min_mtime_ms: Option<i64>,
) -> Result<Vec<SessionMeta>> {
    let session_dir = config_dir.join(mapping.session.path());
    if !session_dir.exists() {
        return Ok(Vec::new());
    }
    let project_map = build_project_map(&config_dir.join("kimi.json"));
    let mut sessions = Vec::new();
    for hash_entry in super::walkdir_dirs(&session_dir)? {
        let hash_name = hash_entry
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let project_dir = project_map.get(&hash_name).cloned();
        for session_entry in super::walkdir_dirs(&hash_entry)? {
            let wire_path = session_entry.join("wire.jsonl");
            if !wire_path.exists() {
                continue;
            }
            if let Some(min_ms) = min_mtime_ms {
                let m = super::mtime_ms(&wire_path);
                if m > 0 && m < min_ms {
                    continue;
                }
            }
            if let Some(meta) =
                parse_session_meta(&session_entry, &wire_path, mapping, project_dir.as_deref())
            {
                sessions.push(meta);
            }
        }
    }
    sessions.sort_by_key(|b| std::cmp::Reverse(b.last_active_at));
    Ok(sessions)
}

fn parse_session_meta(
    session_dir: &Path,
    wire_path: &Path,
    mapping: &ToolMapping,
    project_dir: Option<&str>,
) -> Option<SessionMeta> {
    let session_id = session_dir.file_name()?.to_string_lossy().to_string();
    let state: KimiState = read_json_file(&session_dir.join("state.json"));
    let metadata: KimiMetadata = read_json_file(&session_dir.join("metadata.json"));
    let mut user_texts: Vec<String> = Vec::new();
    let mut created_at: Option<i64> = None;
    let mut last_active_at: Option<i64> = None;
    for line in std::io::BufReader::new(std::fs::File::open(wire_path).ok()?).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let wl: KimiWireLine = match sonic_rs::from_str(&line) {
            Ok(w) => w,
            Err(_) => continue,
        };
        if let Some(ts) = wl.timestamp {
            let ms = (ts * 1000.0) as i64;
            if created_at.is_none() {
                created_at = Some(ms);
            }
            last_active_at = Some(ms);
        }
        let msg = match wl.message {
            Some(m) => m,
            None => continue,
        };
        if msg.msg_type.as_deref() == Some("TurnBegin") && user_texts.len() < 3 {
            if let Some(text) = msg
                .payload
                .as_ref()
                .and_then(|p| extract_kimi_user_input(p.get("user_input")))
            {
                let cleaned = clean_kimi_user_text(&text);
                if !cleaned.is_empty() {
                    user_texts.push(cleaned);
                }
            }
        }
    }
    if last_active_at.is_none() {
        last_active_at = super::mtime_ms_nonzero(wire_path);
    }
    let title = filter_kimi_title(state.custom_title.as_deref())
        .or_else(|| filter_kimi_title(metadata.title.as_deref()))
        .or(super::build_user_title(&user_texts));
    Some(super::build_session_meta(
        mapping,
        session_dir,
        session_id,
        title,
        None,
        project_dir.map(|s| s.to_string()),
        created_at,
        last_active_at,
    ))
}

fn filter_kimi_title(t: Option<&str>) -> Option<String> {
    t.filter(|t| !t.is_empty() && *t != "[...]" && *t != "(untitled)" && !t.starts_with('/'))
        .map(|s| s.to_string())
}

pub(crate) fn extract_usage(session: &SessionMeta) -> Result<Vec<UsageSummary>> {
    let wire_path = session.source_path.join("wire.jsonl");
    if !wire_path.exists() {
        return Ok(Vec::new());
    }
    let mut model_usages: HashMap<String, TokenUsage> = HashMap::new();
    let mut model_counts: HashMap<String, i64> = HashMap::new();
    for line in std::io::BufReader::new(std::fs::File::open(&wire_path)?).lines() {
        let wl: KimiWireLine = match sonic_rs::from_str(&line?) {
            Ok(w) => w,
            Err(_) => continue,
        };
        let msg = match wl.message {
            Some(m) => m,
            None => continue,
        };
        if msg.msg_type.as_deref() != Some("StatusUpdate") {
            continue;
        }
        let tu = match msg.payload.and_then(|p| p.get("token_usage").cloned()) {
            Some(t) => t,
            None => continue,
        };
        let (inp, out, cr, cc) = (
            tu.get("input_other").and_then(|v| v.as_i64()).unwrap_or(0),
            tu.get("output").and_then(|v| v.as_i64()).unwrap_or(0),
            tu.get("input_cache_read")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            tu.get("input_cache_creation")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
        );
        if inp == 0 && out == 0 && cr == 0 && cc == 0 {
            continue;
        }
        let e = model_usages.entry("kimi".to_string()).or_default();
        e.input_tokens += inp;
        e.output_tokens += out;
        e.cache_read_tokens += cr;
        e.cache_creation_tokens += cc;
        *model_counts.entry("kimi".to_string()).or_default() += 1;
    }
    Ok(super::usages_from_maps(
        &session.tool,
        model_usages,
        &model_counts,
        session.last_active_at,
    ))
}

pub(crate) fn delete_session(source_path: &Path) -> Result<DeleteOutcome> {
    let mut outcome = DeleteOutcome::default();
    if source_path.is_dir() {
        outcome.bytes_freed += crate::session::dir_size(source_path);
        std::fs::remove_dir_all(source_path)?;
        outcome.files_removed += 1;
    }
    Ok(outcome)
}

fn build_project_map(kimi_json_path: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let value: serde_json::Value = match std::fs::read_to_string(kimi_json_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(v) => v,
        None => return map,
    };
    let work_dirs = match value.get("work_dirs").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return map,
    };
    use md5::{Digest, Md5};
    for wd in work_dirs {
        if let Some(path) = wd.get("path").and_then(|v| v.as_str()) {
            map.insert(
                format!("{:x}", Md5::digest(path.as_bytes())),
                path.to_string(),
            );
        }
    }
    map
}

fn extract_kimi_user_input(value: Option<&serde_json::Value>) -> Option<String> {
    super::extract_raw_text_from_json(value, "text")
}

fn read_json_file<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    super::read_json(path).unwrap_or_default()
}

fn clean_kimi_user_text(text: &str) -> String {
    let text = text.trim();
    if text.is_empty()
        || text.starts_with('#')
        || text.starts_with('<')
        || text.starts_with('[')
        || text.starts_with('/')
    {
        return String::new();
    }
    super::truncate_str(text.lines().next().unwrap_or("").trim(), 80)
}
