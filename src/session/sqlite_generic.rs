/// 通用 SQLite 解析管线
/// 由 SessionMappingConfig.sqlite 配置驱动，支持 Hermes, Kilo, Crush 等工具
/// 核心流程：打开 SQLite → 执行配置的 SQL 查询 → 映射列为 TokenUsage → 按 (model, date) 聚合

use crate::adapter::mapping::SqliteParseConfig;
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
    let sqlite_config = match &session.sqlite {
        Some(c) => c,
        None => return Ok(Vec::new()),
    };

    let db_path = resolve_db_path(config_dir, session.path(), sqlite_config);
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = match rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()),
    };

    let session_query = match &sqlite_config.session_query {
        Some(q) => q,
        None => return Ok(Vec::new()),
    };
    let _columns = match &sqlite_config.session_columns {
        Some(c) => c,
        None => return Ok(Vec::new()),
    };

    let mut stmt = match conn.prepare(session_query) {
        Ok(s) => s,
        Err(_) => return Ok(Vec::new()),
    };

    let column_count = stmt.column_count();
    let mut sessions = Vec::new();

    let rows = stmt.query_map([], |row| {
        let id: String = row.get(0).unwrap_or_default();
        let title: Option<String> = if column_count > 1 {
            row.get(1).unwrap_or(None)
        } else {
            None
        };
        let project_dir: Option<String> = if column_count > 2 {
            row.get(2).unwrap_or(None)
        } else {
            None
        };
        let created_at: Option<f64> = if column_count > 3 {
            row.get(3).unwrap_or(None)
        } else {
            None
        };
        let last_active_at: Option<f64> = if column_count > 4 {
            row.get(4).unwrap_or(None)
        } else {
            None
        };

        Ok((id, title, project_dir, created_at, last_active_at))
    });

    if let Ok(rows) = rows {
        for row in rows.flatten() {
            let (id, title, project_dir, created_at, last_active_at) = row;
            if id.is_empty() {
                continue;
            }

            let created_at_ms = created_at.map(|ts| normalize_ts_f64(ts));
            let last_active_at_ms = last_active_at
                .map(|ts| normalize_ts_f64(ts))
                .or(created_at_ms);

            // 检查 mtime 过滤
            if let Some(min) = min_mtime_ms {
                if let Some(la) = last_active_at_ms {
                    if la > 0 && la < min {
                        continue;
                    }
                }
            }

            let meta = super::build_session_meta(
                mapping,
                &db_path,
                id,
                title,
                None,
                project_dir,
                created_at_ms,
                last_active_at_ms,
            );
            sessions.push(meta);
        }
    }

    sessions.sort_by_key(|b| std::cmp::Reverse(b.last_active_at));
    Ok(sessions)
}

pub(crate) struct ExtractResult {
    pub usages: Vec<UsageSummary>,
}

pub(crate) fn extract_usage(
    session: &SessionMeta,
    config: &SqliteParseConfig,
) -> Result<ExtractResult> {
    let db_path = &session.source_path;
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(_) => return Ok(ExtractResult { usages: Vec::new() }),
    };

    let usage_query = match &config.usage_query {
        Some(q) => q,
        None => return Ok(ExtractResult { usages: Vec::new() }),
    };

    // 替换 {session_id} 占位符
    let query = usage_query.replace("{session_id}", &session.session_id);

    let mut stmt = match conn.prepare(&query) {
        Ok(s) => s,
        Err(_) => return Ok(ExtractResult { usages: Vec::new() }),
    };

    let column_map = &config.column_map;
    let has_json = config.json_column.is_some();

    let mut entries: Vec<(String, TokenUsage, Option<f64>, i64)> = Vec::new();

    let rows = stmt.query_map([], |row| {
        // 动态读取列
        let col_names: Vec<String> = (0..row.as_ref().column_count())
            .filter_map(|i| row.as_ref().column_name(i).ok().map(|s| s.to_string()))
            .collect();

        let mut row_data: HashMap<String, String> = HashMap::new();
        for (i, name) in col_names.iter().enumerate() {
            let val: String = match row.get_ref(i) {
                Ok(rusqlite::types::ValueRef::Text(s)) => {
                    String::from_utf8_lossy(s).to_string()
                }
                Ok(rusqlite::types::ValueRef::Integer(n)) => n.to_string(),
                Ok(rusqlite::types::ValueRef::Real(f)) => f.to_string(),
                Ok(rusqlite::types::ValueRef::Null) => String::new(),
                _ => String::new(),
            };
            row_data.insert(name.to_lowercase(), val);
        }
        Ok(row_data)
    });

    if let Ok(rows) = rows {
        for row in rows.flatten() {
            if has_json {
                // JSON 列模式（Kilo: data 列包含 JSON）
                if let Some(json_col) = &config.json_column {
                    if let Some(json_str) = row.get(&json_col.to_lowercase()) {
                        if !json_str.is_empty() {
                            if let Ok(json_val) = sonic_rs::from_str::<serde_json::Value>(json_str) {
                                if let Some(e) = extract_from_json_column(&json_val, config, session) {
                                    entries.push(e);
                                }
                            }
                        }
                    }
                }
            } else {
                // 普通列模式
                if let Some(e) = extract_from_columns(&row, column_map, config, session) {
                    entries.push(e);
                }
            }
        }
    }

    // 聚合
    let usages = summarize_entries(&session.tool, &entries, session.last_active_at);
    Ok(ExtractResult { usages })
}

/// 从 JSON 列提取（Kilo 模式）
fn extract_from_json_column(
    val: &serde_json::Value,
    config: &SqliteParseConfig,
    _session: &SessionMeta,
) -> Option<(String, TokenUsage, Option<f64>, i64)> {
    let model = config
        .json_model_path
        .as_ref()
        .and_then(|p| json_get_str(val, p))
        .unwrap_or(&config.default_model)
        .to_string();

    if model.is_empty() || model.starts_with('<') {
        return None;
    }

    let mut usage = TokenUsage::default();
    for (field, path) in &config.json_token_map {
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

    if usage.is_empty() {
        return None;
    }

    let cost_usd = config
        .json_cost_path
        .as_ref()
        .and_then(|p| json_get_f64(val, p))
        .map(|v| v.max(0.0));

    let timestamp_ms = 0; // SQLite 通常没有行级时间戳

    Some((model, usage, cost_usd, timestamp_ms))
}

/// 从普通列提取（Hermes 模式）
fn extract_from_columns(
    row: &HashMap<String, String>,
    column_map: &HashMap<String, String>,
    config: &SqliteParseConfig,
    _session: &SessionMeta,
) -> Option<(String, TokenUsage, Option<f64>, i64)> {
    let model = config
        .model_column
        .as_ref()
        .and_then(|col| row.get(&col.to_lowercase()))
        .unwrap_or(&config.default_model)
        .to_string();

    if model.is_empty() {
        return None;
    }

    let mut usage = TokenUsage::default();
    for (field, col_name) in column_map {
        let v = match row.get(&col_name.to_lowercase()) {
            Some(s) if !s.is_empty() => s.parse::<i64>().unwrap_or(0).max(0),
            _ => 0,
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

    if usage.is_empty() {
        // 如果有 cost 但 token 全零，仍然保留（Crush 场景）
        let cost1 = config
            .cost_column
            .as_ref()
            .and_then(|col| row.get(&col.to_lowercase()))
            .and_then(|s| s.parse::<f64>().ok());
        let cost2 = config
            .cost_column_fallback
            .as_ref()
            .and_then(|col| row.get(&col.to_lowercase()))
            .and_then(|s| s.parse::<f64>().ok());
        let cost = cost1.or(cost2);
        if cost.is_none() || cost.unwrap() <= 0.0 {
            return None;
        }
    }

    let cost_usd = config
        .cost_column
        .as_ref()
        .and_then(|col| row.get(&col.to_lowercase()))
        .and_then(|s| s.parse::<f64>().ok())
        .map(|v| v.max(0.0))
        .or_else(|| {
            config
                .cost_column_fallback
                .as_ref()
                .and_then(|col| row.get(&col.to_lowercase()))
                .and_then(|s| s.parse::<f64>().ok())
                .map(|v| v.max(0.0))
        });

    let timestamp_ms: i64 = row
        .get("started_at")
        .or_else(|| row.get("created_at"))
        .or_else(|| row.get("updated_at"))
        .and_then(|s| s.parse::<i64>().ok())
        .map(normalize_ts)
        .unwrap_or(0);

    Some((model, usage, cost_usd, timestamp_ms))
}

// ── 辅助函数 ──

fn resolve_db_path(config_dir: &Path, session_path: &str, config: &SqliteParseConfig) -> std::path::PathBuf {
    // 如果有 projects_registry，先查找注册表
    if let Some(registry) = &config.projects_registry {
        let registry_path = config_dir.join(registry);
        if registry_path.exists() {
            // 返回第一个找到的 db（简化实现，Crush 等多 db 场景后续扩展）
            if let Some(db) = discover_first_db(&registry_path) {
                return db;
            }
        }
    }
    config_dir.join(session_path)
}

fn discover_first_db(registry_path: &Path) -> Option<std::path::PathBuf> {
    let content = std::fs::read_to_string(registry_path).ok()?;
    let val: serde_json::Value = sonic_rs::from_str(&content).ok()?;
    // 尝试从数组中找第一个含 path 的项目
    if let Some(arr) = val.as_array() {
        for item in arr {
            if let Some(path) = item.get("path").and_then(|v| v.as_str()) {
                let db = std::path::PathBuf::from(path).join("crush.db");
                if db.exists() {
                    return Some(db);
                }
            }
        }
    }
    None
}

fn normalize_ts(ts: i64) -> i64 {
    if ts > 0 && ts < 1_000_000_000_000 {
        ts * 1000
    } else {
        ts
    }
}

fn normalize_ts_f64(ts: f64) -> i64 {
    if ts > 0.0 && ts < 1e12 {
        (ts * 1000.0) as i64
    } else {
        ts as i64
    }
}

fn json_get_str<'a>(val: &'a serde_json::Value, path: &str) -> Option<&'a str> {
    let mut current = val;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    current.as_str()
}

fn json_get_f64(val: &serde_json::Value, path: &str) -> Option<f64> {
    let mut current = val;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    current.as_f64()
}

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

fn summarize_entries(
    tool: &str,
    entries: &[(String, TokenUsage, Option<f64>, i64)],
    last_active_at: Option<i64>,
) -> Vec<UsageSummary> {
    struct Agg {
        usage: TokenUsage,
        count: i64,
        cost: f64,
    }
    let mut agg: HashMap<(String, String), Agg> = HashMap::new();

    for (model, usage, cost_usd, timestamp_ms) in entries {
        let date = if *timestamp_ms > 0 {
            model::ms_to_date(*timestamp_ms)
        } else {
            last_active_at.map(model::ms_to_date).unwrap_or_default()
        };
        let key = (model.clone(), date);
        let a = agg.entry(key).or_insert_with(|| Agg {
            usage: TokenUsage::default(),
            count: 0,
            cost: 0.0,
        });
        a.usage.add_assign_from(usage);
        a.count += 1;
        if let Some(c) = cost_usd {
            a.cost += c;
        }
    }

    agg.into_iter()
        .map(|((model, date), a)| UsageSummary {
            tool: tool.to_string(),
            model,
            usage: a.usage,
            request_count: a.count,
            date: if date.is_empty() { None } else { Some(date) },
            cost_usd: if a.cost > 0.0 { Some(a.cost) } else { None },
        })
        .collect()
}
