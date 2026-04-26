pub mod cache;
pub mod claude;
pub mod codex;
pub mod kimi;
pub mod model;

use crate::adapter::mapping::ToolMapping;
use crate::session::cache::{
    file_meta, merge_ranges, now_ms, CacheStatus, CachedDailyUsage, CachedUsageData,
    SubagentFileState, UnifiedCache,
};
use crate::session::model::{DeleteOutcome, SessionMeta, TimeRange, TokenUsage, UsageSummary};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

pub(crate) fn scan_sessions(
    mapping: &ToolMapping,
    min_mtime_ms: Option<i64>,
) -> Result<Vec<SessionMeta>> {
    let session = &mapping.session;
    if session.format.is_empty() || session.path.is_none() {
        return Ok(Vec::new());
    }
    let config_dir = match mapping.resolved_config_dir() {
        Some(d) if d.exists() => d,
        _ => return Ok(Vec::new()),
    };
    match session.format.as_str() {
        "jsonl" => match mapping.tool.name.as_str() {
            "claude" => claude::scan_sessions(&config_dir, mapping, min_mtime_ms),
            "codex" => codex::scan_sessions(&config_dir, mapping, min_mtime_ms),
            _ => Ok(Vec::new()),
        },
        "json" => scan_gemini_sessions(&config_dir, mapping, min_mtime_ms),
        "opencode_json" => scan_opencode_sessions(&config_dir, mapping, min_mtime_ms),
        "kimi_dir" => kimi::scan_sessions(&config_dir, mapping, min_mtime_ms),
        _ => Ok(Vec::new()),
    }
}

pub(crate) fn extract_usage(
    mapping: &ToolMapping,
    session: &SessionMeta,
    range_start_ms: Option<i64>,
) -> Result<claude::ExtractResult> {
    Ok(match mapping.tool.name.as_str() {
        "claude" => claude::extract_usage(session, range_start_ms)?,
        "codex" => claude::ExtractResult {
            usages: codex::extract_usage(session)?,
            first_byte_offset: 0,
            subagent_files: HashMap::new(),
        },
        "gemini" => claude::ExtractResult {
            usages: extract_gemini_usage(session)?,
            first_byte_offset: 0,
            subagent_files: HashMap::new(),
        },
        "opencode" => claude::ExtractResult {
            usages: Vec::new(),
            first_byte_offset: 0,
            subagent_files: HashMap::new(),
        },
        "kimi" => claude::ExtractResult {
            usages: kimi::extract_usage(session)?,
            first_byte_offset: 0,
            subagent_files: HashMap::new(),
        },
        _ => claude::ExtractResult {
            usages: Vec::new(),
            first_byte_offset: 0,
            subagent_files: HashMap::new(),
        },
    })
}

pub(crate) fn extract_all_usage(
    sessions: &[SessionMeta],
    mappings: &[ToolMapping],
    range: TimeRange,
) -> Vec<UsageSummary> {
    let t0 = std::time::Instant::now();
    let start_ms = range.start_ms();
    let mut cache = UnifiedCache::load().ok().unwrap_or_default();
    eprintln!("[PERF]   3a.cache_load: {:.1}ms", t0.elapsed().as_secs_f64() * 1000.0);

    let t_upsert = std::time::Instant::now();
    let mut cache_dirty = false;

    // 阶段 1：串行更新 session 元数据
    for session in sessions {
        if cache.upsert_session(session) {
            cache_dirty = true;
        }
    }
    eprintln!("[PERF]   3b.cache_upsert: {:.1}ms", t_upsert.elapsed().as_secs_f64() * 1000.0);

    let filtered: Vec<&SessionMeta> = sessions
        .iter()
        .filter(|s| {
            // 只在 --all 时不过滤，其他时候用 last_active_at 做粗略过滤
            // 注意：last_active_at 可能不精确，所以给一定余量（7天）
            if let Some(ms) = start_ms {
                if let Some(a) = s.last_active_at {
                    // 如果 last_active_at 比 range_start 早超过 7 天，跳过
                    // 7 天余量确保不会因 last_active_at 不精确而误跳
                    a >= ms - 7 * 24 * 3600 * 1000
                } else {
                    true // 没有 last_active_at，不过滤
                }
            } else {
                true // --all 不过滤
            }
        })
        .collect();

    // 按文件大小降序排列，确保大 session 配对处理（减少总 wall time）
    let mut filtered = filtered;
    filtered.sort_by(|a, b| b.file_size.cmp(&a.file_size));

    // 阶段 2：提取 usage
    // 策略：session 间并行处理（2个一批），session 内 main→sub 串行
    // main 用全局 rayon，sub 用全局 SUB_POOL，避免线程池争抢
    // 注意：并发度>2 会导致 I/O 和 rayon 嵌套竞争，性能反而退化
    let t1 = std::time::Instant::now();

    let results: Vec<ExtractResult> = filtered
        .chunks(2)
        .flat_map(|chunk| {
            std::thread::scope(|s| {
                let handles: Vec<_> = chunk
                    .iter()
                    .map(|session| {
                        s.spawn(|| process_session(session, mappings, &cache, start_ms))
                    })
                    .collect();
                handles
                    .into_iter()
                    .filter_map(|h| h.join().ok().flatten())
                    .collect::<Vec<_>>()
            })
        })
        .collect();

    eprintln!("[PERF]   3c.parallel_extract: {:.1}ms", t1.elapsed().as_secs_f64() * 1000.0);

    let t2 = std::time::Instant::now();
    let mut summaries: Vec<UsageSummary> = Vec::new();
    for r in results {
        if let Some(data) = r.usage_data {
            cache.update_usage(&r.tool, &r.session_id, data);
            cache_dirty = true;
        }
        summaries.extend(r.usages);
    }
    // purge_missing 每次调用都对所有缓存路径做 stat()，太慢
    // 只在有实际更新时才执行
    if cache_dirty {
        cache.purge_missing();
    }
    if cache_dirty {
        if let Err(e) = cache.save() {
            eprintln!("warning: failed to save cache: {}", e);
        }
    }
    eprintln!("[PERF]   3d.cache_save+merge: {:.1}ms", t2.elapsed().as_secs_f64() * 1000.0);
    // 输出阶段按日期过滤
    filter_by_date(summaries, start_ms)
}

/// 按日期过滤 UsageSummary
fn filter_by_date(usages: Vec<UsageSummary>, start_ms: Option<i64>) -> Vec<UsageSummary> {
    if start_ms.is_none() {
        return usages;
    }
    let start = start_ms.unwrap();
    usages
        .into_iter()
        .filter(|u| {
            u.date
                .as_ref()
                .and_then(|d| model::date_to_ms(d))
                .is_none_or(|ms| ms + 86400_000 > start)
        })
        .collect()
}

struct ExtractResult {
    tool: String,
    session_id: String,
    usages: Vec<UsageSummary>,
    usage_data: Option<CachedUsageData>,
}

/// 处理单个 session 的 usage 提取（用于并行调度）
fn process_session(
    session: &SessionMeta,
    mappings: &[ToolMapping],
    cache: &UnifiedCache,
    start_ms: Option<i64>,
) -> Option<ExtractResult> {
    let mapping = mappings.iter().find(|m| m.tool.name == session.tool)?;
    let status = cache.check_cache_status(&session.tool, &session.session_id, &session.source_path);

    let cache_range_compatible = cache.get_usage(&session.tool, &session.session_id)
        .is_none_or(|u| {
            if start_ms.is_none() && u.parsed_range_start_ms.is_some() {
                return false;
            }
            true
        });

    let effective_status = if !cache_range_compatible {
        CacheStatus::Miss
    } else if status == CacheStatus::Hit && start_ms.is_none() {
        let file_size = std::fs::metadata(&session.source_path).map(|m| m.len()).unwrap_or(0);
        let is_complete = cache.get_usage(&session.tool, &session.session_id)
            .is_some_and(|u| u.is_fully_processed(file_size));
        if is_complete { CacheStatus::Hit } else { CacheStatus::Miss }
    } else {
        status
    };

    let cached_usage = if cache_range_compatible {
        cache.get_usage(&session.tool, &session.session_id).cloned()
    } else {
        None
    };

    let (usages, usage_data): (Vec<UsageSummary>, Option<CachedUsageData>) = match effective_status {
        CacheStatus::Hit => {
            let usages = cache.load_usages_in_range(&session.tool, &session.session_id, None);
            (usages, None)
        }
        CacheStatus::Incremental => {
            match cached_usage {
                Some(ce) => {
                    let subagent_changed = check_subagent_changes(session, &ce);
                    if subagent_changed {
                        let r = extract_usage(mapping, session, start_ms).ok()?;
                        let new_data = build_usage_data(
                            session, &r.usages, None, None, r.first_byte_offset, r.subagent_files, start_ms,
                        );
                        (r.usages, new_data)
                    } else {
                        match incremental_extract(mapping, session, &ce, false) {
                            Ok((merged_usages, st, fbo, subagent_state)) => {
                                let new_data = build_usage_data(
                                    session, &merged_usages, st.as_ref(), Some(&ce), fbo, subagent_state, start_ms,
                                );
                                (merged_usages, new_data)
                            }
                            Err(_) => {
                                let r = extract_usage(mapping, session, start_ms).ok()?;
                                let new_data = build_usage_data(
                                    session, &r.usages, None, None, r.first_byte_offset, r.subagent_files, start_ms,
                                );
                                (r.usages, new_data)
                            }
                        }
                    }
                }
                None => {
                    let r = extract_usage(mapping, session, start_ms).ok()?;
                    let new_data = build_usage_data(
                        session, &r.usages, None, None, r.first_byte_offset, r.subagent_files, start_ms,
                    );
                    (r.usages, new_data)
                }
            }
        }
        CacheStatus::Miss => {
            if session.tool == "claude" && cached_usage.is_some() {
                let ce = cached_usage.unwrap();
                let subagent_changed = check_subagent_changes(session, &ce);
                if subagent_changed {
                    let r = extract_usage(mapping, session, start_ms).ok()?;
                    let new_data = build_usage_data(
                        session, &r.usages, None, None, r.first_byte_offset, r.subagent_files, start_ms,
                    );
                    (r.usages, new_data)
                } else {
                    match incremental_extract(mapping, session, &ce, false) {
                        Ok((merged_usages, st, fbo, subagent_state)) => {
                            let new_data = build_usage_data(
                                session, &merged_usages, st.as_ref(), Some(&ce), fbo, subagent_state, start_ms,
                            );
                            (merged_usages, new_data)
                        }
                        Err(_) => {
                            let r = extract_usage(mapping, session, start_ms).ok()?;
                            let new_data = build_usage_data(
                                session, &r.usages, None, None, r.first_byte_offset, r.subagent_files, start_ms,
                            );
                            (r.usages, new_data)
                        }
                    }
                }
            } else {
                let r = extract_usage(mapping, session, start_ms).ok()?;
                let new_data = build_usage_data(
                    session, &r.usages, None, None, r.first_byte_offset, r.subagent_files, start_ms,
                );
                (r.usages, new_data)
            }
        }
    };
    Some(ExtractResult {
        tool: session.tool.clone(),
        session_id: session.session_id.clone(),
        usages,
        usage_data,
    })
}

fn incremental_extract(
    mapping: &ToolMapping,
    session: &SessionMeta,
    cached: &CachedUsageData,
    subagent_changed: bool,
) -> Result<(Vec<UsageSummary>, Option<codex::CodexIncrementalState>, u64, HashMap<String, SubagentFileState>)> {
    match mapping.tool.name.as_str() {
        "claude" => {
            // 1. 解析主文件增量
            let incremental = claude::extract_usage_incremental(
                session, cached.last_byte_offset,
            )?;

            // 2. 如果 subagent 有变化，增量解析 subagent
            let (subagent_usages, subagent_state) = if subagent_changed {
                claude::extract_subagent_incremental(
                    session, &cached.subagent_files,
                )?
            } else {
                (Vec::new(), cached.subagent_files.clone())
            };

            // 3. 合并：缓存 + 主文件增量 + subagent 增量
            let merged = merge_with_cache_and_subagent(cached, incremental, &subagent_usages, session);
            let fbo = cached.first_byte_offset;
            Ok((merged, None, fbo, subagent_state))
        }
        "codex" => {
            let (usages, state) = codex::extract_usage_incremental(
                session,
                cached.last_byte_offset,
                cached.codex_prev_total.clone(),
                cached.codex_current_model.clone(),
                &codex_cached_usages_from_daily(&cached.daily),
            )?;
            Ok((usages, Some(state), 0, HashMap::new()))
        }
        _ => {
            let r = extract_usage(mapping, session, None)?;
            Ok((r.usages, None, r.first_byte_offset, r.subagent_files))
        }
    }
}

/// 将 daily 格式的缓存转换为 Codex 需要的旧格式 CachedUsage 列表
fn codex_cached_usages_from_daily(
    daily: &std::collections::HashMap<String, Vec<CachedDailyUsage>>,
) -> Vec<cache::CachedDailyUsage> {
    // Codex 不需要 date 粒度，但接口需要 CachedUsage 列表
    // 这里简单展开 daily 为扁平列表（用于恢复 CodexIncrementalState）
    daily
        .values()
        .flatten()
        .cloned()
        .collect()
}

/// 合并缓存的 daily 数据 + 主文件增量 + subagent 增量
fn merge_with_cache_and_subagent(
    cached: &CachedUsageData,
    main_incremental: Vec<UsageSummary>,
    subagent_incremental: &[UsageSummary],
    session: &SessionMeta,
) -> Vec<UsageSummary> {
    let mut merged: std::collections::HashMap<(String, String), (TokenUsage, i64, Option<f64>)> =
        std::collections::HashMap::new();

    // 缓存数据
    for (date, usages) in &cached.daily {
        for cu in usages {
            let key = (cu.model.clone(), date.clone());
            let e = merged.entry(key).or_default();
            e.0.input_tokens += cu.input_tokens;
            e.0.output_tokens += cu.output_tokens;
            e.0.cache_read_tokens += cu.cache_read_tokens;
            e.0.cache_creation_tokens += cu.cache_creation_tokens;
            e.0.cache_creation_5m_tokens += cu.cache_creation_5m_tokens;
            e.0.cache_creation_1h_tokens += cu.cache_creation_1h_tokens;
            e.0.web_search_requests += cu.web_search_requests;
            e.1 += cu.request_count;
            if cu.cost_usd.is_some() {
                e.2 = cu.cost_usd;
            }
        }
    }

    // 主文件增量
    for u in main_incremental {
        let date = u.date.clone().unwrap_or_default();
        let key = (u.model.clone(), date);
        let e = merged.entry(key).or_default();
        e.0 += u.usage;
        e.1 += u.request_count;
        if u.cost_usd.is_some() {
            e.2 = u.cost_usd;
        }
    }

    // subagent 增量
    for u in subagent_incremental {
        let date = u.date.clone().unwrap_or_default();
        let key = (u.model.clone(), date);
        let e = merged.entry(key).or_default();
        e.0.add_assign_from(&u.usage);
        e.1 += u.request_count;
        if u.cost_usd.is_some() {
            e.2 = u.cost_usd;
        }
    }

    merged
        .into_iter()
        .map(|((model, date), (usage, count, cost))| UsageSummary {
            tool: session.tool.clone(),
            model,
            usage,
            request_count: count,
            date: if date.is_empty() { None } else { Some(date) },
            cost_usd: cost,
        })
        .collect()
}

/// 检查 subagent 文件是否有变化（新增、修改或删除）
fn check_subagent_changes(
    session: &SessionMeta,
    cached: &CachedUsageData,
) -> bool {
    let sub_dir = session.source_path.with_extension("").join("subagents");
    if !sub_dir.is_dir() {
        return !cached.subagent_files.is_empty(); // 目录不存在但缓存有记录 → 有变化
    }

    let Ok(entries) = std::fs::read_dir(&sub_dir) else {
        return !cached.subagent_files.is_empty();
    };

    let mut current_count = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.starts_with("agent-") || !path.extension().is_some_and(|e| e == "jsonl") {
            continue;
        }
        current_count += 1;
        if let Some((mms, fs)) = file_meta(&path) {
            match cached.subagent_files.get(name.as_ref()) {
                None => return true, // 新文件
                Some(cs) => {
                    if cs.modified_ms != mms || cs.file_size != fs {
                        return true; // 文件变化
                    }
                }
            }
        }
    }

    // 文件数量不同 → 有变化（可能是删除了文件）
    if current_count != cached.subagent_files.len() {
        return true;
    }

    false
}


/// 构建 CachedUsageData（按天分组）
fn build_usage_data(
    session: &SessionMeta,
    usages: &[UsageSummary],
    codex_state: Option<&codex::CodexIncrementalState>,
    prev_data: Option<&CachedUsageData>,
    first_byte_offset: u64,
    subagent_files: HashMap<String, SubagentFileState>,
    range_start_ms: Option<i64>,
) -> Option<CachedUsageData> {
    let (_modified_ms, file_size) = match file_meta(&session.source_path) {
        Some(m) => m,
        None => (0, 0),
    };

    // 从 prev_data 继承 processed_ranges
    let mut processed_ranges = prev_data
        .map(|p| p.effective_processed_ranges())
        .unwrap_or_default();

    // 添加本次处理的范围
    if file_size > 0 {
        let start = if first_byte_offset > 0 { first_byte_offset } else { 0 };
        processed_ranges.push((start, file_size));
        processed_ranges = merge_ranges(processed_ranges);
    }

    // 按 (date, model) 聚合为 daily
    // 注意：不从 prev_data 导入 daily 数据，因为 usages 已经是完整的数据
    // prev_data 只用于继承 processed_ranges、codex 状态等元信息
    let mut daily: std::collections::HashMap<String, Vec<CachedDailyUsage>> =
        std::collections::HashMap::new();

    // 合并新数据
    for u in usages {
        let date = u.date.clone().unwrap_or_default();
        let entries = daily.entry(date).or_default();
        if let Some(existing) = entries.iter_mut().find(|e| e.model == u.model) {
            existing.input_tokens += u.usage.input_tokens;
            existing.output_tokens += u.usage.output_tokens;
            existing.cache_read_tokens += u.usage.cache_read_tokens;
            existing.cache_creation_tokens += u.usage.cache_creation_tokens;
            existing.cache_creation_5m_tokens += u.usage.cache_creation_5m_tokens;
            existing.cache_creation_1h_tokens += u.usage.cache_creation_1h_tokens;
            existing.web_search_requests += u.usage.web_search_requests;
            existing.request_count += u.request_count;
            if u.cost_usd.is_some() {
                existing.cost_usd = u.cost_usd;
            }
        } else {
            entries.push(CachedDailyUsage::from_usage_summary(u));
        }
    }

    let (cpt, ccm) = match codex_state {
        Some(s) => (s.prev_total.clone(), normalize_codex_model(&s.current_model)),
        None => (
            prev_data.and_then(|p| p.codex_prev_total.clone()),
            prev_data.and_then(|p| p.codex_current_model.clone()),
        ),
    };

    let first_fbo = processed_ranges.first().map(|r| r.0).unwrap_or(0);
    let last_lbo = processed_ranges.last().map(|r| r.1).unwrap_or(0);

    Some(CachedUsageData {
        extracted_at: now_ms(),
        processed_ranges,
        first_byte_offset: first_fbo,
        last_byte_offset: last_lbo,
        daily,
        codex_prev_total: cpt,
        codex_current_model: ccm,
        subagent_files,
        parsed_range_start_ms: range_start_ms,
    })
}

fn normalize_codex_model(model: &str) -> Option<String> {
    if model.is_empty() || model == "unknown" {
        None
    } else {
        Some(model.to_string())
    }
}

pub(crate) fn delete_session(mapping: &ToolMapping, source_path: &Path) -> Result<DeleteOutcome> {
    let config_dir = mapping
        .resolved_config_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot find config directory"))?;
    let canonical = source_path.canonicalize()?;
    let root = config_dir.canonicalize()?;
    if !canonical.starts_with(&root) {
        anyhow::bail!(
            "path traversal detected: {} is outside {}",
            source_path.display(),
            root.display()
        );
    }
    if mapping.tool.name == "kimi" {
        return kimi::delete_session(source_path);
    }
    delete_file_and_dir(source_path)
}

fn delete_file_and_dir(path: &Path) -> Result<DeleteOutcome> {
    let mut o = DeleteOutcome::default();
    if path.is_file() {
        o.bytes_freed += std::fs::metadata(path)?.len();
        std::fs::remove_file(path)?;
        o.files_removed += 1;
    }
    if path.extension().is_some_and(|e| e == "jsonl") {
        let dir = path.with_extension("");
        if dir.is_dir() {
            o.bytes_freed += dir_size(&dir);
            std::fs::remove_dir_all(&dir)?;
            o.files_removed += 1;
        }
    }
    Ok(o)
}

pub(crate) fn dir_size(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| {
            std::fs::symlink_metadata(&p)
                .ok()
                .filter(|m| !m.is_symlink())
                .map(|m| (p, m))
        })
        .map(|(p, m)| {
            if m.is_file() {
                m.len()
            } else if m.is_dir() {
                dir_size(&p)
            } else {
                0
            }
        })
        .sum()
}
pub(crate) fn format_number(n: i64) -> String {
    match n {
        n if n >= 1_000_000_000_000 => format!("{:.2}T", n as f64 / 1_000_000_000_000.0),
        n if n >= 1_000_000_000 => format!("{:.2}B", n as f64 / 1_000_000_000.0),
        n if n >= 1_000_000 => format!("{:.2}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{:.2}K", n as f64 / 1_000.0),
        _ => n.to_string(),
    }
}
pub(crate) fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    match bytes {
        0..KB => format!("{} B", bytes),
        KB..MB => format!("{:.1} KB", bytes as f64 / KB as f64),
        MB..GB => format!("{:.1} MB", bytes as f64 / MB as f64),
        _ => format!("{:.1} GB", bytes as f64 / GB as f64),
    }
}
pub(crate) fn format_relative_time(ts_ms: i64) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let d = (n - ts_ms).max(0) / 1000;
    match d {
        s if s < 60 => format!("{}s ago", s),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}
pub(crate) fn meta_mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn mtime_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .ok()
        .map(|m| meta_mtime_ms(&m))
        .unwrap_or(0)
}

pub(crate) fn mtime_ms_nonzero(path: &Path) -> Option<i64> {
    let ms = mtime_ms(path);
    if ms > 0 {
        Some(ms)
    } else {
        None
    }
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub(crate) fn fallback_session_id(path: &Path) -> Option<String> {
    path.file_stem().map(|s| s.to_string_lossy().to_string())
}

pub(crate) fn parse_iso_timestamp(s: &str) -> Option<i64> {
    // 使用 chrono 正确解析 ISO 8601 时间戳（含时区），然后转为本地时区的毫秒时间戳
    // 支持: 2026-04-24T07:30:00.000Z, 2026-04-24T15:30:00.000+08:00, 2026-04-24T15:30:00.000
    let s = s.trim();
    // 尝试解析带时区的时间戳
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Local).timestamp_millis());
    }
    // 回退：没有时区信息的时间戳，假设为本地时区
    let s = s.trim_end_matches('Z');
    let (ds, ts) = s.split_once('T')?;
    let date_ms = model::date_to_ms(ds)?;
    let tp: Vec<&str> = ts.split(':').collect();
    if tp.len() < 3 {
        return None;
    }
    let h: i64 = tp[0].parse().ok()?;
    let m: i64 = tp[1].parse().ok()?;
    let sp: Vec<&str> = tp[2].split('.').collect();
    let sec: i64 = sp[0].parse().ok()?;
    let ms: i64 = sp.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(date_ms + (h * 3600 + m * 60 + sec) * 1000 + ms)
}

pub(crate) fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max - 3).collect::<String>())
    }
}
pub(crate) fn build_user_title(texts: &[String]) -> Option<String> {
    if texts.is_empty() {
        return None;
    }
    let first = &texts[0];
    if first.chars().count() >= 20 || texts.len() == 1 {
        return Some(truncate_str(first, 80));
    }
    Some(truncate_str(
        &texts[1..].iter().fold(first.clone(), |mut a, m| {
            if a.chars().count() >= 40 {
                return a;
            }
            a.push_str(" | ");
            a.push_str(m);
            a
        }),
        80,
    ))
}
pub(crate) fn fallback_title(texts: &[String], project_dir: Option<&str>) -> Option<String> {
    build_user_title(texts).or_else(|| {
        project_dir.and_then(|d| {
            std::path::PathBuf::from(d)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
        })
    })
}
pub(crate) fn usages_from_maps(
    tool: &str,
    model_usages: std::collections::HashMap<String, model::TokenUsage>,
    model_counts: &std::collections::HashMap<String, i64>,
    last_active_at: Option<i64>,
) -> Vec<model::UsageSummary> {
    model_usages
        .into_iter()
        .map(|(model, usage)| model::UsageSummary {
            tool: tool.to_string(),
            model: model.clone(),
            usage,
            request_count: model_counts.get(&model).copied().unwrap_or(0),
            date: last_active_at.map(model::ms_to_date),
            cost_usd: None,
        })
        .collect()
}
pub(crate) fn walkdir_files(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut e = Vec::new();
    for (p, m) in read_dir_entries(dir)? {
        if m.is_dir() {
            // 跳过 subagents 目录（由 extract_subagent_usage 按需处理）
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            if name != "subagents" {
                e.extend(walkdir_files(&p)?);
            }
        } else {
            e.push(p);
        }
    }
    Ok(e)
}
pub(crate) fn walkdir_dirs(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut d: Vec<_> = read_dir_entries(dir)?
        .into_iter()
        .filter(|(_, m)| m.is_dir())
        .map(|(p, _)| p)
        .collect();
    d.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    Ok(d)
}
fn read_dir_entries(dir: &Path) -> Result<Vec<(std::path::PathBuf, std::fs::Metadata)>> {
    Ok(std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let m = std::fs::symlink_metadata(&p).ok()?;
            if m.is_symlink() {
                None
            } else {
                Some((p, m))
            }
        })
        .collect())
}

pub(crate) fn extract_text_from_json<F>(
    content: Option<&serde_json::Value>,
    ttf: &str,
    clean_fn: F,
) -> Option<String>
where
    F: Fn(&str) -> String,
{
    match content? {
        serde_json::Value::String(s) => {
            let t = clean_fn(s);
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(|item| {
            let obj = item.as_object()?;
            (obj.get("type").and_then(|v| v.as_str()) == Some(ttf))
                .then(|| obj.get("text").and_then(|v| v.as_str()))
                .flatten()
                .map(&clean_fn)
                .filter(|t| !t.is_empty())
        }),
        _ => None,
    }
}
pub(crate) fn extract_raw_text_from_json(
    content: Option<&serde_json::Value>,
    ttf: &str,
) -> Option<String> {
    extract_text_from_json(content, ttf, |s| s.trim().to_string())
}
pub(crate) fn simple_clean_text(text: &str, max: usize) -> String {
    truncate_str(text.trim().lines().next().unwrap_or("").trim(), max)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_session_meta(
    mapping: &ToolMapping,
    path: &Path,
    session_id: String,
    title: Option<String>,
    summary: Option<String>,
    project_dir: Option<String>,
    created_at: Option<i64>,
    last_active_at: Option<i64>,
) -> SessionMeta {
    SessionMeta {
        tool: mapping.tool.name.clone(),
        title,
        summary,
        project_dir,
        created_at,
        last_active_at,
        source_path: path.to_path_buf(),
        resume_command: mapping
            .session
            .resume_command
            .as_ref()
            .map(|cmd| cmd.replace("{id}", &session_id)),
        session_id,
        file_modified_ms: 0,
        file_size: 0,
    }
}
pub(crate) fn scan_sessions_with_filter<F>(
    session_dir: &Path,
    min_mtime_ms: Option<i64>,
    file_filter: F,
    parse_fn: impl Fn(&Path) -> Option<SessionMeta>,
) -> Result<Vec<SessionMeta>>
where
    F: Fn(&std::path::PathBuf) -> bool,
{
    if !session_dir.exists() {
        return Ok(Vec::new());
    }
    let mut s: Vec<SessionMeta> = walkdir_files(session_dir)?
        .into_iter()
        .filter(|e| file_filter(e))
        .filter(|e| {
            min_mtime_ms.map_or(true, |min| {
                let m = mtime_ms(e);
                m == 0 || m >= min
            })
        })
        .filter_map(|e| parse_fn(&e))
        .collect();
    s.sort_by_key(|b| std::cmp::Reverse(b.last_active_at));
    Ok(s)
}

// ── Gemini (inline) ──

#[derive(serde::Deserialize, Default)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
struct GeminiSession {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    title: Option<String>,
    #[serde(rename = "startTime")]
    start_time: Option<String>,
    #[serde(rename = "lastUpdated")]
    last_updated: Option<String>,
    messages: Option<Vec<GeminiMessage>>,
}
#[derive(serde::Deserialize, Default)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
struct GeminiMessage {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    content: Option<serde_json::Value>,
    tokens: Option<GeminiTokens>,
    model: Option<String>,
}
#[derive(serde::Deserialize, Default)]
struct GeminiTokens {
    input: Option<i64>,
    output: Option<i64>,
    cached: Option<i64>,
    thoughts: Option<i64>,
}

fn scan_gemini_sessions(
    config_dir: &Path,
    mapping: &ToolMapping,
    min_mtime_ms: Option<i64>,
) -> Result<Vec<SessionMeta>> {
    let d = config_dir.join(mapping.session.path());
    scan_sessions_with_filter(
        &d,
        min_mtime_ms,
        |e| {
            let n = e.file_name().unwrap_or_default().to_string_lossy();
            n.starts_with("session-") && n.ends_with(".json")
        },
        |p| parse_gemini_session(p, mapping),
    )
}

fn parse_gemini_session(path: &Path, mapping: &ToolMapping) -> Option<SessionMeta> {
    let s: GeminiSession = read_json(path)?;
    let sid = s.session_id.unwrap_or_else(|| {
        fallback_session_id(path)
            .map(|f| f.strip_prefix("session-").unwrap_or(&f).to_string())
            .unwrap_or_default()
    });
    let cat = s.start_time.as_deref().and_then(parse_iso_timestamp);
    let lat = s
        .last_updated
        .as_deref()
        .and_then(parse_iso_timestamp)
        .or(cat)
        .or_else(|| mtime_ms_nonzero(path));
    let title = s.title.or_else(|| {
        s.messages.as_ref().and_then(|msgs| {
            let texts: Vec<String> = msgs
                .iter()
                .filter(|m| m.msg_type.as_deref() == Some("user"))
                .filter_map(|m| m.content.as_ref().and_then(|c| c.as_str()))
                .map(clean_gemini_user_text)
                .filter(|s| !s.is_empty())
                .take(3)
                .collect();
            build_user_title(&texts)
        })
    });
    Some(build_session_meta(
        mapping, path, sid, title, None, None, cat, lat,
    ))
}

fn clean_gemini_user_text(text: &str) -> String {
    let t = text.trim();
    if t.starts_with('\n') || t.contains("You are an AI agent") {
        return String::new();
    }
    let fl = t.lines().next().unwrap_or("").trim();
    if fl.starts_with('@') && fl.lines().count() == 1 {
        if let Some(a) = t
            .lines()
            .skip(1)
            .map(|l| l.trim())
            .find(|l| !l.is_empty() && !l.starts_with('-') && !l.starts_with("Content from"))
        {
            return truncate_str(a, 80);
        }
    }
    if fl.is_empty() {
        String::new()
    } else {
        truncate_str(fl, 80)
    }
}

fn extract_gemini_usage(session: &SessionMeta) -> Result<Vec<UsageSummary>> {
    let gs: GeminiSession = read_json(&session.source_path)
        .ok_or_else(|| anyhow::anyhow!("failed to read gemini session"))?;
    let msgs = match gs.messages {
        Some(m) => m,
        None => return Ok(Vec::new()),
    };
    let (mut mu, mut mc): (
        std::collections::HashMap<String, TokenUsage>,
        std::collections::HashMap<String, i64>,
    ) = (
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    );
    for msg in &msgs {
        if msg.msg_type.as_deref() != Some("gemini") {
            continue;
        }
        if let Some(tk) = &msg.tokens {
            if tk.input.unwrap_or(0) == 0
                && tk.output.unwrap_or(0) == 0
                && tk.thoughts.unwrap_or(0) == 0
                && tk.cached.unwrap_or(0) == 0
            {
                continue;
            }
            let e = mu
                .entry(msg.model.as_deref().unwrap_or("gemini-2.5-pro").to_string())
                .or_default();
            e.input_tokens += tk.input.unwrap_or(0);
            e.output_tokens += tk.output.unwrap_or(0) + tk.thoughts.unwrap_or(0);
            e.cache_read_tokens += tk.cached.unwrap_or(0);
            *mc.entry(msg.model.as_deref().unwrap_or("gemini-2.5-pro").to_string())
                .or_default() += 1;
        }
    }
    if mu.is_empty() {
        return Ok(Vec::new());
    }
    Ok(usages_from_maps(
        &session.tool,
        mu,
        &mc,
        session.last_active_at,
    ))
}

// ── OpenCode (inline) ──

#[derive(serde::Deserialize, Default)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
struct OpenCodeSession {
    id: Option<String>,
    title: Option<String>,
    directory: Option<String>,
    time_created: Option<i64>,
    time_updated: Option<i64>,
}

fn scan_opencode_sessions(
    config_dir: &Path,
    mapping: &ToolMapping,
    min_mtime_ms: Option<i64>,
) -> Result<Vec<SessionMeta>> {
    let session_dir = if mapping.session.path.as_deref() == Some("storage/session") {
        let default_data = dirs::home_dir()
            .unwrap_or_default()
            .join(".local")
            .join("share");
        let dd = std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or(default_data);
        dd.join("opencode").join("storage").join("session")
    } else {
        config_dir.join(mapping.session.path.as_deref().unwrap_or("storage/session"))
    };
    scan_sessions_with_filter(
        &session_dir,
        min_mtime_ms,
        |e| e.extension().is_some_and(|ext| ext == "json"),
        |p| parse_opencode_session(p, mapping),
    )
}

fn parse_opencode_session(path: &Path, mapping: &ToolMapping) -> Option<SessionMeta> {
    let s: OpenCodeSession = read_json(path)?;
    let sid =
        s.id.unwrap_or_else(|| fallback_session_id(path).unwrap_or_default());
    if sid.is_empty() {
        return None;
    }
    let title = s.title.or_else(|| {
        s.directory.as_ref().and_then(|d| {
            std::path::PathBuf::from(d)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
        })
    });
    Some(build_session_meta(
        mapping,
        path,
        sid,
        title,
        None,
        s.directory,
        s.time_created,
        s.time_updated,
    ))
}
