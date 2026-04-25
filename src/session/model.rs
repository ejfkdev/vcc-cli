use chrono::TimeZone;
use std::path::PathBuf;

/// 会话元数据
#[derive(Debug, Clone)]
pub(crate) struct SessionMeta {
    pub tool: String,
    pub session_id: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub project_dir: Option<String>,
    pub created_at: Option<i64>,
    pub last_active_at: Option<i64>,
    pub source_path: PathBuf,
    pub resume_command: Option<String>,
    pub file_modified_ms: i64,
    pub file_size: u64,
}

/// Token 用量（单次请求）
#[derive(Debug, Clone, Default)]
pub(crate) struct TokenUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    /// 5 分钟 TTL 缓存写入 token
    pub cache_creation_5m_tokens: i64,
    /// 1 小时 TTL 缓存写入 token
    pub cache_creation_1h_tokens: i64,
    /// Web 搜索请求次数
    pub web_search_requests: i64,
}

impl std::ops::AddAssign for TokenUsage {
    fn add_assign(&mut self, other: Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
        self.cache_creation_5m_tokens += other.cache_creation_5m_tokens;
        self.cache_creation_1h_tokens += other.cache_creation_1h_tokens;
        self.web_search_requests += other.web_search_requests;
    }
}

impl std::ops::SubAssign for TokenUsage {
    fn sub_assign(&mut self, other: Self) {
        self.input_tokens -= other.input_tokens;
        self.output_tokens -= other.output_tokens;
        self.cache_read_tokens -= other.cache_read_tokens;
        self.cache_creation_tokens -= other.cache_creation_tokens;
        self.cache_creation_5m_tokens -= other.cache_creation_5m_tokens;
        self.cache_creation_1h_tokens -= other.cache_creation_1h_tokens;
        self.web_search_requests -= other.web_search_requests;
    }
}

impl TokenUsage {
    pub fn total(&self) -> i64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// 按 (tool, model, date) 维度的用量汇总
#[derive(Debug, Clone)]
pub(crate) struct UsageSummary {
    pub tool: String,
    pub model: String,
    pub usage: TokenUsage,
    pub request_count: i64,
    pub date: Option<String>,
    /// 预计算费用（来自 JSONL costUSD），如果有
    pub cost_usd: Option<f64>,
}

/// 会话删除结果
#[derive(Debug, Clone, Default)]
pub(crate) struct DeleteOutcome {
    pub files_removed: u32,
    pub bytes_freed: u64,
    pub warnings: Vec<String>,
}

/// 时间范围筛选
#[derive(Debug, Clone, Copy)]
pub(crate) enum TimeRange {
    Today,
    Week,
    Month,
    All,
}

impl TimeRange {
    /// 对齐到本地时区天边界，确保同一本地日内多次运行返回相同值，缓存可复用
    pub fn start_ms(&self) -> Option<i64> {
        let now_local = chrono::Local::now();
        let today_start = now_local
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|dt| chrono::Local.from_local_datetime(&dt).single())
            .flatten()
            .map(|dt| dt.timestamp_millis())?;
        let day_ms = 24 * 3600 * 1000_i64;
        match self {
            TimeRange::Today => Some(today_start),
            TimeRange::Week => Some(today_start - 7 * day_ms),
            TimeRange::Month => Some(today_start - 30 * day_ms),
            TimeRange::All => None,
        }
    }
}

/// 毫秒时间戳 → "YYYY-MM-DD" 日期字符串（本地时区）
pub(crate) fn ms_to_date(ms: i64) -> String {
    chrono::DateTime::from_timestamp(ms / 1000, ((ms % 1000).max(0) * 1_000_000) as u32)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

/// 毫秒时间戳 → "YYYY-MM-DD HH:MM" 格式（本地时区）
pub(crate) fn ms_to_datetime(ms: i64) -> String {
    chrono::DateTime::from_timestamp(ms / 1000, ((ms % 1000).max(0) * 1_000_000) as u32)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "1970-01-01 00:00".to_string())
}

/// "YYYY-MM-DD" → 本地当天 0 点的毫秒时间戳
pub(crate) fn date_to_ms(date: &str) -> Option<i64> {
    let naive = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))?;
    let local = chrono::Local.from_local_datetime(&naive).single()?;
    Some(local.timestamp_millis())
}
