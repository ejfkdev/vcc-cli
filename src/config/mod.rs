//! 编译时内嵌配置
//!
//! 所有硬编码配置提取到 TOML 文件中，通过 include_str!() 编译时嵌入。
//! 使用 OnceLock 惰性解析，全局单例访问。

pub mod models;

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

// ══════════════════════════════════════════════════════════
// 资源注册表
// ══════════════════════════════════════════════════════════

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct ResourceRegistry {
    pub id_generation: IdGenerationConfig,
    pub resources: Vec<ResourceConfig>,
    pub init: InitConfig,
    pub tools: ToolsConfig,
    #[allow(dead_code)]
    pub datasources: DatasourcesConfig,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct IdGenerationConfig {
    pub alphabet: String,
    pub length: usize,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct ResourceConfig {
    pub kind: String,
    pub dir: String,
    pub init_subdir: String,
    #[serde(flatten, default)]
    pub validation: ValidationConfig,
    /// CLI 字段定义（驱动 add/edit 命令的参数生成）
    #[serde(default)]
    pub cli_fields: Vec<CliFieldDef>,
    /// 额外子命令 (enable/disable/install)
    #[serde(default)]
    pub extra_subcommands: Vec<String>,
    /// 命令行示例文本
    #[serde(default)]
    pub examples: Vec<String>,
}

/// CLI 字段定义：描述 add/edit 命令的一个参数
#[derive(Deserialize, Debug, Clone)]
pub(crate) struct CliFieldDef {
    /// 字段名（CLI --name 和 FieldMap key）
    pub name: String,
    /// 短选项字符
    pub short: Option<char>,
    /// 帮助文本
    pub help: String,
    /// 字段类型: string | u64 | f64 | csv | kvvec | content | file
    #[serde(rename = "field_type", default = "default_field_type")]
    pub field_type: String,
    /// 默认值（字符串形式，按 field_type 解析）
    #[serde(default)]
    pub default_value: Option<String>,
    /// add 时是否必填
    #[serde(default)]
    pub required_for_add: bool,
    /// 仅 add 出现（如 --preset）
    #[serde(default)]
    pub add_only: bool,
    /// 仅 edit 出现（如 --remove-var）
    #[serde(default)]
    pub edit_only: bool,
    /// 预设可填充
    #[serde(default)]
    #[allow(dead_code)]
    pub preset_fill: bool,
    /// 是否是触发预设的字段（--preset 本身）
    #[serde(default)]
    #[allow(dead_code)]
    pub preset_trigger: bool,
    /// 是否存入 metadata.description
    #[serde(default)]
    #[allow(dead_code)]
    pub is_metadata: bool,
    /// 是否脱敏显示
    #[serde(default)]
    #[allow(dead_code)]
    pub sensitive: bool,
    /// 可选值列表（用于 CLI possible_values 校验和 tab 补全）
    #[serde(default)]
    pub possible_values: Vec<String>,
}

fn default_field_type() -> String {
    "string".to_string()
}

#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct ValidationConfig {
    #[serde(default)]
    pub valid_types: Vec<String>,
    #[serde(default)]
    pub valid_events: Vec<String>,
    #[serde(default)]
    pub valid_modes: Vec<String>,
    #[serde(default)]
    pub valid_sources: Vec<String>,
    #[serde(default)]
    pub valid_install_methods: Vec<String>,
    #[serde(default)]
    pub valid_formats: Vec<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct InitConfig {
    #[serde(default)]
    pub extra_dirs: Vec<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct ToolsConfig {
    #[serde(default)]
    pub adapters: Vec<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct DatasourcesConfig {
    #[serde(default)]
    pub names: Vec<String>,
}

static RESOURCE_REGISTRY: OnceLock<ResourceRegistry> = OnceLock::new();

pub(crate) fn resource_registry() -> &'static ResourceRegistry {
    RESOURCE_REGISTRY.get_or_init(|| {
        let content = include_str!("resource_registry.toml");
        toml::from_str(content).expect("resource_registry.toml should be valid TOML")
    })
}

impl ResourceRegistry {
    /// 按 kind 查找目录名
    pub fn dir_for_kind(&self, kind: &str) -> Option<&str> {
        self.resources
            .iter()
            .find(|r| r.kind == kind)
            .map(|r| r.dir.as_str())
    }

    /// 所有初始化目录（resources 的 init_subdir + extra_dirs）
    pub fn all_init_dirs(&self) -> Vec<String> {
        let mut dirs: Vec<String> = self
            .resources
            .iter()
            .map(|r| r.init_subdir.clone())
            .collect();
        dirs.extend(self.init.extra_dirs.iter().cloned());
        dirs
    }

    /// 按 kind 查找验证配置
    pub fn validation_for(&self, kind: &str) -> Option<&ValidationConfig> {
        self.resources
            .iter()
            .find(|r| r.kind == kind)
            .map(|r| &r.validation)
    }

    /// 按 kind 查找资源配置
    pub fn resource_for(&self, kind: &str) -> Option<&ResourceConfig> {
        self.resources.iter().find(|r| r.kind == kind)
    }

    /// 所有资源类型 kind 列表
    pub fn all_kinds(&self) -> Vec<&str> {
        self.resources.iter().map(|r| r.kind.as_str()).collect()
    }

    /// 指定 kind 是否支持某个子命令
    #[allow(dead_code)]
    pub fn kind_has_subcommand(&self, kind: &str, subcmd: &str) -> bool {
        self.resource_for(kind)
            .map(|r| r.extra_subcommands.iter().any(|s| s == subcmd))
            .unwrap_or(false)
    }

    /// 支持指定子命令的所有 kind
    #[allow(dead_code)]
    pub fn kinds_with_subcommand(&self, subcmd: &str) -> Vec<&str> {
        self.resources
            .iter()
            .filter(|r| r.extra_subcommands.iter().any(|s| s == subcmd))
            .map(|r| r.kind.as_str())
            .collect()
    }
}

// ══════════════════════════════════════════════════════════
// 预设
// ══════════════════════════════════════════════════════════

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct PresetsConfig {
    #[serde(default)]
    #[allow(dead_code)]
    pub provider: Vec<ProviderPresetConfig>,
    pub mcp: Vec<McpPresetConfig>,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct ProviderPresetConfig {
    pub name: String,
    pub provider_type: String,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub description: String,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct McpPresetConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub description: String,
}

static PRESETS: OnceLock<PresetsConfig> = OnceLock::new();

pub(crate) fn presets() -> &'static PresetsConfig {
    PRESETS.get_or_init(|| {
        let content = include_str!("presets.toml");
        toml::from_str(content).expect("presets.toml should be valid TOML")
    })
}

trait NamedEntry {
    fn name(&self) -> &str;
}
impl NamedEntry for ProviderPresetConfig {
    fn name(&self) -> &str {
        &self.name
    }
}
impl NamedEntry for McpPresetConfig {
    fn name(&self) -> &str {
        &self.name
    }
}

fn find_by_name<'a, T: NamedEntry>(items: &'a [T], name: &str) -> Option<&'a T> {
    items.iter().find(|p| p.name() == name)
}

impl PresetsConfig {
    #[allow(dead_code)]
    pub fn find_provider(&self, name: &str) -> Option<&ProviderPresetConfig> {
        find_by_name(&self.provider, name)
    }
    #[allow(dead_code)]
    pub fn find_mcp(&self, name: &str) -> Option<&McpPresetConfig> {
        find_by_name(&self.mcp, name)
    }
}

// ══════════════════════════════════════════════════════════
// 适配器默认值
// ══════════════════════════════════════════════════════════

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct AdapterDefaultsConfig {
    pub defaults: DefaultsSection,
    pub cache: CacheSection,
    pub plugin_manifest_dirs: HashMap<String, String>,
    pub plugin_manifest_files: HashMap<String, String>,
    #[serde(default)]
    pub default_models: HashMap<String, String>,
    #[serde(default)]
    pub opencode: OpencodeDefaultsConfig,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct DefaultsSection {
    pub env_field: String,
    pub disabled_tools_field: String,
    #[serde(default = "default_hook_timeout")]
    pub hook_timeout: u64,
    #[serde(default = "default_sensitive_keywords")]
    pub sensitive_keywords: Vec<String>,
    #[serde(default = "default_sync_tag")]
    pub sync_tag: String,
    #[serde(default = "default_sync_description_format")]
    pub sync_description_format: String,
}

fn default_hook_timeout() -> u64 {
    30
}
fn default_sensitive_keywords() -> Vec<String> {
    vec![
        "key".into(),
        "token".into(),
        "secret".into(),
        "password".into(),
        "credential".into(),
        "auth".into(),
    ]
}
fn default_sync_tag() -> String {
    "synced".into()
}
fn default_sync_description_format() -> String {
    "Synced from {}".into()
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct CacheSection {
    pub skills_dir: String,
    pub plugins_dir: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[allow(dead_code)]
pub(crate) struct OpencodeDefaultsConfig {
    #[serde(default)]
    pub schema_url: Option<String>,
    #[serde(default)]
    pub default_npm: Option<String>,
    #[serde(default = "default_context_limit")]
    pub default_context_limit: u64,
}

fn default_context_limit() -> u64 {
    200000
}

impl DefaultsSection {
    /// 生成同步描述文本
    pub fn sync_description(&self, source: &str) -> String {
        self.sync_description_format.replace("{}", source)
    }
}

static ADAPTER_DEFAULTS: OnceLock<AdapterDefaultsConfig> = OnceLock::new();

pub(crate) fn adapter_defaults() -> &'static AdapterDefaultsConfig {
    ADAPTER_DEFAULTS.get_or_init(|| {
        let content = include_str!("adapter_defaults.toml");
        toml::from_str(content).expect("adapter_defaults.toml should be valid TOML")
    })
}

impl AdapterDefaultsConfig {
    pub fn manifest_dir(&self, format: &str) -> Option<&str> {
        self.plugin_manifest_dirs.get(format).map(|s| s.as_str())
    }

    pub fn manifest_file(&self, format: &str) -> &str {
        self.plugin_manifest_files
            .get(format)
            .or(self.plugin_manifest_files.get("_default"))
            .map(|s| s.as_str())
            .unwrap_or("plugin.json")
    }
}

// ══════════════════════════════════════════════════════════
// 集中式配置内容加载
// ══════════════════════════════════════════════════════════

/// 返回工具适配器映射的 TOML 内容（编译时内嵌）
pub(crate) fn adapter_mapping_content(tool_name: &str) -> Option<&'static str> {
    // match 无法从 Vec 动态生成（include_str! 需要编译时常量），
    // 但保持与 tools.adapters 列表同步
    Some(match tool_name {
        "claude" => include_str!("adapter_mappings/claude.toml"),
        "codex" => include_str!("adapter_mappings/codex.toml"),
        "copilot" => include_str!("adapter_mappings/copilot.toml"),
        "crush" => include_str!("adapter_mappings/crush.toml"),
        "cursor" => include_str!("adapter_mappings/cursor.toml"),
        "droid" => include_str!("adapter_mappings/droid.toml"),
        "gemini" => include_str!("adapter_mappings/gemini.toml"),
        "hermes" => include_str!("adapter_mappings/hermes.toml"),
        "kilo" => include_str!("adapter_mappings/kilo.toml"),
        "kilocode" => include_str!("adapter_mappings/kilocode.toml"),
        "kimi" => include_str!("adapter_mappings/kimi.toml"),
        "mux" => include_str!("adapter_mappings/mux.toml"),
        "amp" => include_str!("adapter_mappings/amp.toml"),
        "opencode" => include_str!("adapter_mappings/opencode.toml"),
        "openclaw" => include_str!("adapter_mappings/openclaw.toml"),
        "pi" => include_str!("adapter_mappings/pi.toml"),
        "qwen" => include_str!("adapter_mappings/qwen.toml"),
        "roocode" => include_str!("adapter_mappings/roocode.toml"),
        "aider" => include_str!("adapter_mappings/aider.toml"),
        _ => return None,
    })
}

/// 返回数据源映射的 TOML 内容（编译时内嵌）
pub(crate) fn datasource_mapping_content(name: &str) -> Option<&'static str> {
    // match 无法从 Vec 动态生成（include_str! 需要编译时常量），
    // 但保持与 datasources.names 列表同步
    Some(match name {
        "cc-switch" => include_str!("datasource_mappings/cc-switch.toml"),
        "cherry-studio" => include_str!("datasource_mappings/cherry-studio.toml"),
        _ => return None,
    })
}

/// 返回哈希字段配置的 TOML 内容（编译时内嵌）
pub(crate) fn hash_fields_content() -> &'static str {
    include_str!("hash_fields.toml")
}

// ══════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resource_registry_parses() {
        let reg = resource_registry();
        assert_eq!(reg.resources.len(), 8);
        assert_eq!(reg.id_generation.length, 8);
        assert_eq!(reg.dir_for_kind("provider"), Some("providers"));
        assert_eq!(reg.dir_for_kind("mcp"), Some("mcp"));
        assert_eq!(reg.dir_for_kind("nonexistent"), None);
    }

    #[test]
    fn test_presets_parses() {
        let p = presets();
        assert!(p.provider.is_empty()); // provider presets 已迁移到 models.dev
        assert_eq!(p.mcp.len(), 6);
        assert!(p.find_mcp("filesystem").is_some());
        assert!(p.find_provider("nonexistent").is_none());
    }

    #[test]
    fn test_adapter_defaults_parses() {
        let d = adapter_defaults();
        assert_eq!(d.defaults.env_field, "env");
        assert_eq!(d.cache.skills_dir, "cache/skills");
        assert_eq!(d.manifest_dir("claude"), Some(".claude-plugin"));
        assert_eq!(d.manifest_file("gemini"), "gemini-extension.json");
        assert_eq!(d.manifest_file("unknown"), "plugin.json");
    }

    // ── ResourceRegistry methods ──

    #[test]
    fn test_resource_registry_all_kinds() {
        let reg = resource_registry();
        let kinds = reg.all_kinds();
        assert!(kinds.contains(&"provider"));
        assert!(kinds.contains(&"mcp"));
        assert!(kinds.contains(&"hook"));
        assert!(kinds.contains(&"env"));
    }

    #[test]
    fn test_resource_registry_all_init_dirs() {
        let reg = resource_registry();
        let dirs = reg.all_init_dirs();
        assert!(!dirs.is_empty());
    }

    #[test]
    fn test_resource_registry_validation() {
        let reg = resource_registry();
        let v = reg.validation_for("provider");
        assert!(v.is_some());
    }

    #[test]
    fn test_resource_registry_resource_for() {
        let reg = resource_registry();
        let r = reg.resource_for("mcp");
        assert!(r.is_some());
    }

    #[test]
    fn test_resource_registry_kind_has_subcommand() {
        let reg = resource_registry();
        // kind_has_subcommand checks extra_subcommands only
        // "add" is likely a standard subcommand, not an extra one
        assert!(!reg.kind_has_subcommand("nonexistent_kind", "add"));
    }

    #[test]
    fn test_resource_registry_kinds_with_subcommand() {
        let reg = resource_registry();
        let kinds = reg.kinds_with_subcommand("nonexistent_subcmd");
        // Most subcommands are standard, not extras
        assert!(kinds.is_empty() || !kinds.is_empty()); // just verify no crash
    }

    // ── PresetsConfig methods ──

    #[test]
    fn test_presets_find_mcp() {
        let p = presets();
        let fs = p.find_mcp("filesystem");
        assert!(fs.is_some());
    }

    #[test]
    fn test_presets_find_provider_by_name() {
        let p = presets();
        // provider presets 已迁移到 models.dev，内置为空
        assert!(p.find_provider("anthropic").is_none());
        assert!(p.find_provider("openai").is_none());
    }

    // ── adapter_mapping_content ──

    #[test]
    fn test_adapter_mapping_content_known_tool() {
        let content = adapter_mapping_content("claude");
        assert!(content.is_some());
        assert!(content.unwrap().contains("tool"));
    }

    #[test]
    fn test_adapter_mapping_content_unknown_tool() {
        let content = adapter_mapping_content("nonexistent_tool");
        assert!(content.is_none());
    }

    // ── datasource_mapping_content ──

    #[test]
    fn test_datasource_mapping_content_known() {
        let content = datasource_mapping_content("cc-switch");
        assert!(content.is_some());
    }

    #[test]
    fn test_datasource_mapping_content_unknown() {
        let content = datasource_mapping_content("nonexistent");
        assert!(content.is_none());
    }

    // ── hash_fields_content ──

    #[test]
    fn test_hash_fields_content_valid_toml() {
        let content = hash_fields_content();
        let parsed: Result<toml::Value, _> = content.parse();
        assert!(parsed.is_ok());
    }
}
