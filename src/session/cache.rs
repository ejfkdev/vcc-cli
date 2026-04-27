use crate::store::TomlStore;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

trait JsonCache: serde::Serialize + for<'de> serde::Deserialize<'de> + Default {
    const VERSION: u32;
    const FILE_NAME: &'static str;
    fn version(&self) -> u32;
    fn reset(&mut self);
    fn cache_path() -> PathBuf {
        TomlStore::default_root()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(Self::FILE_NAME)
    }
}

fn load_cache<T: JsonCache>() -> Result<T> {
    let p = T::cache_path();
    if !p.exists() {
        let mut c = T::default();
        c.reset(); // 确保新缓存的 version 字段正确初始化
        return Ok(c);
    }
    let mut c: T = serde_json::from_str(&std::fs::read_to_string(&p)?)?;
    if c.version() != T::VERSION {
        c.reset();
    }
    Ok(c)
}

fn save_cache<T: JsonCache>(cache: &T) -> Result<()> {
    let p = T::cache_path();
    let t = p.with_extension("json.tmp");
    std::fs::write(&t, serde_json::to_string(cache)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&t, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&t, &p)?;
    Ok(())
}

fn cache_key(tool: &str, sid: &str) -> String {
    format!("{}/{}", tool, sid)
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}
fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

// ── Subagent 文件状态 ──

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct SubagentFileState {
    pub modified_ms: i64,
    pub file_size: u64,
}

// ── 统一缓存 ──

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct UnifiedCache {
    pub version: u32,
    pub sessions: HashMap<String, CachedSession>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct CachedSession {
    // session 元数据
    pub source_path: String,
    pub file_modified_ms: i64,
    pub file_size: u64,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<i64>,

    // usage 数据（按天分组）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<CachedUsageData>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct CachedUsageData {
    pub extracted_at: i64,
    /// 已处理的字节范围列表（有序，不重叠）
    /// 例：[(10, 20), (50, 70), (80, 90)] 表示文件偏移 10-20, 50-70, 80-90 已处理
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processed_ranges: Vec<(u64, u64)>,
    /// 0=全量读取, >0=只读取了 [first_byte_offset, last_byte_offset) 范围（旧字段，兼容）
    #[serde(default, skip_serializing_if = "is_zero")]
    pub first_byte_offset: u64,
    pub last_byte_offset: u64,
    /// 按 "YYYY-MM-DD" 分组的 usage 数据
    pub daily: HashMap<String, Vec<CachedDailyUsage>>,
    /// Codex 增量状态
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_prev_total: Option<CachedCodexTotal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_current_model: Option<String>,
    /// 跟踪每个 subagent 文件的状态（文件名 → 状态）
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub subagent_files: HashMap<String, SubagentFileState>,
    /// 上次解析时使用的 range_start_ms（None=全量解析，Some(ms)=只解析了 ms 之后的数据）
    /// 用于检测缓存数据是否完整：如果缓存是范围解析但请求全量，需要按 Miss 处理
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parsed_range_start_ms: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct CachedDailyUsage {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub cache_creation_5m_tokens: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub cache_creation_1h_tokens: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub web_search_requests: i64,
    pub request_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl CachedDailyUsage {
    pub fn from_usage_summary(u: &super::model::UsageSummary) -> Self {
        CachedDailyUsage {
            model: u.model.clone(),
            input_tokens: u.usage.input_tokens,
            output_tokens: u.usage.output_tokens,
            cache_read_tokens: u.usage.cache_read_tokens,
            cache_creation_tokens: u.usage.cache_creation_tokens,
            cache_creation_5m_tokens: u.usage.cache_creation_5m_tokens,
            cache_creation_1h_tokens: u.usage.cache_creation_1h_tokens,
            web_search_requests: u.usage.web_search_requests,
            request_count: u.request_count,
            cost_usd: u.cost_usd,
        }
    }

    pub fn to_usage_summary(&self, tool: &str, date: String) -> super::model::UsageSummary {
        super::model::UsageSummary {
            tool: tool.to_string(),
            model: self.model.clone(),
            usage: super::model::TokenUsage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cache_read_tokens: self.cache_read_tokens,
                cache_creation_tokens: self.cache_creation_tokens,
                cache_creation_5m_tokens: self.cache_creation_5m_tokens,
                cache_creation_1h_tokens: self.cache_creation_1h_tokens,
                web_search_requests: self.web_search_requests,
            },
            request_count: self.request_count,
            date: Some(date),
            cost_usd: self.cost_usd,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct CachedCodexTotal {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_input_tokens: i64,
    pub reasoning_output_tokens: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheStatus {
    Hit,
    Incremental,
    Miss,
}

impl JsonCache for UnifiedCache {
    const VERSION: u32 = 10;
    const FILE_NAME: &'static str = "session-cache.json";
    fn version(&self) -> u32 {
        self.version
    }
    fn reset(&mut self) {
        self.version = Self::VERSION;
        // 清除 usage 数据，强制重新解析（VERSION 升级可能改变数据结构或修复 bug）
        for session in self.sessions.values_mut() {
            session.usage = None;
        }
    }
}

impl UnifiedCache {
    pub fn load() -> Result<Self> {
        load_cache::<Self>()
    }
    pub fn save(&self) -> Result<()> {
        save_cache(self)
    }

    // ── Session 元数据操作 ──

    pub fn get_session(&self, tool: &str, sid: &str) -> Option<&CachedSession> {
        self.sessions.get(&cache_key(tool, sid))
    }

    pub fn upsert_session(&mut self, session: &super::model::SessionMeta) -> bool {
        let (mms, fs) = if session.file_modified_ms > 0 || session.file_size > 0 {
            (session.file_modified_ms, session.file_size)
        } else {
            match file_meta(&session.source_path) {
                Some(m) => m,
                None => return false,
            }
        };
        let key = cache_key(&session.tool, &session.session_id);
        let ne = CachedSession {
            source_path: session.source_path.to_string_lossy().to_string(),
            file_modified_ms: mms,
            file_size: fs,
            session_id: session.session_id.clone(),
            title: session.title.clone(),
            summary: session.summary.clone(),
            project_dir: session.project_dir.clone(),
            created_at: session.created_at,
            last_active_at: session.last_active_at,
            usage: None, // 不覆盖已有 usage 数据
        };
        if let Some(ex) = self.sessions.get(&key) {
            if ex.session_id == ne.session_id
                && ex.title == ne.title
                && ex.summary == ne.summary
                && ex.project_dir == ne.project_dir
                && ex.created_at == ne.created_at
                && ex.last_active_at == ne.last_active_at
                && ex.file_modified_ms == ne.file_modified_ms
                && ex.file_size == ne.file_size
            {
                return false;
            }
            // 保留已有 usage 数据
            let mut updated = ne;
            updated.usage = ex.usage.clone();
            self.sessions.insert(key, updated);
        } else {
            self.sessions.insert(key, ne);
        }
        true
    }

    pub fn find_tool_by_session_id(&self, sid: &str) -> Option<String> {
        self.sessions
            .iter()
            .find(|(_, c)| c.session_id == sid)
            .and_then(|(k, _)| k.split('/').next().map(|s| s.to_string()))
    }

    // ── Usage 数据操作 ──

    pub fn get_usage(&self, tool: &str, sid: &str) -> Option<&CachedUsageData> {
        self.sessions.get(&cache_key(tool, sid)).and_then(|s| s.usage.as_ref())
    }

    pub fn update_usage(&mut self, tool: &str, sid: &str, data: CachedUsageData) {
        let key = cache_key(tool, sid);
        if let Some(session) = self.sessions.get_mut(&key) {
            session.usage = Some(data);
        }
    }

    /// 从缓存中加载指定日期范围内的 usage 数据
    pub fn load_usages_in_range(
        &self,
        tool: &str,
        sid: &str,
        start_ms: Option<i64>,
    ) -> Vec<super::model::UsageSummary> {
        let session = match self.sessions.get(&cache_key(tool, sid)) {
            Some(s) => s,
            None => return Vec::new(),
        };
        let usage_data = match &session.usage {
            Some(d) => d,
            None => return Vec::new(),
        };
        let start_date: Option<String> = start_ms.map(super::model::ms_to_date);
        usage_data
            .daily
            .iter()
            .filter(|(date, _)| match &start_date {
                Some(sd) => date.as_str() >= sd.as_str(),
                None => true,
            })
            .flat_map(|(date, usages)| {
                usages
                    .iter()
                    .map(|cu| cu.to_usage_summary(tool, date.clone()))
            })
            .collect()
    }

    /// 检查缓存状态
    pub fn check_cache_status(
        &self,
        tool: &str,
        sid: &str,
        source_path: &std::path::Path,
    ) -> CacheStatus {
        let session = match self.sessions.get(&cache_key(tool, sid)) {
            Some(s) => s,
            None => return CacheStatus::Miss,
        };
        let meta = match std::fs::metadata(source_path) {
            Ok(m) => m,
            Err(_) => return CacheStatus::Miss,
        };
        let sz = meta.len();
        let ms = super::meta_mtime_ms(&meta);
        if sz == session.file_size && ms == session.file_modified_ms {
            // 只有 usage 数据存在时才算 Hit，否则只是元数据匹配
            if session.usage.is_some() {
                CacheStatus::Hit
            } else {
                CacheStatus::Miss
            }
        } else if sz > session.file_size {
            CacheStatus::Incremental
        } else {
            CacheStatus::Miss
        }
    }

    pub fn purge_missing(&mut self) {
        self.sessions
            .retain(|_, c| std::path::Path::new(&c.source_path).exists());
    }
}

/// 合并字节范围列表，将相邻/重叠段合并
/// 例：[(0,10), (10,20), (50,70)] → [(0,20), (50,70)]
pub(crate) fn merge_ranges(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_by_key(|r| r.0);
    let mut merged = vec![ranges[0]];
    for (start, end) in ranges.into_iter().skip(1) {
        let last = merged.last_mut().unwrap();
        if start <= last.1 {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

/// 给定文件大小和已处理范围，返回需要处理的范围列表
/// 例：file_size=100, processed=[(10,20), (50,70), (80,90)]
///   → [(0,10), (20,50), (70,80), (90,100)]
#[allow(dead_code)]
pub(crate) fn unprocessed_ranges(
    file_size: u64,
    processed: &[(u64, u64)],
) -> Vec<(u64, u64)> {
    if file_size == 0 {
        return Vec::new();
    }
    let mut gaps = Vec::new();
    let mut cursor: u64 = 0;
    for &(start, end) in processed {
        if start > cursor {
            gaps.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if cursor < file_size {
        gaps.push((cursor, file_size));
    }
    gaps
}

impl CachedUsageData {
    /// 获取有效的已处理字节范围列表
    /// 优先使用 processed_ranges，否则从 first_byte_offset/last_byte_offset 生成
    pub fn effective_processed_ranges(&self) -> Vec<(u64, u64)> {
        if !self.processed_ranges.is_empty() {
            merge_ranges(self.processed_ranges.clone())
        } else if self.last_byte_offset > 0 {
            vec![(self.first_byte_offset, self.last_byte_offset)]
        } else {
            Vec::new()
        }
    }

    /// 添加新处理的字节范围并合并
    #[allow(dead_code)]
    pub fn add_processed_range(&mut self, start: u64, end: u64) {
        let mut ranges = self.effective_processed_ranges();
        ranges.push((start, end));
        self.processed_ranges = merge_ranges(ranges);
        self.first_byte_offset = self.processed_ranges.first().map(|r| r.0).unwrap_or(0);
        self.last_byte_offset = self.processed_ranges.last().map(|r| r.1).unwrap_or(0);
    }

    /// 检查已处理范围是否覆盖整个文件 [0, file_size]
    pub fn is_fully_processed(&self, file_size: u64) -> bool {
        if file_size == 0 {
            return false;
        }
        let ranges = self.effective_processed_ranges();
        ranges.len() == 1 && ranges[0].0 == 0 && ranges[0].1 >= file_size
    }
}

pub(crate) fn file_meta(source_path: &std::path::Path) -> Option<(i64, u64)> {
    let m = std::fs::metadata(source_path).ok()?;
    let ms = super::meta_mtime_ms(&m);
    if ms == 0 {
        return None;
    }
    Some((ms, m.len()))
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
