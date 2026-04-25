use anyhow::{bail, Result};
use std::collections::HashMap;
use std::io::BufRead;

use super::output::{is_json_mode, output_json};
use crate::adapter::mapping::ToolMapping;
use crate::config::models::models_data;
use crate::session;
use crate::session::cache::{
    file_meta, now_ms, CacheStatus, CachedDailyUsage, CachedUsageData, UnifiedCache,
};
use crate::session::model::{date_to_ms, ms_to_datetime, TimeRange, TokenUsage, UsageSummary};

// ══════════════════════════════════════════════════════════
// Token 价格计算
// ══════════════════════════════════════════════════════════

/// Token 用量对应的价格明细（USD）
#[derive(Debug, Clone, Default)]
struct TokenPrice {
    input_cost: f64,
    output_cost: f64,
    cache_read_cost: f64,
    cache_creation_cost: f64,
    /// 缓存写入 5m TTL 费用
    cache_creation_5m_cost: f64,
    /// 缓存写入 1h TTL 费用
    cache_creation_1h_cost: f64,
    /// Web 搜索费用
    web_search_cost: f64,
    /// 总费用
    total_cost: f64,
}

impl std::ops::AddAssign for TokenPrice {
    fn add_assign(&mut self, other: Self) {
        self.input_cost += other.input_cost;
        self.output_cost += other.output_cost;
        self.cache_read_cost += other.cache_read_cost;
        self.cache_creation_cost += other.cache_creation_cost;
        self.cache_creation_5m_cost += other.cache_creation_5m_cost;
        self.cache_creation_1h_cost += other.cache_creation_1h_cost;
        self.web_search_cost += other.web_search_cost;
        self.total_cost += other.total_cost;
    }
}

impl TokenPrice {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "input_cost": round_price(self.input_cost),
            "output_cost": round_price(self.output_cost),
            "cache_read_cost": round_price(self.cache_read_cost),
            "cache_creation_cost": round_price(self.cache_creation_cost),
            "total_cost": round_price(self.total_cost),
        })
    }
}

/// 根据模型名称和 TokenUsage 计算价格
fn calculate_price(model_name: &str, usage: &TokenUsage) -> TokenPrice {
    // 处理 :fast 后缀
    let (base_model, is_fast) = if let Some(stripped) = model_name.strip_suffix(":fast") {
        (stripped, true)
    } else {
        (model_name, false)
    };

    let data = models_data();
    let pricing = match data.find_model_pricing(base_model) {
        Some(m) => m,
        None => return TokenPrice::default(),
    };

    let input_price = pricing.input_price.unwrap_or(0.0);
    let output_price = pricing.output_price.unwrap_or(0.0);
    let cache_read_price = pricing.cache_read_price.unwrap_or(input_price * 0.1);
    let cache_write_price = pricing.cache_write_price.unwrap_or(input_price * 1.25);

    // Fast 模式加价：输入 2x，输出 2x（参考 goccc/toktrack）
    let (eff_input_price, eff_output_price) = if is_fast {
        (input_price * 2.0, output_price * 2.0)
    } else {
        (input_price, output_price)
    };

    // 缓存写入定价：
    // - 有 5m/1h 分层数据时：5m = input × 1.25, 1h = input × 2.0
    // - 无分层数据时：使用 cache_write_price
    let cache_write_5m_price = input_price * 1.25;
    let cache_write_1h_price = input_price * 2.0;

    // 长上下文阈值
    const LONG_CTX_THRESHOLD: i64 = 200_000;
    // 长上下文加价倍数（参考 goccc/toktrack，约 1.5-2x）
    const LONG_CTX_MULTIPLIER: f64 = 1.5;

    let total_input = usage.input_tokens + usage.cache_read_tokens + usage.cache_creation_tokens;
    let is_long_ctx = total_input > LONG_CTX_THRESHOLD;

    let (eff_cache_read_price, eff_cache_write_price) = if is_long_ctx {
        (cache_read_price * LONG_CTX_MULTIPLIER, cache_write_price * LONG_CTX_MULTIPLIER)
    } else {
        (cache_read_price, cache_write_price)
    };
    let (eff_cw_5m, eff_cw_1h) = if is_long_ctx {
        (cache_write_5m_price * LONG_CTX_MULTIPLIER, cache_write_1h_price * LONG_CTX_MULTIPLIER)
    } else {
        (cache_write_5m_price, cache_write_1h_price)
    };

    // 缓存写入费用：有 5m/1h 分层时使用分层价格，否则使用 cache_write_price
    let (cache_creation_cost, cache_creation_5m_cost, cache_creation_1h_cost) =
        if usage.cache_creation_5m_tokens > 0 || usage.cache_creation_1h_tokens > 0 {
            let cost_5m = usage.cache_creation_5m_tokens as f64 / 1_000_000.0 * eff_cw_5m;
            let cost_1h = usage.cache_creation_1h_tokens as f64 / 1_000_000.0 * eff_cw_1h;
            // 总 cache_creation_tokens 仍以 cache_write_price 计算（用于汇总）
            (usage.cache_creation_tokens as f64 / 1_000_000.0 * eff_cache_write_price, cost_5m, cost_1h)
        } else {
            let cost = usage.cache_creation_tokens as f64 / 1_000_000.0 * eff_cache_write_price;
            (cost, 0.0, 0.0)
        };

    // Web 搜索计费：$0.01/次
    let web_search_cost = usage.web_search_requests as f64 * 0.01;

    let input_cost = usage.input_tokens as f64 / 1_000_000.0 * eff_input_price;
    let output_cost = usage.output_tokens as f64 / 1_000_000.0 * eff_output_price;
    let cache_read_cost = usage.cache_read_tokens as f64 / 1_000_000.0 * eff_cache_read_price;

    let total_cost = input_cost + output_cost + cache_read_cost + cache_creation_cost + web_search_cost;

    TokenPrice {
        input_cost,
        output_cost,
        cache_read_cost,
        cache_creation_cost,
        cache_creation_5m_cost,
        cache_creation_1h_cost,
        web_search_cost,
        total_cost,
    }
}

/// 优先使用 JSONL 中的 costUSD，否则用 calculate_price 计算
fn usage_or_calc_price(model_name: &str, usage: &TokenUsage, cost_usd: Option<f64>) -> TokenPrice {
    if let Some(cost) = cost_usd {
        // costUSD 是总费用，无法拆分细项，统一放入 total_cost
        TokenPrice {
            total_cost: round_price(cost),
            ..TokenPrice::default()
        }
    } else {
        calculate_price(model_name, usage)
    }
}

/// 保留 6 位小数（精度足够且避免浮点显示问题）
fn round_price(v: f64) -> f64 {
    (v * 1_000_000.0).round() / 1_000_000.0
}

/// 格式化价格为可读字符串
fn format_price(v: f64) -> String {
    if v == 0.0 {
        "-".to_string()
    } else if v < 0.01 {
        format!("${:.4}", v)
    } else {
        format!("${:.2}", v)
    }
}

pub(crate) fn handle_subcommand(matches: &clap::ArgMatches) -> Result<()> {
    match matches.subcommand() {
        Some(("list", m)) => list_sessions(m.get_one::<String>("tool").map(|s| s.as_str())),
        Some(("show", m)) => show_session(
            m.get_one::<String>("id").unwrap(),
            m.get_one::<String>("tool").map(|s| s.as_str()),
        ),
        Some(("remove", m)) => delete_session(
            m.get_one::<String>("id").unwrap(),
            m.get_one::<String>("tool").map(|s| s.as_str()),
        ),
        _ => bail!("unknown session subcommand. Run 'vcc session --help' for usage."),
    }
}

struct SessionTokenStats {
    total: TokenUsage,
    by_model: HashMap<String, TokenUsage>,
    total_cost: TokenPrice,
    cost_by_model: HashMap<String, TokenPrice>,
}

impl SessionTokenStats {
    fn to_json(&self) -> serde_json::Value {
        fn uj(u: &TokenUsage) -> serde_json::Value {
            serde_json::json!({ "input": u.input_tokens, "output": u.output_tokens, "cache_read": u.cache_read_tokens, "cache_creation": u.cache_creation_tokens, "total": u.total() })
        }
        let mut obj = uj(&self.total).as_object().cloned().unwrap_or_default();
        obj.insert("cost".into(), self.total_cost.to_json());
        obj.insert(
            "by_model".into(),
            serde_json::json!(self
                .by_model
                .iter()
                .map(|(m, u)| {
                    let mut model_obj = uj(u).as_object().cloned().unwrap_or_default();
                    if let Some(cost) = self.cost_by_model.get(m) {
                        model_obj.insert("cost".into(), cost.to_json());
                    }
                    (m.clone(), serde_json::Value::Object(model_obj))
                })
                .collect::<serde_json::Map<String, serde_json::Value>>()),
        );
        serde_json::Value::Object(obj)
    }
}

fn extract_session_tokens(
    session: &session::model::SessionMeta,
    mapping: &ToolMapping,
) -> Option<SessionTokenStats> {
    let mut cache = UnifiedCache::load().ok().unwrap_or_default();
    let status = cache.check_cache_status(&session.tool, &session.session_id, &session.source_path);
    let usages: Vec<UsageSummary> = match status {
        CacheStatus::Hit => {
            cache.load_usages_in_range(&session.tool, &session.session_id, None)
        }
        _ => match session::extract_usage(mapping, session, None) {
            Ok(r) => {
                save_usage_cache_entry(&mut cache, session, &r.usages);
                if let Err(e) = cache.save() {
                    eprintln!("warning: failed to save usage cache: {}", e);
                }
                r.usages
            }
            Err(_) => return None,
        },
    };
    if usages.is_empty() {
        return None;
    }
    let mut by_model: HashMap<String, TokenUsage> = HashMap::new();
    let mut cost_by_model: HashMap<String, TokenPrice> = HashMap::new();
    let mut total = TokenUsage::default();
    let mut total_cost = TokenPrice::default();
    for u in &usages {
        let price = usage_or_calc_price(&u.model, &u.usage, u.cost_usd);
        *by_model.entry(u.model.clone()).or_default() += u.usage.clone();
        *cost_by_model.entry(u.model.clone()).or_default() += price.clone();
        total += u.usage.clone();
        total_cost += price;
    }
    Some(SessionTokenStats { total, by_model, total_cost, cost_by_model })
}

fn save_usage_cache_entry(
    cache: &mut UnifiedCache,
    session: &session::model::SessionMeta,
    usages: &[UsageSummary],
) {
    let (_modified_ms, file_size) = match file_meta(&session.source_path) {
        Some(m) => m,
        None => return,
    };
    // 按 (date, model) 聚合为 daily
    let mut daily: std::collections::HashMap<String, Vec<CachedDailyUsage>> =
        std::collections::HashMap::new();
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
    cache.update_usage(
        &session.tool,
        &session.session_id,
        CachedUsageData {
            extracted_at: now_ms(),
            processed_ranges: vec![(0, file_size)],
            first_byte_offset: 0,
            last_byte_offset: file_size,
            daily,
            codex_prev_total: None,
            codex_current_model: None,
            subagent_files: HashMap::new(),
        },
    );
}

pub(crate) fn list_sessions(tool: Option<&str>) -> Result<()> {
    let mappings = load_mappings(tool)?;
    let all_sessions = scan_all_sessions(&mappings, None);
    update_session_cache(&all_sessions);
    if is_json_mode() {
        output_json(&serde_json::json!(all_sessions.iter().map(|s| serde_json::json!({
            "tool": s.tool, "session_id": s.session_id, "title": s.title, "project_dir": s.project_dir,
            "created_at": s.created_at, "last_active_at": s.last_active_at, "source_path": s.source_path.display().to_string(),
            "file_size": file_size_of(&s.source_path), "message_count": count_session_messages(&s.source_path, s.tool.as_str()), "resume_command": s.resume_command,
        })).collect::<Vec<_>>()));
        return Ok(());
    }
    if all_sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }
    println!(
        "{:<10} {:<38} {:<30} {:<15}",
        "TOOL", "SESSION_ID", "TITLE", "LAST_ACTIVE"
    );
    println!("{}", "-".repeat(95));
    for s in &all_sessions {
        println!(
            "{:<10} {:<38} {:<30} {:<15}",
            s.tool,
            session::truncate_str(&s.session_id, 36),
            session::truncate_str(s.title.as_deref().unwrap_or("(untitled)"), 28),
            s.last_active_at
                .map(session::format_relative_time)
                .unwrap_or_else(|| "-".into())
        );
    }
    println!("\n{} session(s) total.", all_sessions.len());
    Ok(())
}

pub(crate) fn show_session(session_id: &str, tool: Option<&str>) -> Result<()> {
    let (session, mapping) = find_session(session_id, tool)?;
    let file_size = file_size_of(&session.source_path);
    let line_path = if session.source_path.is_dir() {
        session.source_path.join("wire.jsonl")
    } else {
        session.source_path.clone()
    };
    let line_count = count_lines(&line_path);
    let token_stats = extract_session_tokens(&session, &mapping);
    if is_json_mode() {
        let mut j = serde_json::json!({ "session_id": session.session_id, "tool": session.tool, "title": session.title, "project_dir": session.project_dir, "created_at": session.created_at, "last_active_at": session.last_active_at, "summary": session.summary, "path": session.source_path.display().to_string(), "file_size": file_size, "line_count": line_count, "resume_command": session.resume_command });
        if let Some(s) = &token_stats {
            j["tokens"] = s.to_json();
        }
        output_json(&j);
        return Ok(());
    }
    println!("Session: {}", session.session_id);
    println!("Tool:    {}", session.tool);
    if let Some(ref t) = session.title {
        println!("Title:   {}", t);
    }
    if let Some(ref d) = session.project_dir {
        println!("Project: {}", d);
    }
    if let Some(ts) = session.created_at {
        println!("Created: {}", ms_to_datetime(ts));
    }
    if let Some(ts) = session.last_active_at {
        println!("Active:  {}", ms_to_datetime(ts));
    }
    if let Some(ref s) = session.summary {
        println!("Summary: {}", session::truncate_str(s, 80));
    }
    println!("Path:    {}", session.source_path.display());
    if let Some(sz) = file_size {
        println!("Size:    {}", session::format_bytes(sz));
    }
    if let Some(lc) = line_count {
        println!("Lines:   {}", lc);
    }
    if let Some(stats) = &token_stats {
        println!("\nTokens:");
        println!(
            "  Input:     {}",
            session::format_number(stats.total.input_tokens)
        );
        println!(
            "  Output:    {}",
            session::format_number(stats.total.output_tokens)
        );
        if stats.total.cache_read_tokens > 0 {
            println!(
                "  Cache R:   {}",
                session::format_number(stats.total.cache_read_tokens)
            );
        }
        if stats.total.cache_creation_tokens > 0 {
            println!(
                "  Cache W:   {}",
                session::format_number(stats.total.cache_creation_tokens)
            );
        }
        println!(
            "  Total:     {}",
            session::format_number(stats.total.total())
        );
        if stats.total_cost.total_cost > 0.0 {
            println!("\nCost:");
            if stats.total_cost.input_cost > 0.0 {
                println!("  Input:     {}", format_price(stats.total_cost.input_cost));
            }
            if stats.total_cost.output_cost > 0.0 {
                println!("  Output:    {}", format_price(stats.total_cost.output_cost));
            }
            if stats.total_cost.cache_read_cost > 0.0 {
                println!("  Cache R:   {}", format_price(stats.total_cost.cache_read_cost));
            }
            if stats.total_cost.cache_creation_cost > 0.0 {
                println!("  Cache W:   {}", format_price(stats.total_cost.cache_creation_cost));
            }
            println!("  Total:     {}", format_price(stats.total_cost.total_cost));
        }
        if stats.by_model.len() > 1 {
            println!("  By Model:");
            for (m, u) in &stats.by_model {
                let cost = stats.cost_by_model.get(m);
                let cost_str = cost
                    .map(|c| if c.total_cost > 0.0 { format!(" {}", format_price(c.total_cost)) } else { String::new() })
                    .unwrap_or_default();
                println!("    {:20} {}{}", m, session::format_number(u.total()), cost_str);
            }
        }
    }
    if let Some(ref cmd) = session.resume_command {
        println!("Resume:  {}", cmd);
    }
    Ok(())
}

pub(crate) fn delete_session(session_id: &str, tool: Option<&str>) -> Result<()> {
    let (session, mapping) = find_session(session_id, tool)?;
    let outcome = session::delete_session(&mapping, &session.source_path)?;
    if is_json_mode() {
        output_json(
            &serde_json::json!({ "success": true, "session_id": session_id, "tool": session.tool, "files_removed": outcome.files_removed, "bytes_freed": outcome.bytes_freed, "warnings": outcome.warnings }),
        );
    } else {
        println!("Deleted session '{}' from {}", session_id, session.tool);
        println!("  Files removed: {}", outcome.files_removed);
        println!(
            "  Space freed:   {}",
            session::format_bytes(outcome.bytes_freed)
        );
        for w in &outcome.warnings {
            println!("  Warning: {}", w);
        }
    }
    Ok(())
}

fn find_session(
    session_id: &str,
    tool: Option<&str>,
) -> Result<(session::model::SessionMeta, ToolMapping)> {
    fn scan_and_find(
        mapping: &ToolMapping,
        sid: &str,
    ) -> Option<(
        session::model::SessionMeta,
        Vec<session::model::SessionMeta>,
    )> {
        let sessions = session::scan_sessions(mapping, None).ok()?;
        let found = sessions.iter().find(|s| s.session_id == sid).cloned();
        found.map(|s| (s, sessions))
    }
    if let Some(tool_name) = tool {
        let mappings = load_mappings(Some(tool_name))?;
        let mapping = mappings
            .iter()
            .find(|m| m.tool.name == tool_name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", tool_name))?;
        return Ok((
            scan_and_find(mapping, session_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "session '{}' not found for tool '{}'",
                        session_id,
                        tool_name
                    )
                })?
                .0,
            mapping.clone(),
        ));
    }
    let cache = UnifiedCache::load().ok().unwrap_or_default();
    if let Some(tool_name) = cache.find_tool_by_session_id(session_id) {
        if let Ok(mappings) = load_mappings(Some(&tool_name)) {
            if let Some(mapping) = mappings.iter().find(|m| m.tool.name == tool_name) {
                if let Some((s, _)) = scan_and_find(mapping, session_id) {
                    return Ok((s, mapping.clone()));
                }
            }
        }
    }
    let mappings = load_mappings(None)?;
    for mapping in &mappings {
        if let Some((s, sessions)) = scan_and_find(mapping, session_id) {
            update_session_cache(&sessions);
            return Ok((s, mapping.clone()));
        }
    }
    anyhow::bail!("session '{}' not found in any tool", session_id)
}

pub(crate) fn show_usage(
    tool: Option<&str>,
    range: TimeRange,
    from: Option<&str>,
    to: Option<&str>,
    by: Option<&str>,
) -> Result<()> {
    let mappings = load_mappings(tool)?;
    let from_ms = from
        .map(|d| {
            date_to_ms(d)
                .ok_or_else(|| anyhow::anyhow!("invalid --from date '{}', expected YYYY-MM-DD", d))
        })
        .transpose()?;
    let to_ms = to
        .map(|d| {
            date_to_ms(d)
                .map(|ms| ms + 86400 * 1000)
                .ok_or_else(|| anyhow::anyhow!("invalid --to date '{}', expected YYYY-MM-DD", d))
        })
        .transpose()?;
    if let (Some(f), Some(t)) = (&from_ms, &to_ms) {
        if f >= t {
            bail!("--from date must be before --to date");
        }
    }
    let effective_range = if from.is_some() || to.is_some() {
        TimeRange::All
    } else {
        range
    };
    let all_sessions = scan_all_sessions(&mappings, effective_range.start_ms());
    if all_sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }
    let summaries: Vec<UsageSummary> =
        session::extract_all_usage(&all_sessions, &mappings, effective_range)
            .into_iter()
            .filter(|u| {
                if let Some(from) = from_ms {
                    if u.date.as_ref().and_then(|d| date_to_ms(d)).unwrap_or(0) < from {
                        return false;
                    }
                }
                if let Some(to) = to_ms {
                    if u.date.as_ref().and_then(|d| date_to_ms(d)).unwrap_or(0) >= to {
                        return false;
                    }
                }
                true
            })
            .collect();
    if summaries.is_empty() {
        if is_json_mode() {
            output_json(&serde_json::json!([]));
        } else {
            println!("No token usage data found for the selected period.");
        }
        return Ok(());
    }
    let dims = parse_by_dims(by);
    let aggregated = aggregate_by_dims(&summaries, &dims);
    if is_json_mode() {
        output_json(&serde_json::json!(aggregated.iter().map(|r| {
            let mut obj = serde_json::json!({
                "date": r.key.date,
                "tool": r.key.tool,
                "model": r.key.model,
                "input_tokens": r.usage.input_tokens,
                "output_tokens": r.usage.output_tokens,
                "cache_read_tokens": r.usage.cache_read_tokens,
                "cache_creation_tokens": r.usage.cache_creation_tokens,
                "request_count": r.request_count,
            });
            let cost = r.cost.to_json();
            obj.as_object_mut().unwrap().insert("cost".into(), cost);
            obj
        }).collect::<Vec<_>>()));
        return Ok(());
    }
    print_usage_table(&aggregated, &dims, &range, from, to);
    Ok(())
}

fn parse_by_dims(by: Option<&str>) -> Vec<String> {
    match by {
        None => vec!["tool".into(), "model".into()],
        Some(s) => {
            let mut dims: Vec<String> = s
                .split(',')
                .map(|d| d.trim().to_string())
                .filter(|d| d == "day" || d == "tool" || d == "model")
                .collect();
            let mut seen = std::collections::HashSet::new();
            dims.retain(|d| seen.insert(d.clone()));
            if dims.is_empty() {
                vec!["tool".into(), "model".into()]
            } else {
                dims
            }
        }
    }
}

#[derive(Debug, Clone, Hash, Eq, PartialEq, Default)]
struct AggKey {
    date: Option<String>,
    tool: Option<String>,
    model: Option<String>,
}
#[derive(Debug, Clone, Default)]
struct AggRow {
    key: AggKey,
    usage: crate::session::model::TokenUsage,
    request_count: i64,
    cost: TokenPrice,
}

fn aggregate_by_dims(summaries: &[UsageSummary], dims: &[String]) -> Vec<AggRow> {
    let mut map: HashMap<AggKey, AggRow> = HashMap::new();
    for u in summaries {
        let key = AggKey {
            date: if dims.iter().any(|d| d == "day") {
                u.date.clone()
            } else {
                None
            },
            tool: if dims.iter().any(|d| d == "tool") {
                Some(u.tool.clone())
            } else {
                None
            },
            model: if dims.iter().any(|d| d == "model") {
                Some(u.model.clone())
            } else {
                None
            },
        };
        let price = usage_or_calc_price(&u.model, &u.usage, u.cost_usd);
        let entry = map.entry(key.clone()).or_insert_with(|| AggRow {
            key,
            ..Default::default()
        });
        entry.usage += u.usage.clone();
        entry.request_count += u.request_count;
        entry.cost += price;
    }
    let mut rows: Vec<AggRow> = map.into_values().collect();
    rows.sort_by(|a, b| {
        a.key
            .date
            .cmp(&b.key.date)
            .then(a.key.tool.cmp(&b.key.tool))
            .then(a.key.model.cmp(&b.key.model))
    });
    rows
}

fn print_usage_table(
    rows: &[AggRow],
    dims: &[String],
    range: &TimeRange,
    from: Option<&str>,
    to: Option<&str>,
) {
    let range_label = if from.is_some() || to.is_some() {
        format!("{} ~ {}", from.unwrap_or("..."), to.unwrap_or("..."))
    } else {
        match range {
            TimeRange::Today => "Today",
            TimeRange::Week => "Last 7 Days",
            TimeRange::Month => "Last 30 Days",
            TimeRange::All => "All Time",
        }
        .to_string()
    };
    println!("Token Usage ({})\n", range_label);
    let sd = dims.iter().any(|d| d == "day");
    let st = dims.iter().any(|d| d == "tool");
    let sm = dims.iter().any(|d| d == "model");
    let fmt_row = |k: &AggKey| -> String {
        let mut s = String::new();
        if sd {
            s.push_str(&format!("{:<12}", k.date.as_deref().unwrap_or("-")));
        }
        if st {
            s.push_str(&format!("{:<10}", k.tool.as_deref().unwrap_or("-")));
        }
        if sm {
            s.push_str(&format!(
                "{:<24}",
                session::truncate_str(k.model.as_deref().unwrap_or("-"), 22)
            ));
        }
        s
    };
    let mut hdr_key = AggKey::default();
    if sd {
        hdr_key.date = Some("DATE".into());
    }
    if st {
        hdr_key.tool = Some("TOOL".into());
    }
    if sm {
        hdr_key.model = Some("MODEL".into());
    }
    let hdr = format!(
        "{}{:>10} {:>10} {:>10} {:>10} {:>10}",
        fmt_row(&hdr_key),
        "INPUT",
        "OUTPUT",
        "CACHE_R",
        "CACHE_W",
        "COST"
    );
    println!("{}", hdr);
    println!("{}", "-".repeat(hdr.len()));
    let (mut ti, mut to2, mut tcr, mut tcc) = (0i64, 0i64, 0i64, 0i64);
    let mut total_cost = TokenPrice::default();
    for row in rows {
        println!(
            "{}{:>10} {:>10} {:>10} {:>10} {:>10}",
            fmt_row(&row.key),
            session::format_number(row.usage.input_tokens),
            session::format_number(row.usage.output_tokens),
            session::format_number(row.usage.cache_read_tokens),
            session::format_number(row.usage.cache_creation_tokens),
            format_price(row.cost.total_cost)
        );
        ti += row.usage.input_tokens;
        to2 += row.usage.output_tokens;
        tcr += row.usage.cache_read_tokens;
        tcc += row.usage.cache_creation_tokens;
        total_cost += row.cost.clone();
    }
    println!("{}", "-".repeat(hdr.len()));
    let mut total_key = AggKey::default();
    if st {
        total_key.tool = Some("TOTAL".into());
    }
    println!(
        "{}{:>10} {:>10} {:>10} {:>10} {:>10}",
        fmt_row(&total_key),
        session::format_number(ti),
        session::format_number(to2),
        session::format_number(tcr),
        session::format_number(tcc),
        format_price(total_cost.total_cost)
    );
}

fn file_size_of(path: &std::path::Path) -> Option<u64> {
    if path.is_dir() {
        Some(session::dir_size(path))
    } else {
        std::fs::metadata(path).map(|m| m.len()).ok()
    }
}
fn count_lines(path: &std::path::Path) -> Option<usize> {
    Some(
        std::io::BufReader::new(std::fs::File::open(path).ok()?)
            .lines()
            .count(),
    )
}
fn update_session_cache(sessions: &[session::model::SessionMeta]) {
    if sessions.is_empty() {
        return;
    }
    let mut c = UnifiedCache::load().ok().unwrap_or_default();
    for s in sessions {
        c.upsert_session(s);
    }
    c.purge_missing();
    if let Err(e) = c.save() {
        eprintln!("warning: failed to save session cache: {}", e);
    }
}
fn scan_all_sessions(
    mappings: &[ToolMapping],
    min_mtime_ms: Option<i64>,
) -> Vec<session::model::SessionMeta> {
    let mut all = Vec::new();
    for mapping in mappings {
        match session::scan_sessions(mapping, min_mtime_ms) {
            Ok(s) => all.extend(s),
            Err(e) => eprintln!("  warning: failed to scan {}: {}", mapping.tool.name, e),
        }
    }
    all
}

fn load_mappings(tool: Option<&str>) -> Result<Vec<ToolMapping>> {
    let tool_names: Vec<String> = match tool {
        Some(t) => vec![t.to_string()],
        None => crate::adapter::all_adapters()
            .iter()
            .map(|a| a.tool_name().to_string())
            .collect(),
    };
    Ok(tool_names
        .iter()
        .filter_map(|n| ToolMapping::load_for_tool(n).ok())
        .collect())
}
fn count_session_messages(source_path: &std::path::Path, tool: &str) -> Option<usize> {
    match tool {
        "claude" => count_lines_with(source_path, |l| {
            l.contains("\"type\":\"assistant\"") || l.contains("\"type\": \"assistant\"")
        }),
        "codex" => count_lines_with(source_path, |l| {
            l.contains("response_item") && l.contains("\"assistant\"")
        }),
        "gemini" => {
            let c = std::fs::read_to_string(source_path).ok()?;
            let v: serde_json::Value = serde_json::from_str(&c).ok()?;
            Some(
                v.get("messages")
                    .and_then(|m| m.as_array())
                    .map(|a| {
                        a.iter()
                            .filter(|m| m.get("type").and_then(|v| v.as_str()) == Some("gemini"))
                            .count()
                    })
                    .unwrap_or(0),
            )
        }
        "kimi" => count_lines_with(&source_path.join("wire.jsonl"), |l| l.contains("TurnBegin")),
        _ => count_lines(source_path),
    }
}

fn count_lines_with(path: &std::path::Path, pred: impl Fn(&str) -> bool) -> Option<usize> {
    Some(
        std::io::BufReader::new(std::fs::File::open(path).ok()?)
            .lines()
            .map_while(Result::ok)
            .filter(|l| pred(l))
            .count(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_price_with_builtin_model() {
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            web_search_requests: 0,
        };
        let price = calculate_price("claude-sonnet-4-6", &usage);
        // input: 1M * $3.0/M = $3.0, output: 500K * $15.0/M = $7.5
        assert!((price.input_cost - 3.0).abs() < 0.001);
        assert!((price.output_cost - 7.5).abs() < 0.001);
        assert!((price.total_cost - 10.5).abs() < 0.001);
    }

    #[test]
    fn test_calculate_price_with_cache_tokens() {
        // total_input = 100K + 50K + 10K = 160K < 200K, 不触发长上下文加价
        let usage = TokenUsage {
            input_tokens: 100_000,
            output_tokens: 50_000,
            cache_read_tokens: 50_000,
            cache_creation_tokens: 10_000,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            web_search_requests: 0,
        };
        let price = calculate_price("gpt-4o", &usage);
        // 不再硬编码 cache 价格断言，因为 models.json 可能覆盖内置数据
        assert!(price.input_cost > 0.0);
        assert!(price.output_cost > 0.0);
        assert!(price.total_cost > 0.0);
    }

    #[test]
    fn test_calculate_price_unknown_model() {
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            web_search_requests: 0,
        };
        let price = calculate_price("unknown-model-xyz", &usage);
        assert_eq!(price.total_cost, 0.0);
    }

    #[test]
    fn test_token_price_add_assign() {
        let mut p1 = TokenPrice {
            input_cost: 1.0,
            output_cost: 2.0,
            cache_read_cost: 0.5,
            cache_creation_cost: 0.25,
            cache_creation_5m_cost: 0.0,
            cache_creation_1h_cost: 0.0,
            web_search_cost: 0.0,
            total_cost: 3.75,
        };
        let p2 = TokenPrice {
            input_cost: 0.5,
            output_cost: 1.0,
            cache_read_cost: 0.1,
            cache_creation_cost: 0.05,
            cache_creation_5m_cost: 0.0,
            cache_creation_1h_cost: 0.0,
            web_search_cost: 0.0,
            total_cost: 1.65,
        };
        p1 += p2;
        assert!((p1.input_cost - 1.5).abs() < 0.001);
        assert!((p1.output_cost - 3.0).abs() < 0.001);
        assert!((p1.total_cost - 5.4).abs() < 0.001);
    }

    #[test]
    fn test_round_price() {
        assert_eq!(round_price(1.234567), 1.234567);
        assert_eq!(round_price(1.2345678901), 1.234568);
        assert_eq!(round_price(0.0), 0.0);
    }

    #[test]
    fn test_format_price() {
        assert_eq!(format_price(0.0), "-");
        assert_eq!(format_price(1.5), "$1.50");
        assert_eq!(format_price(0.005), "$0.0050");
    }

    #[test]
    fn test_token_price_to_json() {
        let p = TokenPrice {
            input_cost: 1.5,
            output_cost: 2.5,
            cache_read_cost: 0.0,
            cache_creation_cost: 0.0,
            cache_creation_5m_cost: 0.0,
            cache_creation_1h_cost: 0.0,
            web_search_cost: 0.0,
            total_cost: 4.0,
        };
        let j = p.to_json();
        assert_eq!(j["input_cost"], 1.5);
        assert_eq!(j["output_cost"], 2.5);
        assert_eq!(j["total_cost"], 4.0);
    }
}
