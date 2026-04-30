/// Cursor CSV 解析模块
/// 数据来源：Cursor API 导出的 CSV 文件（缓存于 ~/.config/VibeCodingControl/cursor-cache/）
/// 三种 CSV 格式版本：
/// - v1: Date,Model,Input(w/CacheWrite),Input(w/oCacheWrite),CacheRead,OutputTokens,TotalTokens,Cost,CostToYou
/// - v2: Date,Kind,Model,MaxMode,Input(w/CacheWrite),Input(w/oCacheWrite),CacheRead,OutputTokens,TotalTokens,Cost
/// - v3: Date,CloudAgentID,AutomationID,Kind,Model,MaxMode,Input(w/CacheWrite),Input(w/oCacheWrite),CacheRead,OutputTokens,TotalTokens,Cost
///
/// 关键：cache_write = input_with_cache_write - input_without_cache_write

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

    if !session_dir.exists() {
        return Ok(Vec::new());
    }

    // 扫描所有 usage*.csv 文件
    let mut sessions = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&session_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().map(|f| f.to_string_lossy()).unwrap_or_default();
            if !name.starts_with("usage") || !name.ends_with(".csv") {
                continue;
            }

            // 每个文件作为一个"session"
            let file_meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime = super::meta_mtime_ms(&file_meta);

            if let Some(min) = min_mtime_ms {
                if mtime > 0 && mtime < min {
                    continue;
                }
            }

            let sid = path
                .file_stem()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            if sid.is_empty() {
                continue;
            }

            let meta = super::build_session_meta(
                mapping,
                &path,
                sid,
                None,
                None,
                None,
                Some(mtime),
                Some(mtime),
            );
            sessions.push(meta);
        }
    }

    sessions.sort_by_key(|b| std::cmp::Reverse(b.last_active_at));
    Ok(sessions)
}

pub(crate) fn extract_usage(session: &SessionMeta) -> Result<Vec<UsageSummary>> {
    let content = match std::fs::read_to_string(&session.source_path) {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()),
    };

    let mut lines = content.lines();
    let header = match lines.next() {
        Some(h) => h,
        None => return Ok(Vec::new()),
    };

    // 验证是否为 Cursor CSV
    if !header.contains("Date") || !header.contains("Model") {
        return Ok(Vec::new());
    }

    // 检测格式版本
    let header_fields: Vec<&str> = header.split(',').collect();
    let has_kind = header_fields.iter().any(|f| f.trim() == "Kind");
    let col_count = header_fields.len();

    // (model_idx, input_cw_idx, input_no_cw_idx, cache_read_idx, output_idx, cost_idx)
    let indices: (usize, usize, usize, usize, usize, usize) = if has_kind && col_count >= 11 {
        // v3
        (4, 6, 7, 8, 9, 11)
    } else if has_kind {
        // v2
        (2, 4, 5, 6, 7, 9)
    } else {
        // v1
        (1, 2, 3, 4, 5, 7)
    };

    // 聚合
    struct Agg {
        usage: TokenUsage,
        count: i64,
        cost: f64,
    }
    let mut agg: HashMap<(String, String), Agg> = HashMap::new();

    for line in lines {
        let fields: Vec<&str> = line.split(',').collect();
        let (mi, cw_i, ncw_i, cr_i, o_i, c_i) = indices;

        if fields.len() <= c_i {
            continue;
        }

        let model = fields.get(mi).unwrap_or(&"").trim().to_string();
        if model.is_empty() {
            continue;
        }

        let date_str = fields.get(0).unwrap_or(&"").trim().to_string();
        let date = parse_csv_date(&date_str);

        let input_with_cw = parse_csv_i64(fields.get(cw_i).unwrap_or(&"0"));
        let input_no_cw = parse_csv_i64(fields.get(ncw_i).unwrap_or(&"0"));
        let cache_read = parse_csv_i64(fields.get(cr_i).unwrap_or(&"0"));
        let output = parse_csv_i64(fields.get(o_i).unwrap_or(&"0"));
        let cache_creation = (input_with_cw - input_no_cw).max(0);

        let usage = TokenUsage {
            input_tokens: input_no_cw.max(0),
            output_tokens: output.max(0),
            cache_read_tokens: cache_read.max(0),
            cache_creation_tokens: cache_creation,
            ..Default::default()
        };

        if usage.is_empty() && cache_creation == 0 {
            // 跳过全零行
            let cost = parse_csv_cost(fields.get(c_i).unwrap_or(&"0"));
            if cost <= 0.0 {
                continue;
            }
        }

        let cost = parse_csv_cost(fields.get(c_i).unwrap_or(&"0"));

        let key = (model, date);
        let a = agg.entry(key).or_insert_with(|| Agg {
            usage: TokenUsage::default(),
            count: 0,
            cost: 0.0,
        });
        a.usage.add_assign_from(&usage);
        a.count += 1;
        a.cost += cost;
    }

    let usages = agg
        .into_iter()
        .map(|((model, date), a)| UsageSummary {
            tool: session.tool.clone(),
            model,
            usage: a.usage,
            request_count: a.count,
            date: if date.is_empty() { None } else { Some(date) },
            cost_usd: if a.cost > 0.0 { Some(a.cost) } else { None },
        })
        .collect();

    Ok(usages)
}

/// 解析 CSV 中的整数值
fn parse_csv_i64(s: &str) -> i64 {
    s.trim()
        .replace(',', "")
        .parse()
        .unwrap_or(0)
        .max(0)
}

/// 解析 CSV 中的费用值（支持 "$1.23", "NaN", "Included", "-" 等）
fn parse_csv_cost(s: &str) -> f64 {
    let s = s.trim();
    if s.is_empty() || s == "NaN" || s == "Included" || s == "-" {
        return 0.0;
    }
    s.trim_start_matches('$')
        .replace(',', "")
        .parse::<f64>()
        .unwrap_or(0.0)
        .max(0.0)
}

/// 解析 CSV 日期列（支持 ISO 8601 和 YYYY-MM-DD）
fn parse_csv_date(s: &str) -> String {
    let s = s.trim();
    // 尝试 ISO 时间戳解析
    if let Some(ts) = super::parse_iso_timestamp(s) {
        return model::ms_to_date(ts);
    }
    // 纯日期格式 YYYY-MM-DD
    if s.len() >= 10 && s.chars().nth(4) == Some('-') {
        return s[..10].to_string();
    }
    String::new()
}
