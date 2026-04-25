use anyhow::Result;

use super::output::{is_json_mode, output_json};
use crate::config;

/// 查找 provider preset（优先 models.json，fallback 内置）
pub(crate) fn find_provider_preset(name: &str) -> Option<ProviderPreset> {
    let data = config::models::models_data();
    data.find_provider(name).map(|p| ProviderPreset {
        name: p.id.clone(),
        provider_type: p.provider_type.clone(),
        base_url: p.api.clone(),
        default_model: p.models.first().map(|m| m.id.clone()),
        description: Some(p.name.clone()),
    })
}

/// 兼容旧接口的 ProviderPreset 结构
#[allow(dead_code)]
pub(crate) struct ProviderPreset {
    pub name: String,
    pub provider_type: String,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub description: Option<String>,
}

/// 查找 MCP preset
pub(crate) fn find_mcp_preset(name: &str) -> Option<&'static config::McpPresetConfig> {
    config::presets().find_mcp(name)
}

/// `vcc preset provider list` — 列出所有平台和模型
pub(crate) fn list_providers() -> Result<()> {
    let data = config::models::models_data();

    if is_json_mode() {
        let providers: Vec<serde_json::Value> = data
            .providers
            .values()
            .map(|p| {
                let models: Vec<serde_json::Value> = p.models.iter().map(model_to_json).collect();
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "type": p.provider_type,
                    "api": p.api,
                    "models": models,
                    "original": data.is_original_provider(&p.id),
                })
            })
            .collect();
        output_json(&serde_json::json!({ "providers": providers }));
        return Ok(());
    }

    let has_file = config::models::models_json_path()
        .map(|p| p.exists())
        .unwrap_or(false);
    let cache_age = config::models::models_cache_age();

    let mut providers: Vec<&config::models::ProviderInfo> = data.providers.values().collect();
    providers.sort_by(|a, b| a.id.cmp(&b.id));

    let mut providers: Vec<&config::models::ProviderInfo> = data.providers.values().collect();
    providers.sort_by(|a, b| a.id.cmp(&b.id));

    // ── 平台表 ──
    println!("Provider 平台 ({} 个):", providers.len());
    println!("{:<20} {:<15} {:<6} 描述", "ID", "TYPE", "模型数");
    println!("{}", "-".repeat(65));
    for p in &providers {
        println!("{:<20} {:<15} {:<6} {}", p.id, p.provider_type, p.models.len(), p.name);
    }
    println!();

    // ── 模型表（仅原厂模型，按 id 去重）──
    // 策略：
    // 1. 只收集原厂（is_original_provider）的模型
    // 2. 去掉开头的已知标签前缀（如 "hf:"、"Pro:"、"@cf/"）
    // 3. 去掉 "/" 前的路径（如 "zai-org/"、"MiniMax/"），取最后一段
    // 4. 保留 ":" 后的有意义后缀（如 ":exacto"、":thinking"、":free"）
    // 展示时也用处理后的 id，优先保留 ID 更短（更干净）的版本
    fn model_base_id(id: &str) -> String {
        let s = id.to_lowercase();
        // 去掉开头的已知标签前缀
        let after_prefix = s
            .strip_prefix("hf:")
            .or_else(|| s.strip_prefix("pro:"))
            .or_else(|| s.strip_prefix("@cf/"))
            .or_else(|| s.strip_prefix("workers-ai/@cf/"))
            .unwrap_or(&s);
        // 去掉 / 前的路径（如 "zai-org/"、"MiniMax/"），取最后一段
        after_prefix.rsplit('/').next().unwrap_or(after_prefix).to_string()
    }

    let mut best_by_base: std::collections::HashMap<String, &config::models::ModelInfo> =
        std::collections::HashMap::new();
    let mut reseller_model_count = 0usize;
    let mut short_ctx_model_count = 0usize;
    for p in &providers {
        let is_original = data.is_original_provider(&p.id);
        for m in &p.models {
            if !is_original {
                reseller_model_count += 1;
                continue;
            }
            // 过滤上下文窗口小于 128K 的模型
            if m.context_limit.map_or(true, |c| c < 128_000) {
                short_ctx_model_count += 1;
                continue;
            }
            let key = model_base_id(&m.id);
            let should_insert = match best_by_base.get(&key) {
                None => true,
                Some(existing) => {
                    // 优先保留 ID 更短的（不带厂商前缀的更干净）
                    m.id.len() < existing.id.len()
                }
            };
            if should_insert {
                best_by_base.insert(key, m);
            }
        }
    }
    let mut unique_models: Vec<(String, &config::models::ModelInfo)> = best_by_base
        .into_iter()
        .collect();
    unique_models.sort_by(|a, b| a.0.cmp(&b.0));

    println!("原厂模型 (≥128K 上下文, 去重后 {} 个):", unique_models.len());
    println!("{:<32} {:<6} {:<6} {:<8} {:<14}", "MODEL ID", "推理", "工具", "上下文", "价格");
    println!("{}", "-".repeat(70));
    for (base_id, m) in &unique_models {
        let reasoning_str = if m.reasoning { "✓" } else { "" };
        let tool_str = if m.tool_call { "✓" } else { "" };
        let ctx_str = m.context_limit
            .map(|c| format!("{}k", c / 1000))
            .unwrap_or_default();
        let price_str = match (m.input_price, m.output_price) {
            (Some(i), Some(o)) => format!("${:.2}/${:.2}", i, o),
            _ => String::new(),
        };
        println!("{:<32} {:<6} {:<6} {:<8} {:<14}",
            crate::session::truncate_str(base_id, 30),
            reasoning_str,
            tool_str,
            ctx_str,
            price_str,
        );
    }
    println!();

    if has_file {
        let age_str = cache_age.as_deref().unwrap_or("未知");
        println!("  数据来源: models.dev (缓存于 {})", age_str);
    } else {
        println!("  数据来源: 内置预设（运行 'vcc preset provider update' 获取更多平台）");
    }
    if reseller_model_count > 0 || short_ctx_model_count > 0 {
        let mut parts = Vec::new();
        if reseller_model_count > 0 {
            parts.push(format!("{} 个转售平台模型", reseller_model_count));
        }
        if short_ctx_model_count > 0 {
            parts.push(format!("{} 个小上下文(<128K)模型", short_ctx_model_count));
        }
        println!("  注: 已过滤 {}（使用 'vcc preset provider show <name>' 查看全部）", parts.join("、"));
    }
    println!("  用法: vcc preset provider show <name>   # 查看平台或模型详情");
    println!("        vcc provider add <name> --preset <id> --key <API_KEY>");
    Ok(())
}

/// `vcc preset provider update` — 下载/更新 models.json
pub(crate) async fn update_provider() -> Result<()> {
    let rt = tokio::runtime::Handle::current();
    let path = rt.spawn(config::models::download_models_json()).await??;

    if is_json_mode() {
        output_json(&serde_json::json!({
            "success": true,
            "path": path.to_string_lossy(),
        }));
    } else {
        println!("已更新: {}", path.display());
    }
    Ok(())
}

/// `vcc preset provider show <name>` — 查看平台或模型详情
///
/// 自动判断 name 是平台 ID 还是模型 ID：
/// - 平台 ID（如 openai, anthropic）→ 显示该平台所有模型
/// - 模型 ID（如 gpt-4o, claude-sonnet-4-6）→ 显示该模型在所有平台的信息
pub(crate) fn show_provider_or_model(name: &str) -> Result<()> {
    let data = config::models::models_data();

    // 优先尝试匹配平台 ID
    if let Some(p) = data.find_provider(name) {
        return show_provider(p);
    }

    // 否则按模型 ID 查找
    let results = data.find_model_across_providers(name);
    if !results.is_empty() {
        return show_model(name, &results);
    }

    anyhow::bail!(
        "未找到平台或模型 '{}'. 运行 'vcc preset provider list' 查看所有可用项",
        name
    )
}

/// 显示指定平台的模型列表
fn show_provider(p: &config::models::ProviderInfo) -> Result<()> {
    if is_json_mode() {
        let models: Vec<serde_json::Value> = p
            .models
            .iter()
            .map(model_to_json)
            .collect();
        output_json(&serde_json::json!({
            "id": p.id,
            "name": p.name,
            "type": p.provider_type,
            "api": p.api,
            "env": p.env,
            "npm": p.npm,
            "doc": p.doc,
            "models": models,
        }));
        return Ok(());
    }

    println!("{} ({})", p.name, p.id);
    if let Some(ref api) = p.api {
        println!("  API: {}", api);
    }
    if !p.env.is_empty() {
        println!("  环境变量: {}", p.env.join(", "));
    }
    if let Some(ref npm) = p.npm {
        println!("  NPM: {}", npm);
    }
    if let Some(ref doc) = p.doc {
        println!("  文档: {}", doc);
    }
    println!("  类型: {}", p.provider_type);
    println!("  模型 ({} 个):", p.models.len());
    println!();

    // 按 family 分组显示
    let mut families: std::collections::HashMap<Option<String>, Vec<&config::models::ModelInfo>> =
        std::collections::HashMap::new();
    for m in &p.models {
        families.entry(m.family.clone()).or_default().push(m);
    }
    let mut family_keys: Vec<_> = families.keys().collect();
    family_keys.sort_by(|a, b| {
        a.as_deref().unwrap_or("zzz").cmp(b.as_deref().unwrap_or("zzz"))
    });

    for family in &family_keys {
        let models = families.get(family).unwrap();
        if let Some(ref f) = family {
            println!("  [{}]", f);
        }
        for m in models {
            let mut tags = Vec::new();
            if m.reasoning { tags.push("reasoning"); }
            if m.tool_call { tags.push("tool_call"); }
            if m.attachment { tags.push("attachment"); }
            let tag_str = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(",")) };
            let status_str = m.status.as_deref().map(|s| format!(" ({})", s)).unwrap_or_default();
            let price_str = match (m.input_price, m.output_price) {
                (Some(i), Some(o)) => format!(" ${:.2}/${:.2}", i, o),
                _ => String::new(),
            };
            let ctx_str = m.context_limit
                .map(|c| format!(" {}k", c / 1000))
                .unwrap_or_default();
            println!("    {}{}{}{}{}", m.id, tag_str, status_str, ctx_str, price_str);
        }
        println!();
    }
    Ok(())
}

/// 显示模型在所有平台的信息
fn show_model(query: &str, results: &[(&config::models::ProviderInfo, &config::models::ModelInfo)]) -> Result<()> {
    if is_json_mode() {
        let items: Vec<serde_json::Value> = results
            .iter()
            .map(|(p, m)| {
                serde_json::json!({
                    "provider": p.id,
                    "provider_name": p.name,
                    "model": model_to_json(m),
                })
            })
            .collect();
        output_json(&serde_json::json!({
            "query": query,
            "results": items,
            "count": items.len(),
        }));
        return Ok(());
    }

    println!("匹配 '{}' 的模型 ({} 个平台):", query, results.len());
    println!();
    for (p, m) in results {
        let mut tags = Vec::new();
        if m.reasoning { tags.push("reasoning"); }
        if m.tool_call { tags.push("tool_call"); }
        if m.attachment { tags.push("attachment"); }
        let tag_str = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(",")) };
        let price_str = match (m.input_price, m.output_price) {
            (Some(i), Some(o)) => format!(" ${:.2}/${:.2}", i, o),
            _ => String::new(),
        };
        println!("  {} / {}{}{}", p.id, m.id, tag_str, price_str);
        if let Some(ref name) = m.name {
            println!("    {}", name);
        }
    }
    Ok(())
}

/// `vcc preset mcp` — 列出 MCP 预设
pub(crate) fn list_mcp() -> Result<()> {
    let presets = config::presets();

    if is_json_mode() {
        let mcps: Vec<serde_json::Value> = presets
            .mcp
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "command": p.command,
                    "args": p.args,
                    "description": p.description,
                })
            })
            .collect();
        output_json(&serde_json::json!({ "mcp": mcps }));
        return Ok(());
    }

    println!("MCP Server 预设:");
    println!("{:<20} 描述", "名称");
    println!("{}", "-".repeat(50));
    for p in &presets.mcp {
        println!("{:<20} {}", p.name, p.description);
    }
    println!();
    println!("  用法: vcc mcp add <name> --preset <preset>");
    Ok(())
}

/// `vcc preset` 无子命令 — 显示总览
pub(crate) fn list_presets_overview() -> Result<()> {
    let data = config::models::models_data();
    let has_file = config::models::models_json_path()
        .map(|p| p.exists())
        .unwrap_or(false);
    let cache_age = config::models::models_cache_age();

    let model_count: usize = data.providers.values().map(|p| p.models.len()).sum();
    println!("Provider 平台: {} 个, 模型: {} 个", data.providers.len(), model_count);
    println!("MCP 预设: {} 个", config::presets().mcp.len());
    if has_file {
        let age_str = cache_age.as_deref().unwrap_or("未知");
        println!("数据缓存: models.dev (缓存于 {})", age_str);
    }
    println!();
    println!("子命令:");
    println!("  vcc preset provider list              列出所有平台和模型");
    println!("  vcc preset provider show <name>       查看平台或模型详情");
    println!("  vcc preset provider update            下载/更新平台数据");
    println!("  vcc preset mcp                        列出 MCP 预设");
    if !has_file {
        println!();
        println!("提示: 运行 'vcc preset provider update' 获取完整平台数据");
    }
    Ok(())
}

fn model_to_json(m: &config::models::ModelInfo) -> serde_json::Value {
    serde_json::json!({
        "id": m.id,
        "name": m.name,
        "family": m.family,
        "reasoning": m.reasoning,
        "tool_call": m.tool_call,
        "attachment": m.attachment,
        "context_limit": m.context_limit,
        "output_limit": m.output_limit,
        "input_price": m.input_price,
        "output_price": m.output_price,
        "status": m.status,
    })
}
