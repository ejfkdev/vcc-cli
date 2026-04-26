use anyhow::Result;
use sonic_rs::{get_many, pointer, JsonValueTrait, PointerTree};
use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::adapter::mapping::ToolMapping;
use crate::session::cache::SubagentFileState;
use crate::session::model::{self, SessionMeta, TokenUsage, UsageSummary};

/// 全局 subagent rayon 线程池（避免每个 session 重复创建/销毁线程池）
/// 6 线程：在 16 核机器上，全局 rayon 用 ~10 线程，sub_pool 用 6 线程
static SUB_POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    rayon::ThreadPoolBuilder::new()
        .num_threads(6)
        .build()
        .unwrap()
});

/// 从 JSONL 行字节中快速提取时间戳（毫秒），用于 range_start_ms 提前终止判断
/// 使用 memmem SIMD 加速搜索 `"timestamp":"` 模式
fn extract_line_timestamp_ms(line: &[u8]) -> Option<i64> {
    const NEEDLE: &[u8] = b"\"timestamp\":\"";
    let start = memchr::memmem::find(line, NEEDLE)?;
    let ts_start = start + NEEDLE.len();
    // 找结束引号
    let ts_end = line[ts_start..].iter().position(|&b| b == b'"')?;
    let ts_str = std::str::from_utf8(&line[ts_start..ts_start + ts_end]).ok()?;
    super::parse_iso_timestamp(ts_str)
}

/// 二分查找：在 data 中找时间戳 >= target_ms 的最小行偏移
fn find_range_start_offset_fn(data: &[u8], target_ms: i64) -> usize {
    let mut lo: usize = 0;
    let mut hi: usize = data.len();
    for _ in 0..20 {
        let mid = lo + (hi - lo) / 2;
        let sample_start = mid.saturating_sub(100);
        let sample_end = (mid + 1_000_000).min(data.len());
        let sample = &data[sample_start..sample_end];
        let first_nl = match memchr::memchr(b'\n', sample) {
            Some(nl) => nl + 1,
            None => continue,
        };
        let mut found_ts: Option<i64> = None;
        let mut pos = first_nl;
        for _ in 0..50 {
            let nl = match memchr::memchr(b'\n', &sample[pos..]) {
                Some(nl) => pos + nl,
                None => break,
            };
            let line = &sample[pos..nl];
            let check_end = line.len().min(300);
            if memchr::memmem::find(&line[..check_end], b"\"timestamp\":\"").is_some() {
                if let Some(ts) = extract_line_timestamp_ms(line) {
                    found_ts = Some(ts);
                    break;
                }
            }
            pos = nl + 1;
        }
        match found_ts {
            Some(_ts) if _ts >= target_ms => hi = mid,
            Some(_) => lo = mid,
            None => lo = mid,
        }
    }
    lo.saturating_sub(1_000_000)
}

#[derive(Debug, Clone, Default)]
struct MsgEntry {
    model: String,
    usage: TokenUsage,
    has_stop: bool,
    is_fast: bool,
    /// 从 JSONL costUSD 字段读取的预计算费用
    cost_usd: Option<f64>,
    /// 消息时间戳（毫秒），用于 --by day 按天聚合
    timestamp_ms: i64,
}

/// 四舍五入 costUSD 到 8 位小数避免浮点漂移
fn round_cost_usd(v: f64) -> f64 {
    (v * 100_000_000.0).round() / 100_000_000.0
}

#[derive(serde::Deserialize, Default)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
struct ClaudeLine {
    #[serde(rename = "type")]
    line_type: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "custom-title")]
    custom_title: Option<String>,
    summary: Option<String>,
    #[serde(rename = "isCompactSummary")]
    is_compact_summary: Option<bool>,
    message: Option<ClaudeMessage>,
    leaf_uuid: Option<String>,
    #[serde(rename = "costUSD")]
    cost_usd: Option<f64>,
}

#[derive(serde::Deserialize, Default)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
struct ClaudeMessage {
    role: Option<String>,
    content: Option<serde_json::Value>,
    usage: Option<ClaudeUsage>,
    id: Option<String>,
    stop_reason: Option<String>,
    model: Option<String>,
}

#[derive(serde::Deserialize, Default, Clone)]
struct ClaudeUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
    cache_creation: Option<CacheCreationDetail>,
    server_tool_use: Option<ServerToolUse>,
    speed: Option<String>,
}

#[derive(serde::Deserialize, Default, Clone)]
struct CacheCreationDetail {
    ephemeral_5m_input_tokens: Option<i64>,
    ephemeral_1h_input_tokens: Option<i64>,
}

#[derive(serde::Deserialize, Default, Clone)]
struct ServerToolUse {
    web_search_requests: Option<i64>,
}

impl ClaudeUsage {
    fn to_token_usage(&self) -> TokenUsage {
        let (cache_5m, cache_1h) = match &self.cache_creation {
            Some(detail) => (
                detail.ephemeral_5m_input_tokens.unwrap_or(0),
                detail.ephemeral_1h_input_tokens.unwrap_or(0),
            ),
            None => (0, 0),
        };
        TokenUsage {
            input_tokens: self.input_tokens.unwrap_or(0),
            output_tokens: self.output_tokens.unwrap_or(0),
            cache_read_tokens: self.cache_read_input_tokens.unwrap_or(0),
            cache_creation_tokens: self.cache_creation_input_tokens.unwrap_or(0),
            cache_creation_5m_tokens: cache_5m,
            cache_creation_1h_tokens: cache_1h,
            web_search_requests: self
                .server_tool_use
                .as_ref()
                .and_then(|s| s.web_search_requests)
                .unwrap_or(0),
        }
    }

    /// 是否为 fast 模式
    fn is_fast_mode(&self) -> bool {
        self.speed.as_deref() == Some("fast")
    }
}

fn is_assistant_line(line: &str) -> bool {
    line.contains("\"type\":\"assistant\"") || line.contains("\"type\": \"assistant\"")
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
    let session_cache = crate::session::cache::UnifiedCache::load().ok();
    let mut sessions = Vec::new();
    for (entry, meta) in walkdir_entries(&session_dir)? {
        if entry.extension().map_or(true, |e| e != "jsonl") {
            continue;
        }
        let mtime = file_mtime_ms(&meta);
        if min_mtime_ms.is_some_and(|ms| mtime > 0 && mtime < ms) {
            continue;
        }
        let file_name = entry.file_name().unwrap_or_default().to_string_lossy();
        if !mapping.session.exclude_prefix.is_empty()
            && file_name.starts_with(&mapping.session.exclude_prefix)
        {
            continue;
        }
        if let Some(ref sc) = session_cache {
            if let Some(s) = session_meta_from_cache(&entry, mapping, sc, &meta, mtime) {
                sessions.push(s);
                continue;
            }
        }
        if let Some(mut s) = parse_session_meta(&entry, mapping) {
            s.file_modified_ms = mtime;
            s.file_size = meta.len();
            sessions.push(s);
        }
    }
    sessions.sort_by_key(|b| std::cmp::Reverse(b.last_active_at));
    Ok(sessions)
}

fn session_meta_from_cache(
    path: &Path,
    mapping: &ToolMapping,
    cache: &crate::session::cache::UnifiedCache,
    meta: &std::fs::Metadata,
    mtime: i64,
) -> Option<SessionMeta> {
    let cached = cache.get_session("claude", &path.file_stem()?.to_string_lossy())?;
    if cached.source_path != path.to_string_lossy().as_ref()
        || meta.len() != cached.file_size
        || mtime != cached.file_modified_ms
    {
        return None;
    }
    let mut s = super::build_session_meta(
        mapping,
        path,
        cached.session_id.clone(),
        cached.title.clone(),
        cached.summary.clone(),
        cached.project_dir.clone(),
        cached.created_at,
        cached.last_active_at,
    );
    s.file_modified_ms = mtime;
    s.file_size = meta.len();
    Some(s)
}

fn parse_session_meta(path: &Path, mapping: &ToolMapping) -> Option<SessionMeta> {
    let (head, tail) = read_head_tail(path, 30, 30).ok()?;
    let mut sid = None;
    let mut cwd = None;
    let mut created_at = None;
    let mut custom_title = None;
    let mut user_texts: Vec<String> = Vec::new();
    let mut last_active_at = None;
    let mut summary = None;
    let mut compact_title = None;
    for line in head.iter().chain(&tail) {
        let Ok(cl) = sonic_rs::from_str::<ClaudeLine>(line) else {
            continue;
        };
        if cl.session_id.is_some() {
            sid = cl.session_id;
        }
        if cwd.is_none() {
            cwd = cl.cwd;
        }
        if created_at.is_none() {
            created_at = cl
                .timestamp
                .as_ref()
                .and_then(|ts| super::parse_iso_timestamp(ts));
        }
        if cl.custom_title.is_some() {
            custom_title = cl.custom_title;
        }
        if let Some(ts) = &cl.timestamp {
            last_active_at = super::parse_iso_timestamp(ts);
        }
        if cl.line_type.as_deref() == Some("summary") {
            summary = cl.summary;
        }
        if cl.is_compact_summary == Some(true) && compact_title.is_none() {
            compact_title = cl
                .message
                .as_ref()
                .and_then(|m| extract_raw_text(&m.content))
                .and_then(|r| extract_primary_request(&r));
        }
        if cl.line_type.as_deref() == Some("user") && user_texts.len() < 3 {
            if let Some(text) = cl.message.as_ref().and_then(|m| extract_text(&m.content)) {
                if !text.starts_with('<') && !text.starts_with('[') {
                    user_texts.push(text);
                }
            }
        }
    }
    if compact_title.is_none() {
        compact_title = scan_compact_summary(path);
    }
    if sid.is_none() {
        sid = super::fallback_session_id(path);
    }
    let sid = sid?;
    let title = custom_title
        .or(compact_title)
        .or(super::fallback_title(&user_texts, cwd.as_deref()));
    Some(super::build_session_meta(
        mapping,
        path,
        sid,
        title,
        summary,
        cwd,
        created_at,
        last_active_at,
    ))
}

/// 提取结果（包含 subagent 文件状态）
pub(crate) struct ExtractResult {
    pub usages: Vec<UsageSummary>,
    pub first_byte_offset: u64,
    pub subagent_files: HashMap<String, SubagentFileState>,
}

pub(crate) fn extract_usage(
    session: &SessionMeta,
    range_start_ms: Option<i64>,
) -> Result<ExtractResult> {
    let t0 = std::time::Instant::now();

    let tool = session.tool.clone();
    let last_active_at = session.last_active_at;
    let file_size = session.file_size;

    // 先解析 main（独占全局 rayon），再解析 subagent（独立线程池）
    let mut messages: HashMap<String, MsgEntry> = HashMap::new();
    let fbo = parse_file_into_messages(&session.source_path, 0, &mut messages, &[], range_start_ms, true)?;
    let main_elapsed = t0.elapsed().as_secs_f64() * 1000.0;

    let t_sub = std::time::Instant::now();
    let (subagent_files, sub_results) = parse_subagent_messages_parallel(&session.source_path, range_start_ms)?;
    let sub_elapsed = t_sub.elapsed().as_secs_f64() * 1000.0;

    let t2 = std::time::Instant::now();
    for sub_msgs in sub_results {
        merge_msg_entries(&mut messages, sub_msgs);
    }
    let t3 = std::time::Instant::now();

    let result = ExtractResult {
        usages: summarize_messages(&tool, &messages, last_active_at)?,
        first_byte_offset: fbo,
        subagent_files,
    };

    if file_size > 500 * 1024 * 1024 {
        let wall = t0.elapsed().as_secs_f64() * 1000.0;
        eprintln!("[PERF]     {} wall={:.0}ms(main={:.0}ms,sub={:.0}ms) merge={:.0}ms summarize={:.0}ms size={:.1}MB",
            session.source_path.file_name().unwrap_or_default().to_string_lossy(),
            wall, main_elapsed, sub_elapsed,
            (t3-t2).as_secs_f64()*1000.0, (std::time::Instant::now()-t3).as_secs_f64()*1000.0,
            file_size as f64 / 1e6);
    }
    Ok(result)
}

pub(crate) fn extract_usage_incremental(
    session: &SessionMeta,
    from_byte: u64,
) -> Result<Vec<UsageSummary>> {
    let mut messages: HashMap<String, MsgEntry> = HashMap::new();
    // 只读主文件新增字节（增量路径不需要 range_start_ms 过滤）
    let _ = parse_file_into_messages(&session.source_path, from_byte, &mut messages, &[], None, true)?;
    summarize_messages(&session.tool, &messages, session.last_active_at)
}

/// 增量解析 subagent：只解析新增或变化的 subagent 文件
/// 返回 (增量 messages, 当前所有 subagent 文件状态)
pub(crate) fn extract_subagent_incremental(
    session: &SessionMeta,
    cached_subagent_files: &HashMap<String, SubagentFileState>,
) -> Result<(Vec<UsageSummary>, HashMap<String, SubagentFileState>)> {
    let sub_dir = session.source_path.with_extension("").join("subagents");
    if !sub_dir.is_dir() {
        return Ok((Vec::new(), cached_subagent_files.clone()));
    }

    let mut messages = HashMap::new();
    let mut current_state = HashMap::new();

    for entry in std::fs::read_dir(&sub_dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.starts_with("agent-") || !path.extension().is_some_and(|e| e == "jsonl") {
            continue;
        }
        let (mms, fs) = match super::cache::file_meta(&path) {
            Some(m) => m,
            None => continue,
        };
        current_state.insert(
            name.to_string(),
            SubagentFileState {
                modified_ms: mms,
                file_size: fs,
            },
        );

        // 只解析新增或变化的 subagent 文件
        let need_parse = match cached_subagent_files.get(name.as_ref()) {
            None => true,
            Some(cached) => cached.modified_ms != mms || cached.file_size != fs,
        };
        if !need_parse {
            continue;
        }

        let _ = parse_file_into_messages(&path, 0, &mut messages, &[], None, false);
    }

    let usages = summarize_messages(&session.tool, &messages, session.last_active_at)?;
    Ok((usages, current_state))
}

/// 从单个 JSONL 文件解析 assistant messages 到共享 HashMap
/// 使用 sonic-rs get_many 惰性提取字段，跳过 content 数组分配
/// 返回 first_byte_offset（0=全量读取，>0=尾部读取起始位置）
/// use_rayon: 是否在 mmap 路径中使用 rayon 并行 JSON 解析（主文件用 true，subagent 用 false）
fn parse_file_into_messages(
    path: &Path,
    from_byte: u64,
    messages: &mut HashMap<String, MsgEntry>,
    skip_ranges: &[(usize, usize)],
    range_start_ms: Option<i64>,
    use_rayon: bool,
) -> Result<u64> {
    let file = std::fs::File::open(path)?;
    let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);

    // 增量读取
    if from_byte > 0 {
        return parse_file_sequential(path, from_byte, messages, 0, None);
    }

    // 大文件：mmap 二分查找 + memchr 分行 + memmem 过滤 + JSON 解析
    // 阈值：有 range_start_ms 时降到 1MB（利用 binary search 跳过旧数据），否则 64MB
    let mmap_threshold = if range_start_ms.is_some() { 1024 * 1024 } else { 64 * 1024 * 1024 };
    if file_size > mmap_threshold {
        return parse_file_tail_mmap(path, messages, skip_ranges, range_start_ms, use_rayon);
    }

    // 小文件：全量顺序读取
    parse_file_sequential(path, 0, messages, 0, range_start_ms)
}

/// 顺序读取（全量/增量）
/// 读取 [from_byte, end_byte) 范围，end_byte=0 表示读到文件末尾
/// 顺序读取（全量/增量）
/// 读取 [from_byte, end_byte) 范围，end_byte=0 表示读到文件末尾
fn parse_file_sequential(
    path: &Path,
    from_byte: u64,
    messages: &mut HashMap<String, MsgEntry>,
    end_byte: u64,
    range_start_ms: Option<i64>,
) -> Result<u64> {
    let mut reader = std::io::BufReader::with_capacity(64 * 1024 * 1024, std::fs::File::open(path)?);
    if from_byte > 0 {
        reader.seek(SeekFrom::Start(from_byte))?;
    }
    let tree = build_pointer_tree();
    let mut skip_first = from_byte > 0;
    let mut bytes_read: u64 = from_byte;
    for line in reader.lines() {
        let line = line?;
        if skip_first {
            skip_first = false;
            continue;
        }
        // 字节范围限制：end_byte > 0 时，读到超过 end_byte 就停
        if end_byte > 0 {
            bytes_read += line.len() as u64 + 1; // +1 for newline
            if bytes_read > end_byte {
                break;
            }
        }
        if !is_assistant_line(&line) {
            continue;
        }
        // 时间戳过滤：跳过早于 range_start_ms 的行
        if let Some(rms) = range_start_ms {
            if let Some(ts) = line.find("\"timestamp\":\"").and_then(|p| {
                let start = p + 13;
                line[start..].find('"').and_then(|end| {
                    super::parse_iso_timestamp(&line[start..start + end])
                })
            }) {
                if ts < rms {
                    continue;
                }
            }
        }
        process_assistant_line(&line, &tree, messages);
    }
    Ok(if from_byte > 0 { from_byte } else { 0 })
}

/// 大文件读取：mmap + 二分查找 + 倒序分行 + SIMD 行首关键字 + 并行 JSON 解析
/// 流程：
/// 1. mmap 映射文件
/// 2. 二分查找确定下界 bs_offset
/// 3. 在 [bs_offset, end) 范围内用 memchr 一次扫描找所有换行符位置（分行）
/// 4. 倒序遍历行，仅检查每行前200字节用 memmem(SIMD) 查 assistant 关键字
/// 5. 收集匹配行文本，用 rayon 并行解析 JSON，最后合并结果
fn parse_file_tail_mmap(
    path: &Path,
    messages: &mut HashMap<String, MsgEntry>,
    _skip_ranges: &[(usize, usize)],
    range_start_ms: Option<i64>,
    use_rayon: bool,
) -> Result<u64> {
    let t0 = std::time::Instant::now();
    let file = std::fs::File::open(path)?;
    let file_size = file.metadata()?.len();
    if file_size == 0 {
        return Ok(0);
    }
    let is_large = file_size > 500 * 1024 * 1024;

    let mmap = match unsafe { memmap2::Mmap::map(&file) } {
        Ok(m) => m,
        Err(_) => return parse_file_sequential(path, 0, messages, 0, range_start_ms),
    };
    let data: &[u8] = &mmap;
    if is_large {
        eprintln!("[PERF]       mmap_open: {:.1}ms ({:.1}MB)", t0.elapsed().as_secs_f64() * 1000.0, file_size as f64 / 1e6);
    }

    // 二分查找确定下界
    let bs_offset: usize = if let Some(rms) = range_start_ms {
        if file_size > 64 * 1024 * 1024 {
            let t_bs = std::time::Instant::now();
            let offset = find_range_start_offset_fn(data, rms);
            if is_large {
                eprintln!("[PERF]       binary_search: {:.1}ms, skip to {:.1}MB ({:.0}% of {:.0}MB)",
                    t_bs.elapsed().as_secs_f64() * 1000.0,
                    offset as f64 / 1e6, offset as f64 / file_size as f64 * 100.0, file_size as f64 / 1e6);
            }
            offset
        } else {
            0
        }
    } else {
        0
    };

    // 分行：用 memchr 一次扫描找所有换行符位置
    let t_lines = std::time::Instant::now();
    let nl_positions: Vec<usize> = memchr::memchr_iter(b'\n', &data[bs_offset..])
        .map(|p| bs_offset + p)
        .collect();

    // 构建行范围列表 (start, end)
    let mut line_ranges: Vec<(usize, usize)> = Vec::with_capacity(nl_positions.len() + 1);
    let mut prev = bs_offset;
    for &nl in &nl_positions {
        if nl > prev {
            line_ranges.push((prev, nl));
        }
        prev = nl + 1;
    }
    if prev < data.len() {
        line_ranges.push((prev, data.len()));
    }

    if is_large {
        eprintln!("[PERF]       split_lines: {:.1}ms ({} lines in {}MB range)",
            t_lines.elapsed().as_secs_f64() * 1000.0,
            line_ranges.len(), (data.len() - bs_offset) as f64 / 1e6);
    }

    // SIMD 关键字查找：对每行用 memmem 查 assistant 关键字
    let mfinder1 = memchr::memmem::Finder::new(b"\"type\":\"assistant\"");
    let mfinder2 = memchr::memmem::Finder::new(b"\"type\": \"assistant\"");

    let t_filter = std::time::Instant::now();
    let mut matched_texts: Vec<&str> = Vec::new();
    for &(start, end) in line_ranges.iter().rev() {
        let line_bytes = &data[start..end];
        if mfinder1.find(line_bytes).is_none() && mfinder2.find(line_bytes).is_none() {
            continue;
        }
        if let Ok(s) = std::str::from_utf8(line_bytes) {
            matched_texts.push(s.trim());
        }
    }

    if is_large {
        eprintln!("[PERF]       simd_filter: {:.1}ms ({} matched / {} total lines)",
            t_filter.elapsed().as_secs_f64() * 1000.0,
            matched_texts.len(), line_ranges.len());
    }

    // JSON 解析
    let tree = build_pointer_tree();
    let t_parse = std::time::Instant::now();

    let local_maps: Vec<HashMap<String, MsgEntry>> = if use_rayon {
        use rayon::prelude::*;
        matched_texts.par_iter().filter_map(|line| {
            let mut msgs = HashMap::new();
            process_assistant_line(line, &tree, &mut msgs);
            if msgs.is_empty() { None } else { Some(msgs) }
        }).collect()
    } else {
        matched_texts.iter().filter_map(|line| {
            let mut msgs = HashMap::new();
            process_assistant_line(line, &tree, &mut msgs);
            if msgs.is_empty() { None } else { Some(msgs) }
        }).collect()
    };
    // merge with pre-allocation
    if messages.is_empty() && !local_maps.is_empty() {
        let total_entries: usize = local_maps.iter().map(|m| m.len()).sum();
        messages.reserve(total_entries);
    }
    for local in local_maps { merge_msg_entries(messages, local); }

    if is_large {
        eprintln!("[PERF]       parallel_parse: {:.1}ms", t_parse.elapsed().as_secs_f64() * 1000.0);
    }

    drop(mmap);

    if is_large {
        eprintln!("[PERF]       total: {:.1}ms", t0.elapsed().as_secs_f64() * 1000.0);
    }

    Ok(bs_offset as u64)
}

/// 合并解析结果到主 HashMap
fn merge_msg_entries(
    messages: &mut HashMap<String, MsgEntry>,
    new_entries: HashMap<String, MsgEntry>,
) {
    for (id, entry) in new_entries {
        let should_insert = match messages.get(&id) {
            None => true,
            Some(old) => {
                (!old.has_stop && entry.has_stop)
                    || (old.has_stop == entry.has_stop
                        && entry.usage.output_tokens > old.usage.output_tokens)
            }
        };
        if should_insert {
            messages.insert(id, entry);
        }
    }
}

/// 构建 PointerTree：6 个路径
fn build_pointer_tree() -> PointerTree {
    let mut tree = PointerTree::new();
    tree.add_path(&pointer!["message", "id"]);
    tree.add_path(&pointer!["message", "model"]);
    tree.add_path(&pointer!["message", "stop_reason"]);
    tree.add_path(&pointer!["message", "usage"]);
    tree.add_path(&pointer!["costUSD"]);
    tree.add_path(&pointer!["timestamp"]);
    tree
}

/// 处理一行 assistant 消息，提取 usage 数据并插入 HashMap
#[allow(clippy::too_many_arguments)]
fn process_assistant_line(
    line: &str,
    tree: &PointerTree,
    messages: &mut HashMap<String, MsgEntry>,
) {
    let nodes = match get_many(line, tree) {
        Ok(n) => n,
        Err(_) => return,
    };
    process_line_nodes(&nodes, messages);
}

/// 从已提取的节点处理 assistant 消息
fn process_line_nodes(
    nodes: &[Option<sonic_rs::LazyValue<'_>>],
    messages: &mut HashMap<String, MsgEntry>,
) {
    // [0] message.id
    let id_str = match nodes.first().and_then(|n| n.as_ref()) {
        Some(v) => match v.as_str() {
            Some(s) => s,
            None => return,
        },
        None => return,
    };
    // [1] message.model（先检查 synthetic，避免不必要的 String 分配）
    let model_str = nodes
        .get(1)
        .and_then(|n| n.as_ref())
        .and_then(|v| v.as_str())
        .unwrap_or("claude-sonnet-4");
    // 跳过 <synthetic> 模型
    if model_str.starts_with('<') {
        return;
    }
    // [2] message.stop_reason — null 或 missing 视为无 stop
    let has_stop = nodes
        .get(2)
        .and_then(|n| n.as_ref())
        .is_some_and(|v| !v.is_null() && v.as_str().is_some());
    // [3] message.usage — 小对象，用 serde 反序列化
    let usage: ClaudeUsage = match nodes.get(3).and_then(|n| n.as_ref()) {
        Some(v) => match sonic_rs::from_str(v.as_raw_str()) {
            Ok(u) => u,
            Err(_) => return,
        },
        None => return,
    };
    // [4] costUSD
    let cost_usd = nodes
        .get(4)
        .and_then(|n| n.as_ref())
        .and_then(|v| v.as_f64());
    let new_usage = usage.to_token_usage();
    let is_fast = usage.is_fast_mode();
    // 提取时间戳用于按天聚合
    let timestamp_ms = nodes
        .get(5)
        .and_then(|n| n.as_ref())
        .and_then(|v| v.as_str())
        .and_then(super::parse_iso_timestamp)
        .unwrap_or(0);
    // 相同 messageId 的 streaming 更新：后出现的 token 值更大（单调递增）
    // 倒序路径：首次遇到即最大值 → 直接插入
    // 顺序路径：后出现的更大 → 需要替换旧值
    // 策略：总是插入/替换，但优先保留有 stop_reason 的条目
    let should_insert = match messages.get(id_str) {
        None => true,
        Some(old) => {
            // 已有 stop_reason 的不丢弃，除非新条目也有 stop 且 token 更大
            if old.has_stop && !has_stop {
                false
            } else {
                true // 新条目 token 更大或 stop 状态相同/更好
            }
        }
    };
    if !should_insert {
        return;
    }
    messages.insert(
        id_str.to_string(),
        MsgEntry {
            model: model_str.to_string(),
            usage: new_usage,
            has_stop,
            is_fast,
            cost_usd,
            timestamp_ms,
        },
    );
}

/// 解析主文件对应的 subagents/ 目录下所有 agent-*.jsonl
/// 返回 (subagent_state, 各文件的解析结果)
fn parse_subagent_messages_parallel(
    main_path: &Path,
    range_start_ms: Option<i64>,
) -> Result<(HashMap<String, SubagentFileState>, Vec<HashMap<String, MsgEntry>>)> {
    let sub_dir = main_path.with_extension("").join("subagents");
    if !sub_dir.is_dir() {
        return Ok((HashMap::new(), Vec::new()));
    }

    // 收集需要处理的文件
    let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut subagent_state = HashMap::new();
    for entry in std::fs::read_dir(&sub_dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.starts_with("agent-") || !path.extension().is_some_and(|e| e == "jsonl") {
            continue;
        }
        if let Some((mms, fs)) = super::cache::file_meta(&path) {
            // 始终记录文件状态（用于增量解析判断）
            subagent_state.insert(
                name.to_string(),
                SubagentFileState {
                    modified_ms: mms,
                    file_size: fs,
                },
            );
            // 跳过空文件
            if fs == 0 {
                continue;
            }
            // 跳过修改时间早于 range_start_ms 的文件（追加写入文件，最后修改时间=最新数据时间）
            if range_start_ms.is_some_and(|rs| mms > 0 && mms < rs) {
                continue;
            }
        }
        files.push((name.to_string(), path));
    }

    // 用全局 subagent 线程池并行解析（避免与全局 rayon 池争抢，且避免重复创建线程池）
    let results: Vec<HashMap<String, MsgEntry>> = SUB_POOL.install(|| {
        use rayon::prelude::*;
        files
            .par_iter()
            .map(|(_, path)| {
                let mut msgs = HashMap::new();
                let _ = parse_file_into_messages(path, 0, &mut msgs, &[], range_start_ms, true);
                msgs
            })
            .collect()
    });

    Ok((subagent_state, results))
}

/// 将 HashMap<String, MsgEntry> 汇总为 Vec<UsageSummary>
/// 按 (model, date) 聚合，支持 --by day 按天统计
fn summarize_messages(
    tool: &str,
    messages: &HashMap<String, MsgEntry>,
    last_active_at: Option<i64>,
) -> Result<Vec<UsageSummary>> {
    struct Agg {
        usage: TokenUsage,
        count: i64,
        cost: f64,
    }
    // key = (model_key, date_string)，合并 usage/count/cost 到单一 HashMap
    let mut agg: HashMap<(String, String), Agg> = HashMap::new();
    for msg in messages.values() {
        if !msg.has_stop || msg.usage.is_empty() {
            continue;
        }
        let model_key = if msg.is_fast {
            format!("{}:fast", msg.model)
        } else {
            msg.model.clone()
        };
        let date = if msg.timestamp_ms > 0 {
            model::ms_to_date(msg.timestamp_ms)
        } else {
            last_active_at.map(model::ms_to_date).unwrap_or_default()
        };
        let entry = agg.entry((model_key, date)).or_insert_with(|| Agg {
            usage: TokenUsage::default(),
            count: 0,
            cost: 0.0,
        });
        entry.usage += msg.usage.clone();
        entry.count += 1;
        if let Some(c) = msg.cost_usd {
            entry.cost += c;
        }
    }
    Ok(agg
        .into_iter()
        .map(|((model, date), a)| {
            let date_opt = if date.is_empty() { None } else { Some(date) };
            UsageSummary {
                tool: tool.to_string(),
                model,
                usage: a.usage,
                request_count: a.count,
                date: date_opt,
                cost_usd: if a.cost > 0.0 { Some(round_cost_usd(a.cost)) } else { None },
            }
        })
        .collect())
}

fn extract_text(content: &Option<serde_json::Value>) -> Option<String> {
    super::extract_text_from_json(content.as_ref(), "text", clean_user_text)
}
fn extract_raw_text(content: &Option<serde_json::Value>) -> Option<String> {
    super::extract_raw_text_from_json(content.as_ref(), "text")
}

fn clean_user_text(text: &str) -> String {
    let text = text.trim();
    let text = if text.starts_with("<local-command-caveat>") {
        text.find("</local-command-caveat>")
            .map(|pos| text[pos + "</local-command-caveat>".len()..].trim())
            .unwrap_or(text)
    } else {
        text
    };
    super::truncate_str(
        &redact_secrets(text.lines().next().unwrap_or("").trim()),
        80,
    )
}

fn redact_secrets(s: &str) -> String {
    let mut r = s.to_string();
    for &pat in &["apiKey:\"", "apiKey:'", "apiKey:", "API_KEY=", "api_key="] {
        if let Some(pos) = r.find(pat) {
            let start = pos + pat.len();
            let end = r[start..]
                .find(|c: char| c.is_whitespace() || c == ',' || c == '}' || c == ']' || c == '"')
                .map(|i| start + i)
                .unwrap_or(r.len());
            if end - start > 10 {
                r = format!("{}***{}", &r[..start], &r[end..]);
            }
        }
    }
    r
}

fn extract_primary_request(text: &str) -> Option<String> {
    let marker = "Primary Request and Intent";
    let after = text[text.find(marker)? + marker.len()..]
        .trim_start_matches(':')
        .trim_start();
    let end = ["\n2.", "\n\n", "\nKey ", "\n-"]
        .iter()
        .filter_map(|p| after.find(p))
        .min()
        .unwrap_or(after.len());
    let content = after[..end].trim();
    if content.is_empty() {
        None
    } else {
        Some(super::truncate_str(
            content.lines().next().unwrap_or("").trim(),
            80,
        ))
    }
}

fn scan_compact_summary(path: &Path) -> Option<String> {
    for line in std::io::BufReader::new(std::fs::File::open(path).ok()?)
        .lines()
        .take(500)
    {
        let line = line.ok()?;
        if !line.contains("isCompactSummary") {
            continue;
        }
        if let Ok(cl) = sonic_rs::from_str::<ClaudeLine>(&line) {
            if cl.is_compact_summary == Some(true) {
                if let Some(title) = cl
                    .message
                    .as_ref()
                    .and_then(|m| extract_raw_text(&m.content))
                    .and_then(|r| extract_primary_request(&r))
                {
                    return Some(title);
                }
            }
        }
    }
    None
}

fn read_head_tail(path: &Path, head_n: usize, tail_n: usize) -> Result<(Vec<String>, Vec<String>)> {
    let file = std::fs::File::open(path)?;
    let file_size = file.metadata()?.len();
    let head: Vec<String> = std::io::BufReader::new(&file)
        .lines()
        .map_while(Result::ok)
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .take(head_n)
        .collect();
    let tail = if file_size > 0 {
        let seek_pos = file_size.saturating_sub(65536);
        let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
        reader.seek(SeekFrom::Start(seek_pos))?;
        let mut buf: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        for (i, line) in reader.lines().enumerate() {
            let trimmed = line?.trim_end().to_string();
            if (seek_pos > 0 && i == 0) || trimmed.is_empty() {
                continue;
            }
            buf.push_back(trimmed);
            if buf.len() > tail_n {
                buf.pop_front();
            }
        }
        buf.into()
    } else {
        Vec::new()
    };
    Ok((head, tail))
}

fn file_mtime_ms(meta: &std::fs::Metadata) -> i64 {
    super::meta_mtime_ms(meta)
}
fn walkdir_entries(dir: &Path) -> Result<Vec<(PathBuf, std::fs::Metadata)>> {
    Ok(super::walkdir_files(dir)?
        .into_iter()
        .filter_map(|p| std::fs::symlink_metadata(&p).ok().map(|m| (p, m)))
        .collect())
}

#[cfg(test)]
mod test_unknown_fields {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_sonic_unknown_fields() {
        // Full usage format with extra fields like inference_geo, iterations, service_tier
        let full = r#"{"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0},"cache_creation_input_tokens":0,"cache_read_input_tokens":34685,"inference_geo":"us","input_tokens":463,"iterations":1,"output_tokens":0,"server_tool_use":{"web_search_requests":0},"service_tier":"standard","speed":"normal"}"#;
        let result: Result<ClaudeUsage, _> = sonic_rs::from_str(full);
        match result {
            Ok(u) => {
                assert_eq!(u.input_tokens, Some(463));
                assert_eq!(u.cache_read_input_tokens, Some(34685));
            }
            Err(e) => {
                panic!("sonic_rs REJECTS unknown fields! This causes massive undercount! Error: {:?}", e);
            }
        }
    }

    #[test]
    fn test_sonic_simple_format() {
        let simple = r#"{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":200,"cache_creation_input_tokens":0}"#;
        let result: Result<ClaudeUsage, _> = sonic_rs::from_str(simple);
        assert!(result.is_ok());
    }

    #[test]
    fn test_mmap_vs_sequential() {
        let path = std::path::Path::new("/Users/wudi/.claude/projects/-Users-wudi-Documents-project-zread-ai-ext/d09ec20e-2a1d-4261-8f0b-36de5d724101.jsonl");
        if !path.exists() {
            eprintln!("Test file not found, skipping");
            return;
        }

        let mut seq_messages: HashMap<String, MsgEntry> = HashMap::new();
        parse_file_sequential(path, 0, &mut seq_messages, 0, None).unwrap();

        let mut mmap_messages: HashMap<String, MsgEntry> = HashMap::new();
        parse_file_tail_mmap(path, &mut mmap_messages, &[], None).unwrap();

        assert_eq!(mmap_messages.len(), seq_messages.len(), "mmap and sequential should find same number of entries");
    }
}
