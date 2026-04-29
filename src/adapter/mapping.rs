use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::adapter::{json_to_toml_value, merge_extra_to_json_with_map};
use crate::datasource::{json_str_array, json_str_map};
use crate::model::{
    mcp::{McpConfig, McpServer},
    Metadata, Resource,
};

fn toml_str_map(table: &toml::map::Map<String, toml::Value>, key: &str) -> HashMap<String, String> {
    table
        .get(key)
        .and_then(|e| e.as_table())
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}
fn mcp_metadata(tool_name: &str) -> Metadata {
    Metadata {
        description: Some(
            crate::config::adapter_defaults()
                .defaults
                .sync_description(tool_name),
        ),
        tags: vec![crate::config::adapter_defaults().defaults.sync_tag.clone()],
        ..Default::default()
    }
}
fn build_mcp_server(
    name: &str,
    config: McpConfig,
    tool_name: &str,
    tool: HashMap<String, crate::model::mcp::McpToolOverride>,
) -> McpServer {
    let mut mcp = McpServer::new_with_name(name);
    mcp.config = config;
    mcp.metadata = mcp_metadata(tool_name);
    mcp.tool = tool;
    mcp
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct ToolMapping {
    pub tool: ToolInfo,
    pub settings_path: Option<String>,
    #[serde(default)]
    pub mcp: McpMappingConfig,
    #[serde(default)]
    pub provider: ProviderMappingConfig,
    #[serde(default)]
    pub prompt: PromptMappingConfig,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub hook: HookMappingConfig,
    #[serde(default)]
    pub env: EnvMappingConfig,
    #[serde(default)]
    pub session: SessionMappingConfig,
    #[serde(default)]
    pub agent: AgentMappingConfig,
    #[serde(default)]
    pub skill: SkillMappingConfig,
    #[serde(default)]
    pub plugin: PluginMappingConfig,
}

#[derive(Deserialize, Debug, Clone)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
pub(crate) struct ToolInfo {
    pub name: String,
    pub config_dir: String,
}

fn default_env_field() -> String {
    "env".into()
}
fn default_headers_field() -> String {
    "headers".into()
}
fn default_disabled_tools_field() -> String {
    "disabledTools".into()
}
fn default_plugin_disabled_key() -> String {
    "disabled".into()
}
fn default_url_types() -> Vec<String> {
    vec!["sse".into(), "streamable-http".into()]
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct McpMappingConfig {
    #[serde(default)]
    pub format: String,
    pub path: Option<String>,
    #[serde(default)]
    pub servers_key: String,
    #[serde(default)]
    pub disabled_key: String,
    /// If set, this tool uses an "enabled" field (true=on, false=off)
    /// instead of the "disabled" field (true=off, false=on).
    /// When `enabled_key` is set, `disabled_key` is ignored.
    #[serde(default)]
    pub enabled_key: String,
    #[serde(default)]
    pub type_map: HashMap<String, String>,
    #[serde(default)]
    pub field_map: HashMap<String, String>,
    #[serde(default)]
    pub type_field_map: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub known_keys: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub command_format: String,
    #[serde(default = "default_env_field")]
    pub env_field: String,
    #[serde(default = "default_headers_field")]
    pub headers_field: String,
    #[serde(default = "default_disabled_tools_field")]
    pub disabled_tools_field: String,
    #[serde(default)]
    pub extra_field_map: HashMap<String, String>,
    #[serde(default)]
    pub default_fields: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub skip_type_for: Vec<String>,
    #[serde(default)]
    pub toml_known_keys: Vec<String>,
    #[serde(default)]
    pub jsonc: bool,
    #[serde(default)]
    pub fallback_paths: Vec<String>,
    #[serde(default = "default_url_types")]
    pub url_types: Vec<String>,
}

impl Default for McpMappingConfig {
    fn default() -> Self {
        Self {
            format: "json".into(),
            path: None,
            servers_key: "mcpServers".into(),
            disabled_key: "disabled".into(),
            enabled_key: String::new(),
            type_map: HashMap::new(),
            field_map: HashMap::new(),
            type_field_map: HashMap::new(),
            known_keys: HashMap::new(),
            command_format: "string_args".into(),
            env_field: "env".into(),
            headers_field: "headers".into(),
            disabled_tools_field: "disabledTools".into(),
            extra_field_map: HashMap::new(),
            default_fields: HashMap::new(),
            skip_type_for: Vec::new(),
            toml_known_keys: Vec::new(),
            jsonc: false,
            fallback_paths: Vec::new(),
            url_types: vec!["sse".into(), "streamable-http".into()],
        }
    }
}
impl McpMappingConfig {
    pub fn path(&self) -> &str {
        self.path.as_deref().unwrap_or(if self.format == "toml" {
            "config.toml"
        } else {
            "settings.json"
        })
    }
    /// Returns true if this tool uses an "enabled" field (opencode) instead of "disabled" (claude/codex/kimi).
    pub fn uses_enabled_semantic(&self) -> bool {
        !self.enabled_key.is_empty()
    }
    /// Returns the key name for the toggle field (either `enabled_key` or `disabled_key`).
    pub fn toggle_key(&self) -> &str {
        if self.uses_enabled_semantic() {
            &self.enabled_key
        } else {
            &self.disabled_key
        }
    }
    /// Returns true if the given server type is a URL-based type (sse/streamable-http).
    pub fn is_url_type(&self, t: &str) -> bool {
        self.url_types.iter().any(|u| u == t)
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
pub(crate) struct ProviderMappingConfig {
    #[serde(default)]
    pub format: String,
    pub path: Option<String>,
    pub providers_key: Option<String>,
    #[serde(default)]
    pub env_mapping: Vec<EnvVarMapping>,
    pub api_key_key: Option<String>,
    pub base_url_key: Option<String>,
    #[serde(default)]
    pub type_map: Vec<TypeValueMapping>,
    pub sync_name: Option<String>,
    pub sync_type: Option<String>,
    #[serde(default)]
    pub custom_model_fields: HashMap<String, String>,
    #[serde(default)]
    pub custom_model_known_keys: Vec<String>,
    pub auth_path: Option<String>,
    pub config_path: Option<String>,
    #[serde(default)]
    pub model_env: Vec<ModelEnvMapping>,
    #[serde(default)]
    pub npm_type_map: HashMap<String, String>,
    #[serde(default)]
    pub defaults: HashMap<String, toml::Value>,
    #[serde(default)]
    pub field_map: Option<crate::adapter::doc_engine::FieldMapConfig>,
}

impl ProviderMappingConfig {
    pub fn api_key_key(&self) -> &str {
        self.api_key_key.as_deref().unwrap_or("API_KEY")
    }
    pub fn base_url_key(&self) -> &str {
        self.base_url_key.as_deref().unwrap_or("API_BASE_URL")
    }
    pub fn auth_path(&self) -> &str {
        self.auth_path.as_deref().unwrap_or("auth.json")
    }
    pub fn config_path(&self) -> &str {
        self.config_path.as_deref().unwrap_or("config.toml")
    }
    pub fn sync_type(&self) -> &str {
        self.sync_type.as_deref().unwrap_or("custom")
    }
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct EnvVarMapping {
    pub vcc_type: String,
    pub api_key: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
}
#[derive(Deserialize, Debug, Clone)]
pub(crate) struct ModelEnvMapping {
    pub role: String,
    pub env_var: String,
}
#[derive(Deserialize, Debug, Clone)]
pub(crate) struct TypeValueMapping {
    pub vcc: String,
    pub tool: String,
}
#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct PromptMappingConfig {
    pub path: Option<String>,
    pub sync_name: Option<String>,
}

impl PromptMappingConfig {
    pub fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("AGENTS.md")
    }
}

#[derive(Deserialize, Debug, Clone)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
pub(crate) struct Capabilities {
    #[serde(default)]
    pub defaults: Vec<String>,
    #[serde(default)]
    pub skill_mode: String,
    #[serde(default)]
    pub hook_enabled: bool,
    #[serde(default)]
    pub env_enabled: bool,
    #[serde(default)]
    pub model_format: String,
}

impl Capabilities {
    pub fn skill_disabled(&self) -> bool {
        self.skill_mode == "disabled"
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            defaults: Vec::new(),
            skill_mode: "cache_only".into(),
            hook_enabled: false,
            env_enabled: false,
            model_format: String::new(),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
pub(crate) struct HookMappingConfig {
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub format: String,
    pub path: Option<String>,
    #[serde(default)]
    pub section_key: String,
    /// JSON key for the command field in a hook entry (default: "command")
    #[serde(default = "default_hook_command_key")]
    pub command_key: String,
    /// JSON key for the matcher field in a hook group entry (default: "matcher")
    #[serde(default = "default_hook_matcher_key")]
    pub matcher_key: String,
    /// JSON key for the hooks array in a matcher group (default: "hooks")
    #[serde(default = "default_hook_hooks_key")]
    pub hooks_key: String,
}

fn default_hook_command_key() -> String {
    "command".into()
}
fn default_hook_matcher_key() -> String {
    "matcher".into()
}
fn default_hook_hooks_key() -> String {
    "hooks".into()
}

impl HookMappingConfig {
    pub fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("config.toml")
    }
}

impl Default for HookMappingConfig {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            format: String::new(),
            path: None,
            section_key: "hooks".into(),
            command_key: default_hook_command_key(),
            matcher_key: default_hook_matcher_key(),
            hooks_key: default_hook_hooks_key(),
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
pub(crate) struct EnvMappingConfig {
    #[serde(default)]
    pub exclude_keys: Vec<String>,
    pub section_key: Option<String>,
    pub path: Option<String>,
    #[serde(default)]
    pub format: String,
}

impl EnvMappingConfig {
    pub fn section_key(&self) -> &str {
        self.section_key.as_deref().unwrap_or("env")
    }
    /// Returns the env config file format. Defaults to "json" when empty.
    pub fn format_str(&self) -> &str {
        if self.format.is_empty() {
            "json"
        } else {
            &self.format
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
// Deserialize: fields parsed from external data, not all used
#[allow(dead_code)]
pub(crate) struct SessionMappingConfig {
    #[serde(default)]
    pub format: String,
    pub path: Option<String>,
    #[serde(default)]
    pub glob: String,
    #[serde(default)]
    pub exclude_prefix: String,
    pub resume_command: Option<String>,
    /// 通用 JSONL 解析配置（Qwen, Pi, OpenClaw, Copilot 等）
    #[serde(default)]
    pub jsonl: Option<JsonlParseConfig>,
    /// 通用 JSON 解析配置（Droid, Mux, Amp 等）
    #[serde(default)]
    pub json: Option<JsonParseConfig>,
    /// 通用 SQLite 解析配置（Hermes, Kilo, Crush 等）
    #[serde(default)]
    pub sqlite: Option<SqliteParseConfig>,
    /// 数据目录解析策略：home（默认），xdg_data，env_var
    #[serde(default)]
    pub data_dir_strategy: String,
    /// data_dir_strategy = "env_var" 时的环境变量名
    pub data_dir_env_var: Option<String>,
    /// data_dir_strategy = "env_var" 时的回退相对路径
    pub data_dir_fallback: Option<String>,
}

/// 通用 JSONL 解析配置
/// 驱动 jsonl_generic 管线：逐行读取 JSONL，按 filter 过滤，按 token_map 提取字段
#[derive(Deserialize, Debug, Clone, Default)]
#[allow(dead_code)]
pub(crate) struct JsonlParseConfig {
    /// 行过滤条件 {field: value}，AND 逻辑
    /// 例如：{"type": "assistant"} 或 {"type": "message", "role": "assistant"}
    #[serde(default)]
    pub filter: HashMap<String, String>,
    /// 模型名字段的点分路径（如 "model", "message.model"）
    pub model_path: Option<String>,
    /// 默认模型名
    #[serde(default)]
    pub default_model: String,
    /// TokenUsage 字段映射 {vcc_field: json_path}
    /// vcc_field: input, output, cache_read, cache_creation, reasoning
    /// json_path: 点分路径（如 "usageMetadata.promptTokenCount"）
    #[serde(default)]
    pub token_map: HashMap<String, String>,
    /// 费用字段的点分路径（如 "cost.total"）
    pub cost_path: Option<String>,
    /// 时间戳字段的点分路径（如 "timestamp"）
    pub timestamp_path: Option<String>,
    /// Session ID 字段路径（用于第一行 session 检测，如 Pi 的 "id"）
    pub session_id_path: Option<String>,
    /// 标题字段路径
    pub title_path: Option<String>,
    /// 项目目录字段路径（如 Pi 的 "cwd"）
    pub cwd_path: Option<String>,
    /// 是否从 input 中减去 cache_read（Copilot OTEL 归一化）
    #[serde(default)]
    pub normalize_cache_read: bool,
    /// 模型变更事件的过滤条件（OpenClaw: {type = "custom", customType = "model-snapshot"}）
    #[serde(default)]
    pub model_change_filter: HashMap<String, String>,
    /// 模型变更行中模型名的路径（如 "data.modelId"）
    pub model_change_path: Option<String>,
    /// 去重键字段路径（如 Copilot 的 "traceId:spanId"）
    pub dedup_key_paths: Option<String>,
}

/// 通用 JSON 解析配置
/// 驱动 json_generic 管线：读取 JSON 文件，按策略提取 usage
#[derive(Deserialize, Debug, Clone, Default)]
#[allow(dead_code)]
pub(crate) struct JsonParseConfig {
    /// 解析策略："flat"（单条记录）, "messages"（迭代消息数组）, "by_model"（迭代模型哈希表）
    #[serde(default)]
    pub strategy: String,
    /// Session ID 字段路径
    pub session_id_path: Option<String>,
    /// 标题字段路径
    pub title_path: Option<String>,
    /// 创建时间字段路径
    pub created_at_path: Option<String>,
    /// 最后活跃时间字段路径
    pub last_active_at_path: Option<String>,
    /// 项目目录字段路径
    pub project_dir_path: Option<String>,
    /// 消息数组路径（如 "messages"）
    pub messages_path: Option<String>,
    /// 消息过滤条件 {field: value}
    #[serde(default)]
    pub message_filter: HashMap<String, String>,
    /// 消息内模型名字段路径
    pub model_path: Option<String>,
    /// 默认模型名
    #[serde(default)]
    pub default_model: String,
    /// 消息内 TokenUsage 字段映射 {vcc_field: json_path}
    #[serde(default)]
    pub token_map: HashMap<String, String>,
    /// 消息内费用字段路径
    pub cost_path: Option<String>,
    /// 消息内时间戳字段路径
    pub timestamp_path: Option<String>,
    /// byModel 哈希表路径（Mux: "byModel"）
    pub by_model_path: Option<String>,
    /// byModel 策略：模型名是否含 provider 前缀（如 "anthropic:claude-opus-4-6"）
    #[serde(default)]
    pub by_model_provider_prefix: bool,
    /// byModel 每个 bucket 内 token 字段映射
    #[serde(default)]
    pub by_model_token_map: HashMap<String, String>,
    /// byModel 每个 bucket 内 cost 字段路径
    pub by_model_cost_path: Option<String>,
    /// byModel 最后请求时间字段路径
    pub by_model_timestamp_path: Option<String>,
    /// 模型名归一化规则（如 Droid: 去中括号、转小写、点转横杠）
    #[serde(default)]
    pub model_normalize: String,
}

/// 通用 SQLite 解析配置
/// 驱动 sqlite_generic 管线：打开 SQLite，执行配置的 SQL 查询
#[derive(Deserialize, Debug, Clone, Default)]
#[allow(dead_code)]
pub(crate) struct SqliteParseConfig {
    /// 会话列表 SQL（用于 scan_sessions）
    pub session_query: Option<String>,
    /// 会话查询列映射
    pub session_columns: Option<SessionColumnMap>,
    /// 使用量查询 SQL（支持 {session_id} 占位符）
    pub usage_query: Option<String>,
    /// 列名映射 {vcc_field: column_name}
    #[serde(default)]
    pub column_map: HashMap<String, String>,
    /// 模型名列名
    pub model_column: Option<String>,
    /// 默认模型名
    #[serde(default)]
    pub default_model: String,
    /// 费用列名（优先）
    pub cost_column: Option<String>,
    /// 费用列名（备选）
    pub cost_column_fallback: Option<String>,
    /// 是否使用 JSON 列提取 tokens（如 Kilo 的 data 列）
    #[serde(default)]
    pub json_column: Option<String>,
    /// JSON 列内的 token 字段映射
    #[serde(default)]
    pub json_token_map: HashMap<String, String>,
    /// JSON 列内的模型名字段路径
    pub json_model_path: Option<String>,
    /// JSON 列内的费用字段路径
    pub json_cost_path: Option<String>,
    /// 项目注册表文件（如 Crush 的 projects.json）
    pub projects_registry: Option<String>,
    /// 递增查询的排序列（用于增量缓存）
    pub incremental_column: Option<String>,
}

/// 会话查询列映射
#[derive(Deserialize, Debug, Clone, Default)]
#[allow(dead_code)]
pub(crate) struct SessionColumnMap {
    pub id: String,
    pub title: Option<String>,
    pub project_dir: Option<String>,
    pub created_at: Option<String>,
    pub last_active_at: Option<String>,
    pub model: Option<String>,
}

impl SessionMappingConfig {
    pub fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("projects")
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct AgentMappingConfig {
    pub path: Option<String>,
    #[serde(default)]
    pub format: String,
}

impl AgentMappingConfig {
    pub fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("agents")
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct SkillMappingConfig {
    pub path: Option<String>,
}

impl SkillMappingConfig {
    pub fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("skills")
    }
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct PluginMappingConfig {
    #[serde(default)]
    pub format: String,
    pub path: Option<String>,
    pub plugins_key: Option<String>,
    #[serde(default = "default_plugin_disabled_key")]
    pub disabled_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_file: Option<String>,
    pub install_dir: Option<String>,
    pub marketplace_key: Option<String>,
    /// Path to the enablement file (e.g. "extension-enablement.json" for gemini)
    pub enablement_path: Option<String>,
    /// Default install method for synced plugins (default: "symlink")
    #[serde(default = "default_install_method")]
    pub install_method: String,
}

fn default_install_method() -> String {
    "symlink".into()
}

impl Default for PluginMappingConfig {
    fn default() -> Self {
        Self {
            format: String::new(),
            path: None,
            plugins_key: None,
            disabled_key: default_plugin_disabled_key(),
            manifest_dir: None,
            manifest_file: None,
            install_dir: None,
            marketplace_key: None,
            enablement_path: None,
            install_method: default_install_method(),
        }
    }
}

impl PluginMappingConfig {
    pub fn disabled_key(&self) -> &str {
        if self.disabled_key.is_empty() {
            "disabled"
        } else {
            &self.disabled_key
        }
    }
    pub fn plugins_key(&self) -> &str {
        self.plugins_key.as_deref().unwrap_or("plugins")
    }
    pub fn enablement_path(&self) -> &str {
        self.enablement_path
            .as_deref()
            .unwrap_or("extension-enablement.json")
    }
    /// Array-format default path: "settings.json" for enabled_list, "opencode.json" for json_array.
    pub fn array_path(&self) -> &str {
        self.path
            .as_deref()
            .unwrap_or(if self.format == "enabled_list" {
                "settings.json"
            } else {
                "opencode.json"
            })
    }
    /// Array-format default plugins key: "enabledPlugins" for enabled_list, "plugin" for json_array.
    pub fn array_key(&self) -> &str {
        if self.plugins_key.is_some() {
            self.plugins_key()
        } else if self.format == "enabled_list" {
            "enabledPlugins"
        } else {
            "plugin"
        }
    }
    /// Map-format default path: "config.toml" for toml_table, "settings.json" for enabled_map.
    pub fn map_path(&self) -> &str {
        self.path
            .as_deref()
            .unwrap_or(if self.format == "toml_table" {
                "config.toml"
            } else {
                "settings.json"
            })
    }
    pub fn manifest_dir(&self) -> &str {
        self.manifest_dir.as_deref().unwrap_or("")
    }
    pub fn manifest_file(&self) -> &str {
        self.manifest_file.as_deref().unwrap_or("plugin.json")
    }
    pub fn install_dir(&self) -> &str {
        self.install_dir.as_deref().unwrap_or("plugins")
    }
}

impl ToolMapping {
    pub fn load(content: &str) -> Result<Self> {
        Ok(toml::from_str(content)?)
    }
    pub fn load_for_tool(tool_name: &str) -> Result<Self> {
        let content = crate::config::adapter_mapping_content(tool_name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", tool_name))?;
        Self::load(content)
    }
    pub fn resolved_config_dir(&self) -> Option<PathBuf> {
        // 支持 env_var 策略：先从环境变量获取路径
        if self.session.data_dir_strategy == "env_var" {
            if let Some(env_var) = &self.session.data_dir_env_var {
                if let Ok(val) = std::env::var(env_var) {
                    let p = PathBuf::from(&val);
                    if p.is_absolute() && p.exists() {
                        return Some(p);
                    }
                }
            }
            // 回退到默认路径
            if let Some(fallback) = &self.session.data_dir_fallback {
                let path = fallback.strip_prefix("~/").unwrap_or(fallback);
                return dirs::home_dir().map(|h| h.join(path));
            }
        }
        let path = self
            .tool
            .config_dir
            .strip_prefix("~/")
            .unwrap_or(&self.tool.config_dir);
        dirs::home_dir().map(|h| h.join(path))
    }
    pub fn require_config_dir(&self) -> anyhow::Result<PathBuf> {
        self.resolved_config_dir()
            .context("cannot find config directory")
    }
    /// Resolve the default settings file path (e.g. "settings.json") within the config dir.
    pub fn settings_file(&self) -> &str {
        self.settings_path.as_deref().unwrap_or("settings.json")
    }
}

pub(crate) fn mcp_to_tool_json(
    mapping: &McpMappingConfig,
    mcp: &McpServer,
    tool_name: &str,
) -> serde_json::Value {
    let tool_type = mapping
        .type_map
        .get(&mcp.config.server_type)
        .cloned()
        .unwrap_or_else(|| mcp.config.server_type.clone());
    let mut obj = serde_json::json!({});
    if !mapping.type_map.is_empty() && !mapping.skip_type_for.contains(&mcp.config.server_type) {
        let tf = mapping
            .field_map
            .get("type")
            .map(|s| s.as_str())
            .unwrap_or("type");
        obj[tf] = serde_json::json!(tool_type);
    }
    for (k, v) in &mapping.default_fields {
        obj[k] = v.clone();
    }
    let type_fields = mapping
        .type_field_map
        .get(&tool_type)
        .or_else(|| mapping.type_field_map.get(&mcp.config.server_type));
    match mcp.config.server_type.as_str() {
        "stdio" => {
            if mapping.command_format == "array" {
                let mut cv = vec![];
                if let Some(cmd) = &mcp.config.command {
                    cv.push(cmd.clone());
                }
                cv.extend(mcp.config.args.iter().cloned());
                obj[resolve_field_name(mapping, type_fields, "command")] = serde_json::json!(cv);
            } else {
                if let Some(cmd) = &mcp.config.command {
                    obj[resolve_field_name(mapping, type_fields, "command")] =
                        serde_json::json!(cmd);
                }
                if !mcp.config.args.is_empty() {
                    obj[resolve_field_name(mapping, type_fields, "args")] =
                        serde_json::json!(mcp.config.args);
                }
            }
            if !mcp.config.env.is_empty() {
                let eo: serde_json::Map<String, serde_json::Value> = mcp
                    .config
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), serde_json::json!(v)))
                    .collect();
                obj[&mapping.env_field] = serde_json::Value::Object(eo);
            }
        }
        "sse" | "streamable-http" => {
            if let Some(url) = &mcp.config.url {
                obj[resolve_field_name(mapping, type_fields, "url")] = serde_json::json!(url);
            }
            if !mcp.config.headers.is_empty() {
                let hs: serde_json::Map<String, serde_json::Value> = mcp
                    .config
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), serde_json::json!(v)))
                    .collect();
                obj[&resolve_field_name(mapping, type_fields, "headers")] =
                    serde_json::Value::Object(hs);
            }
            if !mcp.config.env.is_empty() {
                let eo: serde_json::Map<String, serde_json::Value> = mcp
                    .config
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), serde_json::json!(v)))
                    .collect();
                obj[&mapping.env_field] = serde_json::Value::Object(eo);
            }
        }
        _ => {}
    }
    if !mcp.config.disabled_tools.is_empty() {
        obj[&mapping.disabled_tools_field] = serde_json::json!(mcp.config.disabled_tools);
    }
    if !mcp.config.extra.is_empty() {
        if let Some(m) = obj.as_object_mut() {
            merge_extra_to_json_with_map(m, &mcp.config.extra, &mapping.extra_field_map);
        }
    }
    if let Some(o) = mcp.tool.get(tool_name) {
        if !o.extra.is_empty() {
            if let Some(m) = obj.as_object_mut() {
                merge_extra_to_json_with_map(m, &o.extra, &mapping.extra_field_map);
            }
        }
    }
    obj
}

/// Infer server type from config keys: command → stdio, url → sse, else None.
fn infer_server_type(config: &serde_json::Value) -> Option<String> {
    if config.get("command").is_some() {
        Some("stdio".into())
    } else if config.get("url").is_some() {
        Some("sse".into())
    } else {
        None
    }
}

pub(crate) fn tool_json_to_mcp(
    mapping: &McpMappingConfig,
    name: &str,
    config: &serde_json::Value,
    tool_name: &str,
) -> Option<McpServer> {
    let (server_type, type_fields) = if !mapping.type_map.is_empty() {
        let type_field = mapping
            .field_map
            .get("type")
            .map(|s| s.as_str())
            .unwrap_or("type");
        let tool_type = config
            .get(type_field)
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if tool_type.is_empty() {
            (infer_server_type(config)?, None)
        } else {
            // Resolve tool type → VCC type. When multiple VCC types map to the same
            // tool type (e.g. sse and streamable-http both → "remote"), prefer "sse"
            // as the more common default, then fall back to the first match in type_map.
            let vt = mapping
                .type_map
                .iter()
                .filter(|(_, v)| v.as_str() == tool_type)
                .map(|(k, _)| k.as_str())
                .find(|k| *k == "sse")
                .or_else(|| {
                    mapping
                        .type_map
                        .iter()
                        .find(|(_, v)| v.as_str() == tool_type)
                        .map(|(k, _)| k.as_str())
                })
                .unwrap_or(tool_type);
            (
                vt.to_string(),
                mapping
                    .type_field_map
                    .get(tool_type)
                    .or_else(|| mapping.type_field_map.get(vt)),
            )
        }
    } else {
        (infer_server_type(config)?, None)
    };
    let disabled_tools = extract_disabled_tools(mapping, config);
    let command_field = resolve_reverse_field(mapping, type_fields, "command")
        .unwrap_or_else(|| "command".to_string());
    let args_field =
        resolve_reverse_field(mapping, type_fields, "args").unwrap_or_else(|| "args".to_string());
    let mcp_config = match server_type.as_str() {
        "stdio" => {
            if mapping.command_format == "array" {
                let ca = json_str_array(config, &command_field);
                let (command, args) = if ca.is_empty() {
                    (None, vec![])
                } else {
                    (Some(ca[0].clone()), ca[1..].to_vec())
                };
                McpConfig {
                    server_type: server_type.clone(),
                    command,
                    args,
                    env: json_str_map(config, &mapping.env_field),
                    disabled_tools: vec![],
                    extra: extract_extra(mapping, config, &server_type),
                    ..Default::default()
                }
            } else {
                McpConfig {
                    server_type: server_type.clone(),
                    command: config
                        .get(&command_field)
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    args: json_str_array(config, &args_field),
                    env: json_str_map(config, &mapping.env_field),
                    disabled_tools: vec![],
                    extra: extract_extra(mapping, config, &server_type),
                    ..Default::default()
                }
            }
        }
        "sse" | "streamable-http" => McpConfig {
            server_type: server_type.clone(),
            url: resolve_reverse_field(mapping, type_fields, "url")
                .and_then(|f| {
                    config
                        .get(&f)
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .or_else(|| {
                    config
                        .get("url")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                }),
            headers: json_str_map(
                config,
                &resolve_reverse_field(mapping, type_fields, "headers")
                    .unwrap_or_else(|| mapping.headers_field.clone()),
            ),
            env: json_str_map(config, &mapping.env_field),
            disabled_tools: vec![],
            extra: extract_extra(mapping, config, &server_type),
            ..Default::default()
        },
        _ => return None,
    };
    let mut tool_extra: HashMap<String, toml::Value> = HashMap::new();
    if !mapping.extra_field_map.is_empty() {
        let rf: HashMap<&str, &str> = mapping
            .extra_field_map
            .iter()
            .map(|(k, v)| (v.as_str(), k.as_str()))
            .collect();
        if let Some(obj) = config.as_object() {
            for (jk, val) in obj {
                if let Some(vk) = rf.get(jk.as_str()) {
                    tool_extra.insert(vk.to_string(), json_to_toml_value(val));
                }
            }
        }
    }
    let mut tool_overrides = HashMap::new();
    if !tool_extra.is_empty() || !disabled_tools.is_empty() {
        tool_overrides.insert(
            tool_name.to_string(),
            crate::model::mcp::McpToolOverride {
                extra: tool_extra,
                disabled_tools: disabled_tools.clone(),
                ..Default::default()
            },
        );
    }
    // config.disabled_tools stays empty — disabledTools is per-tool, stored in tool overrides
    let mcp_config = McpConfig {
        disabled_tools: vec![],
        ..mcp_config
    };
    Some(build_mcp_server(
        name,
        mcp_config,
        tool_name,
        tool_overrides,
    ))
}

pub(crate) fn map_provider_type(type_map: &[TypeValueMapping], vcc_type: &str) -> String {
    type_map
        .iter()
        .find(|m| m.vcc == vcc_type)
        .map(|m| m.tool.clone())
        .unwrap_or_else(|| vcc_type.to_string())
}
pub(crate) fn unmap_provider_type(type_map: &[TypeValueMapping], tool_type: &str) -> String {
    type_map
        .iter()
        .find(|m| m.tool == tool_type)
        .map(|m| m.vcc.clone())
        .unwrap_or_else(|| tool_type.to_string())
}

fn resolve_reverse_field(
    mapping: &McpMappingConfig,
    type_fields: Option<&HashMap<String, String>>,
    vcc_field: &str,
) -> Option<String> {
    type_fields
        .and_then(|tf| tf.get(vcc_field).cloned())
        .or_else(|| mapping.field_map.get(vcc_field).cloned())
}
fn resolve_field_name(
    mapping: &McpMappingConfig,
    type_fields: Option<&HashMap<String, String>>,
    vcc_field: &str,
) -> String {
    resolve_reverse_field(mapping, type_fields, vcc_field).unwrap_or_else(|| vcc_field.to_string())
}
fn extract_disabled_tools(mapping: &McpMappingConfig, config: &serde_json::Value) -> Vec<String> {
    config
        .get(&mapping.disabled_tools_field)
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}
fn extract_extra(
    mapping: &McpMappingConfig,
    config: &serde_json::Value,
    server_type: &str,
) -> HashMap<String, toml::Value> {
    let obj = match config.as_object() {
        Some(o) => o,
        None => return HashMap::new(),
    };
    let mut all_known: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for v in mapping.type_map.values() {
        all_known.insert(v);
    }
    for v in mapping.field_map.values() {
        all_known.insert(v);
    }
    for fields in mapping.type_field_map.values() {
        for v in fields.values() {
            all_known.insert(v);
        }
    }
    all_known.extend(
        [
            "command",
            "args",
            "url",
            "headers",
            &mapping.headers_field,
            &mapping.env_field,
            &mapping.disabled_tools_field,
        ]
        .iter()
        .copied(),
    );
    for v in mapping.extra_field_map.values() {
        all_known.insert(v);
    }
    for k in mapping.default_fields.keys() {
        all_known.insert(k);
    }
    if let Some(keys) = mapping.known_keys.get(server_type) {
        for k in keys {
            all_known.insert(k);
        }
    }
    obj.iter()
        .filter(|(k, _)| !all_known.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), json_to_toml_value(v)))
        .collect()
}

pub(crate) fn mcp_to_toml_value(mapping: &McpMappingConfig, mcp: &McpServer) -> toml::Value {
    let mut table = toml::map::Map::new();
    match mcp.config.server_type.as_str() {
        "stdio" => {
            if let Some(cmd) = &mcp.config.command {
                table.insert("command".into(), toml::Value::String(cmd.clone()));
            }
            if !mcp.config.args.is_empty() {
                table.insert(
                    "args".into(),
                    toml::Value::Array(
                        mcp.config
                            .args
                            .iter()
                            .map(|a| toml::Value::String(a.clone()))
                            .collect(),
                    ),
                );
            }
            if !mcp.config.env.is_empty() {
                let mut et = toml::map::Map::new();
                for (k, v) in &mcp.config.env {
                    et.insert(k.clone(), toml::Value::String(v.clone()));
                }
                table.insert(mapping.env_field.clone(), toml::Value::Table(et));
            }
        }
        "sse" | "streamable-http" => {
            if let Some(url) = &mcp.config.url {
                table.insert("url".into(), toml::Value::String(url.clone()));
            }
            if !mcp.config.headers.is_empty() {
                let mut ht = toml::map::Map::new();
                for (k, v) in &mcp.config.headers {
                    ht.insert(k.clone(), toml::Value::String(v.clone()));
                }
                table.insert(mapping.headers_field.clone(), toml::Value::Table(ht));
            }
            if !mcp.config.env.is_empty() {
                let mut et = toml::map::Map::new();
                for (k, v) in &mcp.config.env {
                    et.insert(k.clone(), toml::Value::String(v.clone()));
                }
                table.insert(mapping.env_field.clone(), toml::Value::Table(et));
            }
        }
        _ => {}
    }
    if !mcp.config.disabled_tools.is_empty() {
        table.insert(
            mapping.disabled_tools_field.clone(),
            toml::Value::Array(
                mcp.config
                    .disabled_tools
                    .iter()
                    .map(|t| toml::Value::String(t.clone()))
                    .collect(),
            ),
        );
    }
    for (k, v) in &mcp.config.extra {
        table.insert(k.clone(), v.clone());
    }
    toml::Value::Table(table)
}

pub(crate) fn mcp_from_toml(
    mapping: &McpMappingConfig,
    name: &str,
    val: &toml::Value,
    tool_name: &str,
) -> Option<McpServer> {
    let table = val.as_table()?;
    let disabled_tools = extract_toml_string_vec(table, &mapping.disabled_tools_field);
    let mcp_config = if table.get("command").is_some() {
        let command = table
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let args: Vec<String> = table
            .get("args")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let env = toml_str_map(table, &mapping.env_field);
        McpConfig {
            server_type: "stdio".into(),
            command,
            args,
            env,
            disabled_tools: vec![],
            extra: extract_toml_extra(table, &mapping.toml_known_keys),
            ..Default::default()
        }
    } else if table.get("url").is_some() {
        let url = table
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let headers = toml_str_map(table, &mapping.headers_field);
        let env = toml_str_map(table, &mapping.env_field);
        McpConfig {
            server_type: "streamable-http".into(),
            url,
            headers,
            env,
            disabled_tools: vec![],
            extra: extract_toml_extra(table, &mapping.toml_known_keys),
            ..Default::default()
        }
    } else {
        return None;
    };
    // disabled_tools is per-tool, stored in tool overrides not config
    let mut tool_overrides = HashMap::new();
    if !disabled_tools.is_empty() {
        tool_overrides.insert(
            tool_name.to_string(),
            crate::model::mcp::McpToolOverride {
                disabled_tools,
                ..Default::default()
            },
        );
    }
    Some(build_mcp_server(
        name,
        mcp_config,
        tool_name,
        tool_overrides,
    ))
}

fn extract_toml_string_vec(table: &toml::map::Map<String, toml::Value>, key: &str) -> Vec<String> {
    table
        .get(key)
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}
fn extract_toml_extra(
    table: &toml::map::Map<String, toml::Value>,
    known_keys: &[String],
) -> HashMap<String, toml::Value> {
    let ks: std::collections::HashSet<&str> = known_keys.iter().map(|s| s.as_str()).collect();
    table
        .iter()
        .filter(|(k, _)| !ks.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── map_provider_type / unmap_provider_type ──

    #[test]
    fn test_map_provider_type_found() {
        let map = vec![TypeValueMapping {
            vcc: "openai".into(),
            tool: "openai-compatible".into(),
        }];
        assert_eq!(map_provider_type(&map, "openai"), "openai-compatible");
    }

    #[test]
    fn test_map_provider_type_not_found() {
        let map = vec![TypeValueMapping {
            vcc: "openai".into(),
            tool: "openai-compatible".into(),
        }];
        assert_eq!(map_provider_type(&map, "anthropic"), "anthropic");
    }

    #[test]
    fn test_unmap_provider_type_found() {
        let map = vec![TypeValueMapping {
            vcc: "openai".into(),
            tool: "openai-compatible".into(),
        }];
        assert_eq!(unmap_provider_type(&map, "openai-compatible"), "openai");
    }

    #[test]
    fn test_unmap_provider_type_not_found() {
        let map = vec![TypeValueMapping {
            vcc: "openai".into(),
            tool: "openai-compatible".into(),
        }];
        assert_eq!(unmap_provider_type(&map, "anthropic"), "anthropic");
    }

    // ── infer_server_type ──

    #[test]
    fn test_skill_disabled() {
        let mut caps = Capabilities::default();
        assert!(!caps.skill_disabled()); // default "cache_only" is not disabled
        caps.skill_mode = "disabled".into();
        assert!(caps.skill_disabled());
        caps.skill_mode = "full".into();
        assert!(!caps.skill_disabled());
    }

    #[test]
    fn test_env_format_str() {
        let mut env = EnvMappingConfig::default();
        assert_eq!(env.format_str(), "json"); // default
        env.format = "toml".into();
        assert_eq!(env.format_str(), "toml");
        env.format = String::new();
        assert_eq!(env.format_str(), "json"); // empty falls back to json
    }

    #[test]
    fn test_hook_mapping_config_defaults() {
        let hook = HookMappingConfig::default();
        assert_eq!(hook.command_key, "command");
        assert_eq!(hook.matcher_key, "matcher");
        assert_eq!(hook.hooks_key, "hooks");
        assert_eq!(hook.section_key, "hooks");
        assert!(hook.events.is_empty());
        assert!(hook.format.is_empty());
    }

    #[test]
    fn test_mcp_is_url_type() {
        let mcp = McpMappingConfig::default();
        assert!(mcp.is_url_type("sse"));
        assert!(mcp.is_url_type("streamable-http"));
        assert!(!mcp.is_url_type("stdio"));
        // Custom url_types
        let custom = McpMappingConfig {
            url_types: vec!["custom".into()],
            ..Default::default()
        };
        assert!(custom.is_url_type("custom"));
        assert!(!custom.is_url_type("sse"));
    }

    #[test]
    fn test_skill_path_default() {
        let skill = SkillMappingConfig::default();
        assert_eq!(skill.path(), "skills");
    }

    #[test]
    fn test_infer_server_type_command() {
        let config = serde_json::json!({"command": "npx"});
        assert_eq!(infer_server_type(&config), Some("stdio".into()));
    }

    #[test]
    fn test_infer_server_type_url() {
        let config = serde_json::json!({"url": "http://localhost:8080"});
        assert_eq!(infer_server_type(&config), Some("sse".into()));
    }

    #[test]
    fn test_infer_server_type_none() {
        let config = serde_json::json!({"other": "value"});
        assert_eq!(infer_server_type(&config), None);
    }

    // ── mcp_to_toml_value / mcp_from_toml ──

    #[test]
    fn test_mcp_to_toml_stdio() {
        let mcp = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                args: vec!["-y".into(), "@anthropic/fs".into()],
                env: HashMap::from([("KEY".into(), "VAL".into())]),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let val = mcp_to_toml_value(&McpMappingConfig::default(), &mcp);
        let table = val.as_table().unwrap();
        assert_eq!(table.get("command").unwrap().as_str(), Some("npx"));
        assert_eq!(table.get("args").unwrap().as_array().unwrap().len(), 2);
        assert!(table
            .get("env")
            .unwrap()
            .as_table()
            .unwrap()
            .contains_key("KEY"));
    }

    #[test]
    fn test_mcp_to_toml_sse() {
        let mcp = McpServer {
            name: "remote".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "sse".into(),
                url: Some("http://localhost:8080".into()),
                headers: HashMap::from([("Authorization".into(), "Bearer xyz".into())]),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        // Default mapping uses headers_field = "headers"
        let val = mcp_to_toml_value(&McpMappingConfig::default(), &mcp);
        let table = val.as_table().unwrap();
        assert_eq!(
            table.get("url").unwrap().as_str(),
            Some("http://localhost:8080")
        );
        assert!(table
            .get("headers")
            .unwrap()
            .as_table()
            .unwrap()
            .contains_key("Authorization"));

        // Codex-style mapping uses headers_field = "http_headers"
        let codex_mapping = McpMappingConfig {
            headers_field: "http_headers".into(),
            ..Default::default()
        };
        let val2 = mcp_to_toml_value(&codex_mapping, &mcp);
        let table2 = val2.as_table().unwrap();
        assert!(table2
            .get("http_headers")
            .unwrap()
            .as_table()
            .unwrap()
            .contains_key("Authorization"));
    }

    #[test]
    fn test_mcp_toml_roundtrip_stdio() {
        let mapping = McpMappingConfig::default();
        let mcp = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                args: vec!["arg1".into()],
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let toml_val = mcp_to_toml_value(&mapping, &mcp);
        let parsed = mcp_from_toml(&mapping, "fs", &toml_val, "claude").unwrap();
        assert_eq!(parsed.name, "fs");
        assert_eq!(parsed.config.command, Some("npx".into()));
        assert_eq!(parsed.config.args, vec!["arg1"]);
    }

    #[test]
    fn test_mcp_toml_roundtrip_sse_with_env() {
        let mapping = McpMappingConfig::default();
        let mcp = McpServer {
            name: "remote".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "sse".into(),
                url: Some("http://localhost:8080".into()),
                headers: HashMap::from([("Authorization".into(), "Bearer xyz".into())]),
                env: HashMap::from([("API_KEY".into(), "secret".into())]),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let toml_val = mcp_to_toml_value(&mapping, &mcp);
        let parsed = mcp_from_toml(&mapping, "remote", &toml_val, "codex").unwrap();
        assert_eq!(parsed.name, "remote");
        assert_eq!(parsed.config.url, Some("http://localhost:8080".into()));
        assert_eq!(
            parsed.config.headers.get("Authorization").unwrap(),
            "Bearer xyz"
        );
        assert_eq!(parsed.config.env.get("API_KEY").unwrap(), "secret");
    }

    #[test]
    fn test_mcp_toml_roundtrip_sse_codex_mapping() {
        // Codex uses headers_field = "http_headers"
        let mapping = McpMappingConfig {
            headers_field: "http_headers".into(),
            toml_known_keys: vec![
                "command".into(),
                "args".into(),
                "url".into(),
                "http_headers".into(),
                "env".into(),
                "disabled_tools".into(),
            ],
            ..Default::default()
        };
        let mcp = McpServer {
            name: "remote".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "streamable-http".into(),
                url: Some("http://localhost:9090".into()),
                headers: HashMap::from([("X-Custom".into(), "value".into())]),
                env: HashMap::from([("TOKEN".into(), "abc".into())]),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let toml_val = mcp_to_toml_value(&mapping, &mcp);
        let parsed = mcp_from_toml(&mapping, "remote", &toml_val, "codex").unwrap();
        assert_eq!(parsed.config.url, Some("http://localhost:9090".into()));
        assert_eq!(parsed.config.headers.get("X-Custom").unwrap(), "value");
        assert_eq!(parsed.config.env.get("TOKEN").unwrap(), "abc");
        // Extra should be empty (all keys known)
        assert!(
            parsed.config.extra.is_empty(),
            "extra should be empty, got: {:?}",
            parsed.config.extra
        );
    }

    #[test]
    fn test_mcp_to_toml_disabled_tools_snake_case() {
        let mcp = McpServer {
            name: "test".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                args: vec![],
                disabled_tools: vec!["Read".into(), "Write".into()],
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let mapping = McpMappingConfig::default();
        let val = mcp_to_toml_value(&mapping, &mcp);
        let table = val.as_table().unwrap();
        // Field name is driven by mapping.disabled_tools_field (default: "disabled_tools")
        assert!(
            table.contains_key(&mapping.disabled_tools_field),
            "TOML should use '{}' from mapping config",
            mapping.disabled_tools_field
        );
        let dt = table
            .get(&mapping.disabled_tools_field)
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(dt.len(), 2);
    }

    #[test]
    fn test_mcp_to_toml_sse_with_env() {
        let mcp = McpServer {
            name: "remote".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "sse".into(),
                url: Some("http://localhost:8080".into()),
                env: HashMap::from([("API_KEY".into(), "secret".into())]),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let val = mcp_to_toml_value(&McpMappingConfig::default(), &mcp);
        let table = val.as_table().unwrap();
        assert!(table
            .get("env")
            .unwrap()
            .as_table()
            .unwrap()
            .contains_key("API_KEY"));
    }

    #[test]
    fn test_mcp_toml_roundtrip_disabled_tools() {
        let mapping = McpMappingConfig::default();
        let mcp = McpServer {
            name: "test".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                args: vec![],
                disabled_tools: vec!["Bash".into(), "Edit".into()],
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let toml_val = mcp_to_toml_value(&mapping, &mcp);
        let parsed = mcp_from_toml(&mapping, "test", &toml_val, "codex").unwrap();
        // disabled_tools is per-tool, stored in tool overrides
        assert_eq!(
            parsed
                .tool
                .get("codex")
                .map(|t| t.disabled_tools.clone())
                .unwrap_or_default(),
            vec!["Bash", "Edit"]
        );
        assert!(parsed.config.disabled_tools.is_empty());
    }

    // ── mcp_to_tool_json / tool_json_to_mcp ──

    #[test]
    fn test_mcp_to_tool_json_stdio() {
        let mapping = McpMappingConfig::default();
        let mcp = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                args: vec![],
                env: HashMap::new(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let json = mcp_to_tool_json(&mapping, &mcp, "claude");
        assert_eq!(json["command"], "npx");
    }

    #[test]
    fn test_tool_json_to_mcp_stdio() {
        let mapping = McpMappingConfig::default();
        let config = serde_json::json!({"command": "npx", "args": ["-y", "pkg"]});
        let mcp = tool_json_to_mcp(&mapping, "fs", &config, "claude").unwrap();
        assert_eq!(mcp.name, "fs");
        assert_eq!(mcp.config.command, Some("npx".into()));
        assert_eq!(mcp.config.args, vec!["-y", "pkg"]);
    }

    #[test]
    fn test_mcp_to_tool_json_sse_with_env() {
        let mapping = McpMappingConfig::default();
        let mcp = McpServer {
            name: "remote".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "sse".into(),
                url: Some("http://localhost:8080".into()),
                env: HashMap::from([
                    ("API_KEY".into(), "secret".into()),
                    ("REGION".into(), "us".into()),
                ]),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let json = mcp_to_tool_json(&mapping, &mcp, "claude");
        assert_eq!(json["url"], "http://localhost:8080");
        assert!(
            json.get("env").is_some(),
            "SSE MCP should include env field"
        );
        assert_eq!(json["env"]["API_KEY"], "secret");
        assert_eq!(json["env"]["REGION"], "us");
    }

    #[test]
    fn test_mcp_json_roundtrip_sse_with_env() {
        let mapping = McpMappingConfig::default();
        let mcp = McpServer {
            name: "remote".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "sse".into(),
                url: Some("http://localhost:8080".into()),
                env: HashMap::from([("API_KEY".into(), "secret".into())]),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let json = mcp_to_tool_json(&mapping, &mcp, "claude");
        let parsed = tool_json_to_mcp(&mapping, "remote", &json, "claude").unwrap();
        assert_eq!(parsed.name, "remote");
        assert_eq!(parsed.config.url, Some("http://localhost:8080".into()));
        assert_eq!(parsed.config.env.get("API_KEY").unwrap(), "secret");
    }

    #[test]
    fn test_mcp_json_roundtrip_sse_kimi_with_env() {
        // Kimi mapping with type_map
        let mapping = McpMappingConfig {
            type_map: HashMap::from([
                ("stdio".into(), "stdio".into()),
                ("sse".into(), "sse".into()),
                ("streamable-http".into(), "http".into()),
            ]),
            field_map: HashMap::from([("type".into(), "transport".into())]),
            skip_type_for: vec!["stdio".into()],
            ..Default::default()
        };
        let mcp = McpServer {
            name: "remote".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "sse".into(),
                url: Some("http://localhost:8080".into()),
                env: HashMap::from([("TOKEN".into(), "abc".into())]),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let json = mcp_to_tool_json(&mapping, &mcp, "kimi");
        assert_eq!(json["transport"], "sse");
        assert_eq!(json["env"]["TOKEN"], "abc");
        let parsed = tool_json_to_mcp(&mapping, "remote", &json, "kimi").unwrap();
        assert_eq!(parsed.config.server_type, "sse");
        assert_eq!(parsed.config.url, Some("http://localhost:8080".into()));
        assert_eq!(parsed.config.env.get("TOKEN").unwrap(), "abc");
    }

    #[test]
    fn test_mcp_to_tool_json_disabled_tools_camelcase() {
        let mapping = McpMappingConfig::default();
        let mcp = McpServer {
            name: "test".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                args: vec![],
                disabled_tools: vec!["Read".into(), "Write".into()],
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let json = mcp_to_tool_json(&mapping, &mcp, "kimi");
        // JSON format should use camelCase "disabledTools"
        assert!(
            json.get("disabledTools").is_some(),
            "JSON should use camelCase 'disabledTools'"
        );
        assert!(
            json.get("disabled_tools").is_none(),
            "JSON should NOT use snake_case 'disabled_tools'"
        );
    }

    // ── Bug #38: disabledTools should be stored in tool overrides, not config ──

    #[test]
    fn test_tool_json_to_mcp_disabled_tools_in_tool_overrides() {
        // When tool_json_to_mcp parses a JSON config with disabledTools,
        // it should store them in tool[tool_name].disabled_tools, NOT config.disabled_tools
        let mapping = McpMappingConfig::default();
        let config = serde_json::json!({
            "command": "npx",
            "args": ["mcp-server"],
            "disabledTools": ["Read", "Write"]
        });
        let mcp = tool_json_to_mcp(&mapping, "test-mcp", &config, "kimi").unwrap();
        assert!(
            mcp.config.disabled_tools.is_empty(),
            "config.disabled_tools should be empty — disabledTools is per-tool"
        );
        assert_eq!(
            mcp.tool
                .get("kimi")
                .map(|t| t.disabled_tools.clone())
                .unwrap_or_default(),
            vec!["Read", "Write"],
            "disabledTools should be stored in tool.kimi.disabled_tools"
        );
    }

    #[test]
    fn test_mcp_from_toml_disabled_tools_in_tool_overrides() {
        // When mcp_from_toml parses a TOML table with disabled_tools,
        // it should store them in tool[tool_name].disabled_tools, NOT config.disabled_tools
        let mapping = McpMappingConfig {
            disabled_tools_field: "disabledTools".into(),
            ..Default::default()
        };
        let table = toml::toml! {
            command = "npx"
            args = ["mcp-server"]
            disabledTools = ["Bash"]
        };
        let mcp = mcp_from_toml(&mapping, "test-mcp", &toml::Value::Table(table), "codex").unwrap();
        assert!(
            mcp.config.disabled_tools.is_empty(),
            "config.disabled_tools should be empty"
        );
        assert_eq!(
            mcp.tool
                .get("codex")
                .map(|t| t.disabled_tools.clone())
                .unwrap_or_default(),
            vec!["Bash"],
            "disabledTools should be stored in tool.codex.disabled_tools"
        );
    }

    #[test]
    fn test_tool_json_to_mcp_no_disabled_tools_no_tool_override() {
        // When there are no disabledTools and no tool_extra,
        // there should be no tool overrides entry
        let mapping = McpMappingConfig::default();
        let config = serde_json::json!({
            "command": "npx",
            "args": ["mcp-server"]
        });
        let mcp = tool_json_to_mcp(&mapping, "test-mcp", &config, "claude").unwrap();
        assert!(
            mcp.tool.is_empty(),
            "no tool overrides when no disabledTools and no extra"
        );
        assert!(mcp.config.disabled_tools.is_empty());
    }

    // ── McpMappingConfig defaults ──

    #[test]
    fn test_mcp_mapping_config_defaults() {
        let cfg = McpMappingConfig::default();
        assert_eq!(cfg.format, "json");
        assert_eq!(cfg.servers_key, "mcpServers");
        assert_eq!(cfg.disabled_key, "disabled");
        assert!(cfg.enabled_key.is_empty());
        assert!(!cfg.uses_enabled_semantic());
        assert_eq!(cfg.toggle_key(), "disabled");
        assert_eq!(cfg.command_format, "string_args");
        assert_eq!(cfg.path(), "settings.json");
    }

    #[test]
    fn test_mcp_mapping_config_toml_path() {
        let cfg = McpMappingConfig {
            format: "toml".into(),
            ..Default::default()
        };
        assert_eq!(cfg.path(), "config.toml");
    }

    #[test]
    fn test_mcp_mapping_config_enabled_semantic() {
        let cfg = McpMappingConfig {
            enabled_key: "enabled".into(),
            ..Default::default()
        };
        assert!(cfg.uses_enabled_semantic());
        assert_eq!(cfg.toggle_key(), "enabled");
    }

    #[test]
    fn test_mcp_mapping_config_disabled_semantic_is_default() {
        let cfg = McpMappingConfig::default();
        assert!(!cfg.uses_enabled_semantic());
        assert_eq!(cfg.toggle_key(), "disabled");
    }

    #[test]
    fn test_opencode_toml_parses_enabled_key() {
        let content = crate::config::adapter_mapping_content("opencode").unwrap();
        let mapping: ToolMapping = toml::from_str(content).unwrap();
        assert_eq!(mapping.mcp.enabled_key, "enabled");
        assert!(mapping.mcp.uses_enabled_semantic());
        assert_eq!(mapping.mcp.toggle_key(), "enabled");
    }

    #[test]
    fn test_claude_toml_uses_disabled_semantic() {
        let content = crate::config::adapter_mapping_content("claude").unwrap();
        let mapping: ToolMapping = toml::from_str(content).unwrap();
        assert!(mapping.mcp.enabled_key.is_empty());
        assert!(!mapping.mcp.uses_enabled_semantic());
        assert_eq!(mapping.mcp.toggle_key(), "disabled");
    }

    // ── PluginMappingConfig ──

    #[test]
    fn test_plugin_mapping_config_defaults() {
        let cfg = PluginMappingConfig::default();
        assert_eq!(cfg.plugins_key(), "plugins");
        assert_eq!(cfg.manifest_file(), "plugin.json");
        assert_eq!(cfg.install_dir(), "plugins");
        assert_eq!(cfg.install_method, "symlink");
        assert_eq!(cfg.disabled_key(), "disabled");
    }

    #[test]
    fn test_plugin_mapping_config_enabled_list() {
        let cfg = PluginMappingConfig {
            format: "enabled_list".into(),
            ..Default::default()
        };
        assert_eq!(cfg.array_path(), "settings.json");
        assert_eq!(cfg.array_key(), "enabledPlugins");
    }

    #[test]
    fn test_plugin_mapping_config_json_array() {
        let cfg = PluginMappingConfig {
            format: "json_array".into(),
            ..Default::default()
        };
        assert_eq!(cfg.array_path(), "opencode.json");
        assert_eq!(cfg.array_key(), "plugin");
    }

    #[test]
    fn test_plugin_mapping_config_toml_table() {
        let cfg = PluginMappingConfig {
            format: "toml_table".into(),
            ..Default::default()
        };
        assert_eq!(cfg.map_path(), "config.toml");
    }

    // ── extract_toml_string_vec ──

    #[test]
    fn test_extract_toml_string_vec_found() {
        let mut table = toml::map::Map::new();
        table.insert(
            "items".into(),
            toml::Value::Array(vec![
                toml::Value::String("a".into()),
                toml::Value::String("b".into()),
            ]),
        );
        let result = extract_toml_string_vec(&table, "items");
        assert_eq!(result, vec!["a", "b"]);
    }

    #[test]
    fn test_extract_toml_string_vec_missing() {
        let table = toml::map::Map::new();
        let result = extract_toml_string_vec(&table, "items");
        assert!(result.is_empty());
    }

    // ── extract_toml_extra ──

    #[test]
    fn test_extract_toml_extra_filters_known() {
        let mut table = toml::map::Map::new();
        table.insert("command".into(), toml::Value::String("npx".into()));
        table.insert("custom_field".into(), toml::Value::String("value".into()));
        let known = vec!["command".to_string()];
        let extra = extract_toml_extra(&table, &known);
        assert!(!extra.contains_key("command"));
        assert!(extra.contains_key("custom_field"));
    }

    // ── ToolMapping::settings_file ──

    #[test]
    fn test_tool_mapping_settings_file_default() {
        let tm = ToolMapping {
            tool: ToolInfo {
                name: "test".into(),
                config_dir: "~/test".into(),
            },
            settings_path: None,
            mcp: Default::default(),
            provider: Default::default(),
            prompt: Default::default(),
            capabilities: Default::default(),
            hook: Default::default(),
            env: Default::default(),
            session: Default::default(),
            agent: Default::default(),
            skill: Default::default(),
            plugin: Default::default(),
        };
        assert_eq!(tm.settings_file(), "settings.json");
    }

    // ── SkillMappingConfig ──

    #[test]
    fn test_skill_mapping_default_path() {
        let s = SkillMappingConfig::default();
        assert_eq!(s.path(), "skills");
    }

    #[test]
    fn test_skill_mapping_custom_path() {
        let s = SkillMappingConfig {
            path: Some("custom-skills".into()),
        };
        assert_eq!(s.path(), "custom-skills");
    }

    // ── McpMappingConfig::is_url_type ──

    #[test]
    fn test_mcp_is_url_type_default() {
        let m = McpMappingConfig::default();
        assert!(m.is_url_type("sse"));
        assert!(m.is_url_type("streamable-http"));
        assert!(!m.is_url_type("stdio"));
        assert!(!m.is_url_type("remote"));
    }

    #[test]
    fn test_mcp_is_url_type_custom() {
        let m = McpMappingConfig {
            url_types: vec!["sse".into()],
            ..Default::default()
        };
        assert!(m.is_url_type("sse"));
        assert!(!m.is_url_type("streamable-http"));
    }

    #[test]
    fn test_mcp_is_url_type_empty() {
        let m = McpMappingConfig {
            url_types: vec![],
            ..Default::default()
        };
        assert!(!m.is_url_type("sse"));
        assert!(!m.is_url_type("streamable-http"));
    }

    #[test]
    fn test_mcp_url_types_serde_default() {
        // When url_types is omitted from TOML, it should default to ["sse", "streamable-http"]
        let toml = r#"
format = "json"
servers_key = "mcp"
"#;
        let m: McpMappingConfig = toml::from_str(toml).unwrap();
        assert_eq!(m.url_types, vec!["sse", "streamable-http"]);
    }

    // ── extract_disabled_tools ──

    #[test]
    fn test_extract_disabled_tools() {
        let mapping = McpMappingConfig::default();
        let config = serde_json::json!({"disabledTools": ["tool1", "tool2"]});
        let tools = extract_disabled_tools(&mapping, &config);
        assert_eq!(tools, vec!["tool1", "tool2"]);
    }

    #[test]
    fn test_extract_disabled_tools_empty() {
        let mapping = McpMappingConfig::default();
        let config = serde_json::json!({});
        let tools = extract_disabled_tools(&mapping, &config);
        assert!(tools.is_empty());
    }

    // ── Platform-aware path configuration tests ──

    #[test]
    fn test_mcp_config_path_with_tilde() {
        // MCP config with tilde path should be preserved for later expansion
        let mcp = McpMappingConfig {
            path: Some(
                "~/Library/Application Support/Claude/claude_desktop_config.json".to_string(),
            ),
            ..Default::default()
        };
        assert!(mcp.path.as_ref().unwrap().starts_with("~"));
    }

    #[test]
    fn test_mcp_config_path_windows_style() {
        let mcp = McpMappingConfig {
            path: Some(
                "C:\\Users\\test\\AppData\\Roaming\\Claude\\claude_desktop_config.json".to_string(),
            ),
            ..Default::default()
        };
        assert!(mcp.path.as_ref().unwrap().contains("AppData"));
    }

    #[test]
    fn test_mcp_config_path_linux_xdg() {
        let mcp = McpMappingConfig {
            path: Some("~/.config/Claude/claude_desktop_config.json".to_string()),
            ..Default::default()
        };
        assert!(mcp.path.as_ref().unwrap().contains(".config"));
    }

    #[test]
    fn test_provider_config_path_cross_platform() {
        let prov = ProviderMappingConfig {
            path: Some("config.toml".to_string()),
            ..Default::default()
        };
        assert_eq!(prov.path.as_deref(), Some("config.toml"));
    }

    #[test]
    fn test_path_join_cross_platform() {
        // Verify that path joining works correctly on all platforms
        let base = std::path::PathBuf::from("/data");
        let result = base.join("settings.json");
        assert!(result.to_string_lossy().ends_with("settings.json"));
    }

    // ── infer_server_type cross-platform paths ──

    #[test]
    fn test_infer_server_type_command_wins_over_url() {
        // When both command and url are present, command takes precedence
        let config = serde_json::json!({"command": "npx", "url": "https://mcp.example.com"});
        assert_eq!(infer_server_type(&config), Some("stdio".to_string()));
    }

    #[test]
    fn test_infer_server_type_python_path_with_spaces() {
        // Windows paths may contain spaces
        let config = serde_json::json!({"command": "C:\\Program Files\\Python\\python.exe"});
        assert_eq!(infer_server_type(&config), Some("stdio".to_string()));
    }

    #[test]
    fn test_infer_server_type_unix_path() {
        let config = serde_json::json!({"command": "/usr/local/bin/mcp-server"});
        assert_eq!(infer_server_type(&config), Some("stdio".to_string()));
    }

    // ── Bug #7: extract_extra should exclude headers_field ──

    #[test]
    fn test_extract_extra_excludes_headers_field() {
        // kimi uses "custom_headers" as headers field in JSON
        let mapping = McpMappingConfig {
            headers_field: "custom_headers".into(),
            type_field_map: HashMap::from([(
                "sse".into(),
                HashMap::from([("headers".into(), "custom_headers".into())]),
            )]),
            ..Default::default()
        };
        let config = serde_json::json!({
            "transport": "sse",
            "url": "http://localhost:8080",
            "custom_headers": {"Authorization": "Bearer xyz"},
        });
        let extra = extract_extra(&mapping, &config, "sse");
        // custom_headers should NOT appear in extra since it's already parsed as headers
        assert!(
            !extra.contains_key("custom_headers"),
            "custom_headers should be excluded from extra (parsed as headers)"
        );
    }

    #[test]
    fn test_extract_extra_excludes_default_headers() {
        // Default mapping uses "headers" as headers_field
        let mapping = McpMappingConfig::default();
        let config = serde_json::json!({
            "url": "http://localhost:8080",
            "headers": {"Authorization": "Bearer xyz"},
            "custom_field": "value",
        });
        let extra = extract_extra(&mapping, &config, "sse");
        // "headers" should be excluded (it's the default headers_field)
        assert!(!extra.contains_key("headers"));
        // "custom_field" should remain in extra
        assert!(extra.contains_key("custom_field"));
    }

    // ── Bug #1: disabled_tools_field should be config-driven in TOML ──

    #[test]
    fn test_mcp_toml_disabled_tools_custom_field() {
        // gemini uses "excludeTools" as disabled_tools_field in JSON path
        // but in TOML path, verify that disabled_tools_field drives the key name
        let mapping = McpMappingConfig {
            disabled_tools_field: "excluded_tools".into(),
            ..Default::default()
        };
        let mcp = McpServer {
            name: "test".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                args: vec![],
                disabled_tools: vec!["Bash".into()],
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let val = mcp_to_toml_value(&mapping, &mcp);
        let table = val.as_table().unwrap();
        assert!(
            table.contains_key("excluded_tools"),
            "Should use mapping.disabled_tools_field as key"
        );
        assert!(
            !table.contains_key("disabled_tools"),
            "Should NOT use default 'disabled_tools' when mapping specifies different field"
        );

        // Roundtrip: parse back
        let parsed = mcp_from_toml(&mapping, "test", &val, "test-tool").unwrap();
        // disabled_tools is per-tool, stored in tool overrides
        assert_eq!(
            parsed
                .tool
                .get("test-tool")
                .map(|t| t.disabled_tools.clone())
                .unwrap_or_default(),
            vec!["Bash"]
        );
        assert!(parsed.config.disabled_tools.is_empty());
    }

    // ── extract_toml_extra should exclude headers_field and disabled_tools_field ──

    #[test]
    fn test_extract_toml_extra_with_custom_headers_field() {
        // Codex uses headers_field = "http_headers"
        let mapping = McpMappingConfig {
            headers_field: "http_headers".into(),
            disabled_tools_field: "disabled_tools".into(),
            toml_known_keys: vec![
                "command".into(),
                "args".into(),
                "url".into(),
                "http_headers".into(),
                "env".into(),
                "disabled_tools".into(),
            ],
            ..Default::default()
        };
        // Simulate TOML table for an SSE entry
        let mut table = toml::map::Map::new();
        table.insert("url".into(), toml::Value::String("http://localhost".into()));
        let mut headers = toml::map::Map::new();
        headers.insert("Auth".into(), toml::Value::String("Bearer x".into()));
        table.insert("http_headers".into(), toml::Value::Table(headers));
        table.insert("custom_data".into(), toml::Value::String("keep_me".into()));

        let extra = extract_toml_extra(&table, &mapping.toml_known_keys);
        // http_headers should be excluded (it's in toml_known_keys)
        assert!(!extra.contains_key("http_headers"));
        // custom_data should remain
        assert!(extra.contains_key("custom_data"));
    }

    // ── kimi custom_headers roundtrip: JSON SSE with field_map ──

    #[test]
    fn test_kimi_sse_headers_no_extra_duplication() {
        // Simulate kimi config: SSE with custom_headers
        let mapping = McpMappingConfig {
            format: "json".into(),
            servers_key: "mcpServers".into(),
            type_map: HashMap::from([
                ("stdio".into(), "stdio".into()),
                ("sse".into(), "sse".into()),
                ("streamable-http".into(), "http".into()),
            ]),
            field_map: HashMap::from([("type".into(), "transport".into())]),
            type_field_map: HashMap::from([(
                "sse".into(),
                HashMap::from([("headers".into(), "custom_headers".into())]),
            )]),
            ..Default::default()
        };

        // Build JSON config as kimi would have it
        let config = serde_json::json!({
            "transport": "sse",
            "url": "http://localhost:8080",
            "custom_headers": {"Authorization": "Bearer xyz"}
        });

        let mcp = tool_json_to_mcp(&mapping, "my-sse", &config, "kimi").unwrap();
        assert_eq!(mcp.config.server_type, "sse");
        assert_eq!(
            mcp.config.headers.get("Authorization"),
            Some(&"Bearer xyz".to_string())
        );
        // custom_headers should NOT be duplicated in extra
        assert!(
            !mcp.config.extra.contains_key("custom_headers"),
            "custom_headers should not appear in extra — already parsed as headers"
        );
    }

    #[test]
    fn test_kimi_sse_env_no_extra_duplication() {
        // Simulate kimi config: SSE with env
        let mapping = McpMappingConfig {
            format: "json".into(),
            servers_key: "mcpServers".into(),
            type_map: HashMap::from([("sse".into(), "sse".into())]),
            field_map: HashMap::from([("type".into(), "transport".into())]),
            ..Default::default()
        };

        let config = serde_json::json!({
            "transport": "sse",
            "url": "http://localhost:8080",
            "env": {"API_KEY": "secret123"}
        });

        let mcp = tool_json_to_mcp(&mapping, "my-sse", &config, "kimi").unwrap();
        assert_eq!(
            mcp.config.env.get("API_KEY"),
            Some(&"secret123".to_string())
        );
        // env should NOT be duplicated in extra
        assert!(
            !mcp.config.extra.contains_key("env"),
            "env should not appear in extra — already parsed as McpConfig.env"
        );
    }

    #[test]
    fn test_codex_sse_env_no_extra_duplication_toml() {
        // Codex TOML: SSE with env field
        let mapping = McpMappingConfig {
            format: "toml".into(),
            headers_field: "http_headers".into(),
            toml_known_keys: vec![
                "command".into(),
                "args".into(),
                "url".into(),
                "http_headers".into(),
                "env".into(),
                "disabled_tools".into(),
            ],
            ..Default::default()
        };

        let mut table = toml::map::Map::new();
        table.insert(
            "url".into(),
            toml::Value::String("http://localhost:8080".into()),
        );
        let mut h = toml::map::Map::new();
        h.insert("Auth".into(), toml::Value::String("Bearer x".into()));
        table.insert("http_headers".into(), toml::Value::Table(h));
        let mut e = toml::map::Map::new();
        e.insert("API_KEY".into(), toml::Value::String("secret".into()));
        table.insert("env".into(), toml::Value::Table(e));

        let val = toml::Value::Table(table);
        let mcp = mcp_from_toml(&mapping, "my-sse", &val, "codex").unwrap();
        assert_eq!(mcp.config.server_type, "streamable-http");
        assert_eq!(
            mcp.config.headers.get("Auth"),
            Some(&"Bearer x".to_string())
        );
        assert_eq!(mcp.config.env.get("API_KEY"), Some(&"secret".to_string()));
        // Neither http_headers nor env should appear in extra
        assert!(
            !mcp.config.extra.contains_key("http_headers"),
            "http_headers should not be in extra"
        );
        assert!(
            !mcp.config.extra.contains_key("env"),
            "env should not be in extra"
        );
    }

    // ── Finding 11: headers fallback should use mapping.headers_field ──

    #[test]
    fn test_tool_json_to_mcp_sse_headers_fallback_uses_headers_field() {
        // When type_field_map has no "headers" entry, the fallback should use
        // mapping.headers_field instead of hardcoded "headers"
        let mapping = McpMappingConfig {
            headers_field: "custom_headers".into(),
            type_map: HashMap::from([("sse".into(), "sse".into())]),
            field_map: HashMap::from([("type".into(), "transport".into())]),
            // No type_field_map for sse → resolve_reverse_field returns None for "headers"
            ..Default::default()
        };

        let config = serde_json::json!({
            "transport": "sse",
            "url": "http://localhost:8080",
            "custom_headers": {"Authorization": "Bearer xyz"}
        });

        let mcp = tool_json_to_mcp(&mapping, "test-sse", &config, "test-tool").unwrap();
        assert_eq!(
            mcp.config.headers.get("Authorization"),
            Some(&"Bearer xyz".to_string()),
            "Headers should be read from custom_headers when headers_field is set"
        );
        assert!(
            !mcp.config.extra.contains_key("custom_headers"),
            "custom_headers should not appear in extra"
        );
    }

    // ── Finding 17: plugin disabled_key should be config-driven ──

    #[test]
    fn test_plugin_disabled_key_default() {
        // derive Default gives empty string; accessor should fall back to "disabled"
        let config = PluginMappingConfig::default();
        assert_eq!(config.disabled_key(), "disabled");
    }

    #[test]
    fn test_plugin_disabled_key_custom() {
        let config = PluginMappingConfig {
            disabled_key: "isDisabled".into(),
            ..Default::default()
        };
        assert_eq!(config.disabled_key(), "isDisabled");
    }

    // ── kimi config: type_field_map resolves custom_headers ──

    #[test]
    fn test_kimi_type_field_map_sse_resolves_custom_headers() {
        // Simulate kimi.toml's type_field_map.sse: headers → custom_headers
        let mapping = McpMappingConfig {
            headers_field: "custom_headers".into(),
            type_map: HashMap::from([
                ("sse".into(), "sse".into()),
                ("streamable-http".into(), "http".into()),
            ]),
            field_map: HashMap::from([("type".into(), "transport".into())]),
            type_field_map: HashMap::from([
                (
                    "sse".into(),
                    HashMap::from([("headers".into(), "custom_headers".into())]),
                ),
                (
                    "http".into(),
                    HashMap::from([("headers".into(), "custom_headers".into())]),
                ),
            ]),
            ..Default::default()
        };

        // Write: SSE MCP should write headers as "custom_headers"
        let mcp = McpServer {
            name: "test-sse".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "sse".into(),
                url: Some("http://example.com/mcp".into()),
                headers: HashMap::from([("Authorization".into(), "Bearer xyz".into())]),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let json = mcp_to_tool_json(&mapping, &mcp, "kimi");
        assert!(
            json.get("custom_headers").is_some(),
            "SSE MCP should write headers as custom_headers for kimi"
        );
        assert!(
            json.get("headers").is_none(),
            "SSE MCP should NOT write 'headers' key for kimi"
        );

        // Read: custom_headers should be read back as headers
        let config = serde_json::json!({
            "transport": "sse",
            "url": "http://example.com/mcp",
            "custom_headers": {"Authorization": "Bearer xyz"}
        });
        let result = tool_json_to_mcp(&mapping, "test-sse", &config, "kimi").unwrap();
        assert_eq!(
            result.config.headers.get("Authorization"),
            Some(&"Bearer xyz".to_string()),
            "custom_headers should be read back into headers field"
        );
    }

    // ── disabled_tools_field config-driven resolution ──

    #[test]
    fn test_mcp_to_toml_uses_disabled_tools_field() {
        let mapping = McpMappingConfig {
            disabled_tools_field: "disabledTools".into(), // camelCase variant
            ..Default::default()
        };
        let mcp = McpServer {
            name: "test".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                disabled_tools: vec!["Read".into(), "Write".into()],
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let val = mcp_to_toml_value(&mapping, &mcp);
        let table = val.as_table().unwrap();
        assert!(
            table.contains_key("disabledTools"),
            "Should use mapping.disabled_tools_field as key"
        );
        assert!(
            !table.contains_key("disabled_tools"),
            "Should NOT use hardcoded snake_case key"
        );
    }

    #[test]
    fn test_mcp_from_toml_uses_disabled_tools_field() {
        let mapping = McpMappingConfig {
            disabled_tools_field: "disabledTools".into(),
            ..Default::default()
        };
        let table = toml::toml! {
            command = "npx"
            args = ["-y", "mcp-server"]
            disabledTools = ["Read", "Write"]
        };
        let mcp = mcp_from_toml(&mapping, "test", &toml::Value::Table(table), "test-tool").unwrap();
        // disabled_tools is per-tool, stored in tool overrides
        assert_eq!(
            mcp.tool
                .get("test-tool")
                .map(|t| t.disabled_tools.clone())
                .unwrap_or_default(),
            vec!["Read", "Write"],
            "Should read from mapping.disabled_tools_field key and store in tool overrides"
        );
        assert!(mcp.config.disabled_tools.is_empty());
    }

    // ── env_field config-driven resolution ──

    #[test]
    fn test_mcp_to_tool_json_uses_env_field() {
        let mapping = McpMappingConfig {
            env_field: "env_vars".into(),
            ..Default::default()
        };
        let mcp = McpServer {
            name: "test".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                env: HashMap::from([("API_KEY".into(), "sk-123".into())]),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let json = mcp_to_tool_json(&mapping, &mcp, "tool");
        assert!(
            json.get("env_vars").is_some(),
            "Should use mapping.env_field as key"
        );
        assert!(
            json.get("env").is_none(),
            "Should NOT use hardcoded 'env' key"
        );
    }
}
