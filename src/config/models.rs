//! models.dev 数据加载与查询
//!
//! 从 https://models.dev/api.json 下载的 provider/model 数据，
//! 保存到 `~/.config/VibeCodingControl/models.json`，按需加载。
//!
//! 字段定义参考: https://github.com/anomalyco/models.dev

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

// ══════════════════════════════════════════════════════════
// 数据结构（公开，供 CLI 和 model 层使用）
// ══════════════════════════════════════════════════════════

/// 顶层：provider id → ProviderInfo
#[derive(Debug, Clone, Default)]
pub(crate) struct ModelsData {
    pub providers: HashMap<String, ProviderInfo>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderInfo {
    pub id: String,
    pub name: String,
    /// API base URL（部分 provider 有）
    pub api: Option<String>,
    /// 环境变量名列表
    pub env: Vec<String>,
    /// npm 包名（如 @ai-sdk/openai）
    pub npm: Option<String>,
    /// 文档链接
    pub doc: Option<String>,
    /// 推断的 vcc provider_type: "openai" | "anthropic" | "google"
    pub provider_type: String,
    /// 该 provider 下的模型列表
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct ModelInfo {
    pub id: String,
    pub name: Option<String>,
    pub family: Option<String>,
    // ── 能力标记 ──
    pub reasoning: bool,
    pub tool_call: bool,
    pub attachment: bool,
    /// 是否支持温度调节
    pub temperature: Option<bool>,
    /// 是否支持结构化输出
    pub structured_output: Option<bool>,
    /// 是否开源权重
    pub open_weights: Option<bool>,
    // ── 限制 ──
    pub context_limit: Option<u64>,
    pub input_limit: Option<u64>,
    pub output_limit: Option<u64>,
    // ── 模态 ──
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    // ── 价格（每百万 token, USD）──
    pub input_price: Option<f64>,
    pub output_price: Option<f64>,
    pub reasoning_price: Option<f64>,
    pub cache_read_price: Option<f64>,
    pub cache_write_price: Option<f64>,
    pub input_audio_price: Option<f64>,
    pub output_audio_price: Option<f64>,
    // ── 状态与日期 ──
    /// "alpha" | "beta" | "deprecated" | None（稳定）
    pub status: Option<String>,
    pub knowledge: Option<String>,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
}

// ══════════════════════════════════════════════════════════
// JSON 解析（models.dev/api.json 格式）
// ══════════════════════════════════════════════════════════

#[derive(Deserialize, Debug)]
struct ApiRoot(HashMap<String, ApiProvider>);

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ApiProvider {
    id: String,
    name: String,
    #[serde(default)]
    env: Vec<String>,
    npm: Option<String>,
    doc: Option<String>,
    api: Option<String>,
    #[serde(default)]
    models: HashMap<String, ApiModel>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ApiModel {
    id: String,
    name: Option<String>,
    family: Option<String>,
    // ── 能力标记 ──
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    attachment: bool,
    temperature: Option<bool>,
    structured_output: Option<bool>,
    open_weights: Option<bool>,
    // ── 限制 ──
    limit: Option<ApiLimit>,
    // ── 模态 ──
    #[serde(default)]
    modalities: Option<ApiModalities>,
    // ── 价格 ──
    cost: Option<ApiCost>,
    // ── 状态与日期 ──
    status: Option<String>,
    knowledge: Option<String>,
    release_date: Option<String>,
    last_updated: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ApiLimit {
    context: Option<u64>,
    input: Option<u64>,
    output: Option<u64>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ApiCost {
    input: Option<f64>,
    output: Option<f64>,
    reasoning: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
    input_audio: Option<f64>,
    output_audio: Option<f64>,
}

#[derive(Deserialize, Debug)]
struct ApiModalities {
    #[serde(default)]
    input: Vec<String>,
    #[serde(default)]
    output: Vec<String>,
}

// ══════════════════════════════════════════════════════════
// 解析 & 查询
// ══════════════════════════════════════════════════════════

impl ModelsData {
    /// 从 JSON 字节解析
    pub fn from_json(data: &[u8]) -> Result<Self> {
        let root: ApiRoot =
            serde_json::from_slice(data).context("failed to parse models.dev API JSON")?;
        let mut providers = HashMap::new();
        for (_, ap) in root.0 {
            let provider_type = infer_provider_type(&ap.id, ap.npm.as_deref());
            let models: Vec<ModelInfo> = ap
                .models
                .into_values()
                .map(|m| ModelInfo {
                    id: m.id,
                    name: m.name,
                    family: m.family,
                    reasoning: m.reasoning,
                    tool_call: m.tool_call,
                    attachment: m.attachment,
                    temperature: m.temperature,
                    structured_output: m.structured_output,
                    open_weights: m.open_weights,
                    context_limit: m.limit.as_ref().and_then(|l| l.context),
                    input_limit: m.limit.as_ref().and_then(|l| l.input),
                    output_limit: m.limit.as_ref().and_then(|l| l.output),
                    input_modalities: m
                        .modalities
                        .as_ref()
                        .map(|md| md.input.clone())
                        .unwrap_or_default(),
                    output_modalities: m
                        .modalities
                        .as_ref()
                        .map(|md| md.output.clone())
                        .unwrap_or_default(),
                    input_price: m.cost.as_ref().and_then(|c| c.input),
                    output_price: m.cost.as_ref().and_then(|c| c.output),
                    reasoning_price: m.cost.as_ref().and_then(|c| c.reasoning),
                    cache_read_price: m.cost.as_ref().and_then(|c| c.cache_read),
                    cache_write_price: m.cost.as_ref().and_then(|c| c.cache_write),
                    input_audio_price: m.cost.as_ref().and_then(|c| c.input_audio),
                    output_audio_price: m.cost.as_ref().and_then(|c| c.output_audio),
                    status: m.status,
                    knowledge: m.knowledge,
                    release_date: m.release_date,
                    last_updated: m.last_updated,
                })
                .collect();
            providers.insert(
                ap.id.clone(),
                ProviderInfo {
                    id: ap.id,
                    name: ap.name,
                    api: ap.api,
                    env: ap.env,
                    npm: ap.npm,
                    doc: ap.doc,
                    provider_type,
                    models,
                },
            );
        }
        Ok(ModelsData { providers })
    }

    /// 获取内置 fallback 预设（无需联网）
    pub fn builtin_fallback() -> Self {
        let mut providers = HashMap::new();
        providers.insert(
            "anthropic".into(),
            ProviderInfo {
                id: "anthropic".into(),
                name: "Anthropic".into(),
                api: None,
                env: vec!["ANTHROPIC_API_KEY".into()],
                npm: Some("@ai-sdk/anthropic".into()),
                doc: None,
                provider_type: "anthropic".into(),
                models: vec![ModelInfo {
                    id: "claude-sonnet-4-6".into(),
                    name: Some("Claude Sonnet 4.6".into()),
                    family: Some("claude".into()),
                    reasoning: false,
                    tool_call: true,
                    attachment: true,
                    temperature: Some(true),
                    structured_output: Some(true),
                    open_weights: Some(false),
                    context_limit: Some(200000),
                    input_limit: None,
                    output_limit: Some(64000),
                    input_modalities: vec!["text".into(), "image".into(), "pdf".into()],
                    output_modalities: vec!["text".into()],
                    input_price: Some(3.0),
                    output_price: Some(15.0),
                    reasoning_price: None,
                    cache_read_price: None,
                    cache_write_price: None,
                    input_audio_price: None,
                    output_audio_price: None,
                    status: None,
                    knowledge: None,
                    release_date: None,
                    last_updated: None,
                }],
            },
        );
        providers.insert(
            "openai".into(),
            ProviderInfo {
                id: "openai".into(),
                name: "OpenAI".into(),
                api: Some("https://api.openai.com/v1".into()),
                env: vec!["OPENAI_API_KEY".into()],
                npm: Some("@ai-sdk/openai".into()),
                doc: None,
                provider_type: "openai".into(),
                models: vec![ModelInfo {
                    id: "gpt-4o".into(),
                    name: Some("GPT-4o".into()),
                    family: Some("gpt-4o".into()),
                    reasoning: false,
                    tool_call: true,
                    attachment: true,
                    temperature: Some(true),
                    structured_output: Some(true),
                    open_weights: Some(false),
                    context_limit: Some(128000),
                    input_limit: None,
                    output_limit: Some(16384),
                    input_modalities: vec!["text".into(), "image".into(), "audio".into()],
                    output_modalities: vec!["text".into()],
                    input_price: Some(2.5),
                    output_price: Some(10.0),
                    reasoning_price: None,
                    cache_read_price: Some(1.25),
                    cache_write_price: None,
                    input_audio_price: None,
                    output_audio_price: None,
                    status: None,
                    knowledge: None,
                    release_date: None,
                    last_updated: None,
                }],
            },
        );
        providers.insert(
            "google".into(),
            ProviderInfo {
                id: "google".into(),
                name: "Google".into(),
                api: None,
                env: vec!["GOOGLE_API_KEY".into()],
                npm: Some("@ai-sdk/google".into()),
                doc: None,
                provider_type: "google".into(),
                models: vec![ModelInfo {
                    id: "gemini-2.0-flash".into(),
                    name: Some("Gemini 2.0 Flash".into()),
                    family: Some("gemini".into()),
                    reasoning: false,
                    tool_call: true,
                    attachment: true,
                    temperature: Some(true),
                    structured_output: Some(true),
                    open_weights: Some(false),
                    context_limit: Some(1048576),
                    input_limit: None,
                    output_limit: Some(8192),
                    input_modalities: vec!["text".into(), "image".into(), "audio".into(), "video".into(), "pdf".into()],
                    output_modalities: vec!["text".into()],
                    input_price: None,
                    output_price: None,
                    reasoning_price: None,
                    cache_read_price: None,
                    cache_write_price: None,
                    input_audio_price: None,
                    output_audio_price: None,
                    status: None,
                    knowledge: None,
                    release_date: None,
                    last_updated: None,
                }],
            },
        );
        ModelsData { providers }
    }

    /// 按 provider id 查找
    pub fn find_provider(&self, id: &str) -> Option<&ProviderInfo> {
        self.providers.get(id)
    }

    /// 按模型名称查找定价信息（用于 usage 计费）
    ///
    /// 匹配策略（按优先级）：
    /// 1. 精确匹配 model id，优先选原始 provider（provider id 最短的）
    /// 2. session model 是 model id 的前缀（如 "claude-sonnet-4" 匹配 "claude-sonnet-4-6"）
    /// 3. 去掉日期后缀后匹配
    pub fn find_model_pricing(&self, model_name: &str) -> Option<&ModelInfo> {
        let name_lower = model_name.to_lowercase();

        // 原始 provider 优先级列表（越靠前越优先）
        const PREFERRED_PROVIDERS: &[&str] = &[
            "anthropic", "openai", "google", "zai", "zhipuai", "deepseek", "mistral",
        ];

        // 1. 精确匹配
        let mut exact_matches: Vec<(&ProviderInfo, &ModelInfo)> = Vec::new();
        for p in self.providers.values() {
            for m in &p.models {
                if m.id.to_lowercase() == name_lower {
                    exact_matches.push((p, m));
                }
            }
        }
        if !exact_matches.is_empty() {
            // 优先选 preferred provider
            for pref in PREFERRED_PROVIDERS {
                if let Some((_, m)) = exact_matches.iter().find(|(p, _)| p.id == *pref) {
                    return Some(m);
                }
            }
            // 否则按 provider id 长度排序，短的优先（原始 provider 通常名称较短）
            exact_matches.sort_by_key(|(p, _)| p.id.len());
            return Some(exact_matches[0].1);
        }

        // 2. 标准化匹配：点转横线后重试精确匹配
        //    "claude-opus-4.5" → "claude-opus-4-5"
        let normalized = name_lower.replace('.', "-");
        if normalized != name_lower {
            let mut norm_matches: Vec<(&ProviderInfo, &ModelInfo)> = Vec::new();
            for p in self.providers.values() {
                for m in &p.models {
                    if m.id.to_lowercase() == normalized {
                        norm_matches.push((p, m));
                    }
                }
            }
            if !norm_matches.is_empty() {
                for pref in PREFERRED_PROVIDERS {
                    if let Some((_, m)) = norm_matches.iter().find(|(p, _)| p.id == *pref) {
                        return Some(m);
                    }
                }
                norm_matches.sort_by_key(|(p, _)| p.id.len());
                return Some(norm_matches[0].1);
            }
        }

        // 3. 前缀匹配：session model name 是 model id 的前缀
        //    "claude-sonnet-4" → "claude-sonnet-4-6"
        //    也用标准化后的名称尝试前缀匹配
        let mut prefix_matches: Vec<(&ProviderInfo, &ModelInfo)> = Vec::new();
        for p in self.providers.values() {
            for m in &p.models {
                let m_lower = m.id.to_lowercase();
                if m_lower.starts_with(&name_lower) || m_lower.starts_with(&normalized) {
                    prefix_matches.push((p, m));
                }
            }
        }
        if !prefix_matches.is_empty() {
            // 优先选 preferred provider
            for pref in PREFERRED_PROVIDERS {
                if let Some((_, m)) = prefix_matches.iter().find(|(p, _)| p.id == *pref) {
                    return Some(m);
                }
            }
            // 否则优先选稳定版、短 id
            prefix_matches.sort_by(|a, b| {
                let a_stable = a.1.status.is_none();
                let b_stable = b.1.status.is_none();
                b_stable.cmp(&a_stable).then(a.0.id.len().cmp(&b.0.id.len())).then(a.1.id.len().cmp(&b.1.id.len()))
            });
            return Some(prefix_matches[0].1);
        }

        // 4. 去掉日期后缀再匹配
        //    "claude-opus-4-20250514" → "claude-opus-4"
        let stripped = strip_date_suffix(&name_lower);
        if stripped != name_lower {
            let mut stripped_matches: Vec<(&ProviderInfo, &ModelInfo)> = Vec::new();
            for p in self.providers.values() {
                for m in &p.models {
                    let m_stripped = strip_date_suffix(&m.id.to_lowercase());
                    if m_stripped == stripped {
                        stripped_matches.push((p, m));
                    }
                }
            }
            if !stripped_matches.is_empty() {
                for pref in PREFERRED_PROVIDERS {
                    if let Some((_, m)) = stripped_matches.iter().find(|(p, _)| p.id == *pref) {
                        return Some(m);
                    }
                }
                stripped_matches.sort_by_key(|(p, _)| p.id.len());
                return Some(stripped_matches[0].1);
            }
        }

        None
    }

    /// 判断 provider 是否为 AI 原厂（而非转售/聚合/托管平台）
    ///
    /// 判断逻辑：只保留真正的 AI 模型制造商白名单，
    /// 排除云托管（azure, amazon-bedrock, vertex）、聚合代理（openrouter, vercel）、
    /// 转售平台（使用 @ai-sdk/openai-compatible 的 84 家）等。
    pub fn is_original_provider(&self, provider_id: &str) -> bool {
        // AI 原厂白名单：拥有自研模型的公司
        const ORIGINAL_MANUFACTURERS: &[&str] = &[
            // ── 第一梯队 ──
            "anthropic", "openai", "google",
            // ── 中国厂商 ──
            "zai", "zhipuai", "deepseek", "alibaba", "minimax", "minimax-cn",
            "minimax-coding-plan", "minimax-cn-coding-plan",
            "alibaba-cn", "alibaba-coding-plan", "alibaba-coding-plan-cn",
            "bailing",
            // ── 欧美厂商 ──
            "mistral", "xai", "cohere", "perplexity", "cerebras",
            // ── 推理优化/部署平台（有自己的硬件和专有模型）──
            "groq", "fireworks-ai", "togetherai",
            // ── 其他原厂 ──
            "cloudflare-workers-ai",   // Workers AI 有自研模型
        ];
        ORIGINAL_MANUFACTURERS.contains(&provider_id)
    }

    /// 按模型 id 模糊查找所有支持该模型的 provider
    pub fn find_model_across_providers(&self, model_id: &str) -> Vec<(&ProviderInfo, &ModelInfo)> {
        let mut results = Vec::new();
        let model_lower = model_id.to_lowercase();
        for p in self.providers.values() {
            for m in &p.models {
                if m.id.to_lowercase().contains(&model_lower) {
                    results.push((p, m));
                }
            }
        }
        results.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        results
    }
}

/// 推断 provider_type
fn infer_provider_type(id: &str, npm: Option<&str>) -> String {
    let npm_lower = npm.map(|s| s.to_lowercase()).unwrap_or_default();
    if id == "anthropic" || npm_lower.contains("anthropic") {
        "anthropic".to_string()
    } else if id == "google" || id == "vertex" || npm_lower.contains("google") {
        "google".to_string()
    } else {
        "openai".to_string()
    }
}

// ══════════════════════════════════════════════════════════
// 文件 I/O
// ══════════════════════════════════════════════════════════

/// models.json 文件路径
pub(crate) fn models_json_path() -> Option<PathBuf> {
    crate::store::TomlStore::default_root().ok().map(|root| root.join("models.json"))
}

/// 返回 models.json 缓存的年龄描述（如 "3 天前"、"2 小时前"）
///
/// 返回 None 表示文件不存在
pub(crate) fn models_cache_age() -> Option<String> {
    let path = models_json_path()?;
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta.modified().ok()?;
    let elapsed = std::time::SystemTime::now().duration_since(modified).ok()?;
    Some(format_duration(elapsed))
}

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{} 秒前", secs)
    } else if secs < 3600 {
        format!("{} 分钟前", secs / 60)
    } else if secs < 86400 {
        format!("{} 小时前", secs / 3600)
    } else {
        format!("{} 天前", secs / 86400)
    }
}

/// 从本地文件加载 ModelsData
///
/// 文件不存在时返回 None（调用方应使用 builtin_fallback）
pub(crate) fn load_models_data() -> Option<ModelsData> {
    let path = models_json_path()?;
    if !path.exists() {
        return None;
    }
    let data = std::fs::read(&path).ok()?;
    ModelsData::from_json(&data).ok()
}

/// 下载 models.dev/api.json 并保存到本地
pub(crate) async fn download_models_json() -> Result<PathBuf> {
    let path = models_json_path()
        .context("cannot determine models.json path")?;

    let response = reqwest::get("https://models.dev/api.json")
        .await
        .context("failed to download models.dev/api.json")?;

    if !response.status().is_success() {
        anyhow::bail!(
            "failed to download models.dev/api.json: HTTP {}",
            response.status()
        );
    }

    let bytes = response
        .bytes()
        .await
        .context("failed to read response body")?;

    // 验证是有效 JSON
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .context("downloaded content is not valid JSON")?;

    // 确保父目录存在
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory: {}", parent.display()))?;
    }

    std::fs::write(&path, &bytes)
        .with_context(|| format!("failed to write {}", path.display()))?;

    Ok(path)
}

// ══════════════════════════════════════════════════════════
// 全局单例（优先文件，fallback 内置）
// ══════════════════════════════════════════════════════════

static MODELS: OnceLock<ModelsData> = OnceLock::new();

/// 获取 ModelsData 单例
///
/// 优先从 `~/.config/VibeCodingControl/models.json` 加载，
/// 文件不存在则使用内置 fallback（anthropic/openai/google）。
pub(crate) fn models_data() -> &'static ModelsData {
    MODELS.get_or_init(|| load_models_data().unwrap_or_else(ModelsData::builtin_fallback))
}

/// 重新加载（下载更新后调用）
#[allow(dead_code)]
pub(crate) fn reload_models_data() -> &'static ModelsData {
    let data = load_models_data().unwrap_or_else(ModelsData::builtin_fallback);
    // OnceLock 只能设置一次，如果已经被初始化则返回旧值
    // 这里我们接受这个限制：重启后生效
    let _ = MODELS.set(data);
    MODELS.get_or_init(ModelsData::builtin_fallback)
}

/// 去掉模型 ID 中的日期后缀（如 "-20250514"）
fn strip_date_suffix(id: &str) -> String {
    let re = regex::Regex::new(r"-\d{8}$").unwrap();
    re.replace(id, "").to_string()
}

// ══════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_fallback_has_three_providers() {
        let data = ModelsData::builtin_fallback();
        assert_eq!(data.providers.len(), 3);
        assert!(data.find_provider("anthropic").is_some());
        assert!(data.find_provider("openai").is_some());
        assert!(data.find_provider("google").is_some());
    }

    #[test]
    fn test_builtin_fallback_provider_types() {
        let data = ModelsData::builtin_fallback();
        assert_eq!(data.find_provider("anthropic").unwrap().provider_type, "anthropic");
        assert_eq!(data.find_provider("openai").unwrap().provider_type, "openai");
        assert_eq!(data.find_provider("google").unwrap().provider_type, "google");
    }

    #[test]
    fn test_builtin_fallback_openai_has_api_url() {
        let data = ModelsData::builtin_fallback();
        let openai = data.find_provider("openai").unwrap();
        assert_eq!(openai.api.as_deref(), Some("https://api.openai.com/v1"));
    }

    #[test]
    fn test_builtin_fallback_anthropic_no_api_url() {
        let data = ModelsData::builtin_fallback();
        let anthropic = data.find_provider("anthropic").unwrap();
        assert!(anthropic.api.is_none());
    }

    #[test]
    fn test_find_model_across_providers() {
        let data = ModelsData::builtin_fallback();
        let results = data.find_model_across_providers("claude");
        assert!(!results.is_empty());
        assert_eq!(results[0].0.id, "anthropic");
    }

    #[test]
    fn test_find_model_case_insensitive() {
        let data = ModelsData::builtin_fallback();
        let results = data.find_model_across_providers("GPT-4O");
        assert!(!results.is_empty());
        assert_eq!(results[0].0.id, "openai");
    }

    #[test]
    fn test_find_model_no_match() {
        let data = ModelsData::builtin_fallback();
        let results = data.find_model_across_providers("nonexistent-model");
        assert!(results.is_empty());
    }

    #[test]
    fn test_infer_provider_type_anthropic() {
        assert_eq!(infer_provider_type("anthropic", None), "anthropic");
        assert_eq!(
            infer_provider_type("other", Some("@anthropic/sdk")),
            "anthropic"
        );
    }

    #[test]
    fn test_infer_provider_type_google() {
        assert_eq!(infer_provider_type("google", None), "google");
        assert_eq!(infer_provider_type("vertex", None), "google");
        assert_eq!(
            infer_provider_type("other", Some("@google/generative-ai")),
            "google"
        );
    }

    #[test]
    fn test_infer_provider_type_openai_default() {
        assert_eq!(infer_provider_type("deepseek", None), "openai");
        assert_eq!(infer_provider_type("openrouter", None), "openai");
    }

    #[test]
    fn test_parse_minimal_json() {
        let json = r#"{
            "test-provider": {
                "id": "test-provider",
                "name": "Test Provider",
                "env": ["TEST_API_KEY"],
                "models": {
                    "test-model": {
                        "id": "test-model"
                    }
                }
            }
        }"#;
        let data = ModelsData::from_json(json.as_bytes()).unwrap();
        assert_eq!(data.providers.len(), 1);
        let p = data.find_provider("test-provider").unwrap();
        assert_eq!(p.name, "Test Provider");
        assert_eq!(p.models.len(), 1);
        assert_eq!(p.models[0].id, "test-model");
        assert!(!p.models[0].reasoning);
        assert!(!p.models[0].tool_call);
        assert!(!p.models[0].attachment);
    }

    #[test]
    fn test_parse_full_model() {
        let json = r#"{
            "test": {
                "id": "test",
                "name": "Test",
                "api": "https://api.test.com/v1",
                "npm": "@test/sdk",
                "doc": "https://docs.test.com",
                "env": ["TEST_KEY"],
                "models": {
                    "m1": {
                        "id": "m1",
                        "name": "Model One",
                        "family": "test",
                        "reasoning": true,
                        "toolCall": true,
                        "attachment": true,
                        "structuredOutput": true,
                        "temperature": true,
                        "openWeights": false,
                        "cost": {
                            "input": 1.5,
                            "output": 5.0,
                            "reasoning": 8.0,
                            "cacheRead": 0.5,
                            "cacheWrite": 1.0
                        },
                        "limit": {
                            "context": 128000,
                            "output": 4096
                        },
                        "modalities": {
                            "input": ["text", "image"],
                            "output": ["text"]
                        },
                        "status": "beta",
                        "knowledge": "2025-04",
                        "releaseDate": "2025-01",
                        "lastUpdated": "2025-04"
                    }
                }
            }
        }"#;
        let data = ModelsData::from_json(json.as_bytes()).unwrap();
        let m = &data.find_provider("test").unwrap().models[0];
        assert_eq!(m.name.as_deref(), Some("Model One"));
        assert_eq!(m.family.as_deref(), Some("test"));
        assert!(m.reasoning);
        assert!(m.tool_call);
        assert!(m.attachment);
        assert_eq!(m.structured_output, Some(true));
        assert_eq!(m.temperature, Some(true));
        assert_eq!(m.open_weights, Some(false));
        assert_eq!(m.context_limit, Some(128000));
        assert_eq!(m.output_limit, Some(4096));
        assert_eq!(m.input_price, Some(1.5));
        assert_eq!(m.output_price, Some(5.0));
        assert_eq!(m.reasoning_price, Some(8.0));
        assert_eq!(m.cache_read_price, Some(0.5));
        assert_eq!(m.cache_write_price, Some(1.0));
        assert_eq!(m.input_modalities, vec!["text", "image"]);
        assert_eq!(m.output_modalities, vec!["text"]);
        assert_eq!(m.status.as_deref(), Some("beta"));
        assert_eq!(m.knowledge.as_deref(), Some("2025-04"));
        assert_eq!(m.release_date.as_deref(), Some("2025-01"));
        assert_eq!(m.last_updated.as_deref(), Some("2025-04"));
    }

    #[test]
    fn test_parse_invalid_json_returns_error() {
        let result = ModelsData::from_json(b"not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_models_json_path_is_under_vcc_root() {
        let path = models_json_path();
        assert!(path.is_some());
        let p = path.unwrap();
        assert!(p.to_string_lossy().contains("VibeCodingControl"));
        assert!(p.to_string_lossy().ends_with("models.json"));
    }

    #[test]
    fn test_find_model_pricing_exact_match() {
        let data = ModelsData::builtin_fallback();
        let m = data.find_model_pricing("claude-sonnet-4-6");
        assert!(m.is_some());
        assert_eq!(m.unwrap().input_price, Some(3.0));
    }

    #[test]
    fn test_find_model_pricing_prefix_match() {
        let data = ModelsData::builtin_fallback();
        // "claude-sonnet-4" is a prefix of "claude-sonnet-4-6"
        let m = data.find_model_pricing("claude-sonnet-4");
        assert!(m.is_some());
    }

    #[test]
    fn test_find_model_pricing_case_insensitive() {
        let data = ModelsData::builtin_fallback();
        let m = data.find_model_pricing("GPT-4o");
        assert!(m.is_some());
        assert_eq!(m.unwrap().input_price, Some(2.5));
    }

    #[test]
    fn test_find_model_pricing_no_match() {
        let data = ModelsData::builtin_fallback();
        let m = data.find_model_pricing("nonexistent-model");
        assert!(m.is_none());
    }

    #[test]
    fn test_find_model_pricing_prefers_original_provider() {
        // Create data with multiple providers having the same model
        let json = r#"{
            "zai": {
                "id": "zai",
                "name": "Z.AI",
                "models": {
                    "glm-5.1": { "id": "glm-5.1", "cost": { "input": 0.86, "output": 3.50 } }
                }
            },
            "reseller": {
                "id": "reseller",
                "name": "Reseller",
                "models": {
                    "glm-5.1": { "id": "glm-5.1", "cost": { "input": 6.00, "output": 24.00 } }
                }
            }
        }"#;
        let data = ModelsData::from_json(json.as_bytes()).unwrap();
        let m = data.find_model_pricing("glm-5.1").unwrap();
        // zai (shorter provider id) should be preferred over reseller
        assert_eq!(m.input_price, Some(0.86));
    }

    #[test]
    fn test_strip_date_suffix() {
        assert_eq!(strip_date_suffix("claude-opus-4-20250514"), "claude-opus-4");
        assert_eq!(strip_date_suffix("gpt-4o-20241120"), "gpt-4o");
        assert_eq!(strip_date_suffix("claude-sonnet-4-6"), "claude-sonnet-4-6");
    }
}
