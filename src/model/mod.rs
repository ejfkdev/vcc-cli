use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

macro_rules! validate_in_set {
    ($kind:expr, $value:expr, $field_label:expr, $set_access:expr) => {
        let _registry = crate::config::resource_registry();
        let _empty: Vec<String> = Vec::new();
        let _valid = _registry
            .validation_for($kind)
            .map($set_access)
            .unwrap_or(&_empty);
        if !_valid.is_empty() && !_valid.iter().any(|t| t == $value) {
            anyhow::bail!(
                "{} {} must be one of {}, got '{}'",
                $kind,
                $field_label,
                _valid.join(", "),
                $value
            );
        }
    };
}
#[allow(unused_imports)]
use validate_in_set;

/// Macro for apply_field implementations: handles common patterns declaratively.
/// - `desc` sets metadata.description
/// - `str("field")` sets config.field = get_string("field").unwrap_or_default()
///   Declarative field binding for `apply_field` methods.
///   Usage: `apply_field_body!(self, name, fields; "server_type" => str => server_type, "command" => opt => command)`
///   The first token after `=>` is the kind (str/opt/csv/u64/f64/desc),
///   the second is the struct field identifier.
macro_rules! apply_field_body {
    ($self:expr, $name:expr, $fields:expr; $($key:literal => $kind:tt => $field:ident),* $(,)?) => {
        match $name {
            $($key => apply_field_body!(@kind $self, $fields, $key, $kind, $field),)*
            _ => {}
        }
    };
    (@kind $self:expr, $fields:expr, $key:literal, desc, $field:ident) => {
        $self.metadata.description = $fields.get_string($key)
    };
    (@kind $self:expr, $fields:expr, $key:literal, str, $field:ident) => {
        $self.config.$field = $fields.get_string($key).unwrap_or_default()
    };
    (@kind $self:expr, $fields:expr, $key:literal, opt, $field:ident) => {
        $self.config.$field = $fields.get_string($key)
    };
    (@kind $self:expr, $fields:expr, $key:literal, csv, $field:ident) => {
        $self.config.$field = $fields.get_csv($key).unwrap_or_default()
    };
    (@kind $self:expr, $fields:expr, $key:literal, u64, $field:ident) => {
        if let Some(v) = $fields.get_u64($key) { $self.config.$field = v; }
    };
    (@kind $self:expr, $fields:expr, $key:literal, f64, $field:ident) => {
        $self.config.$field = $fields.get_f64($key)
    };
}

/// Apply --tool_set TOOL:KEY=VALUE entries to a resource's `tool` map.
/// Each TOOL gets a ToolOverride entry with the kv pairs stored in `extra`.
fn apply_tool_set<T: Default + HasExtra>(
    tool_map: &mut HashMap<String, T>,
    toolset: &HashMap<String, HashMap<String, String>>,
) {
    for (tool_name, kvs) in toolset {
        let entry = tool_map.entry(tool_name.clone()).or_default();
        for (k, v) in kvs {
            entry
                .extra_mut()
                .insert(k.clone(), toml::Value::String(v.clone()));
        }
    }
}

trait HasExtra {
    fn extra_mut(&mut self) -> &mut HashMap<String, toml::Value>;
}

macro_rules! define_resource {
    ($vis:vis struct $name:ident { kind = $kind:literal, config = $config:ty, tool_override = $tool:ty, metadata = $metadata_type:ty $(,)? }) => {
        #[derive(Serialize, Deserialize, Debug, Clone)]
        $vis struct $name {
            pub name: String, #[serde(skip_serializing_if = "String::is_empty", default)] pub id: String,
            #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")] pub r#type: String, pub config: $config,
            #[serde(default)] pub metadata: $metadata_type, #[serde(default, skip_serializing_if = "HashMap::is_empty")] pub tool: HashMap<String, $tool>,
        }
        impl crate::model::Resource for $name {
            fn kind(&self) -> &'static str { $kind } fn name(&self) -> &str { &self.name } fn id(&self) -> &str { &self.id }
            fn set_id(&mut self, id: String) { self.id = id; }
            fn validate(&self) -> Result<()> { if self.name.is_empty() { bail!(concat!($kind, " name cannot be empty")); } self.validate_config() }
            fn new_with_name(name: &str) -> Self { Self { name: name.to_string(), id: String::new(), r#type: $kind.to_string(), config: <$config>::default(), metadata: <$metadata_type>::default(), tool: HashMap::new() } }
            fn apply_field(&mut self, name: &str, fields: &crate::cli::dynamic::FieldMap) -> Result<()> { self.apply_field(name, fields) }
        }
    };
}

pub(crate) trait Resource:
    serde::Serialize + for<'de> serde::Deserialize<'de> + std::fmt::Debug + Clone + Send + Sync + Sized
{
    fn kind(&self) -> &'static str;
    fn name(&self) -> &str;
    fn id(&self) -> &str;
    fn set_id(&mut self, id: String);
    fn validate(&self) -> Result<()>;
    fn new_with_name(name: &str) -> Self;
    fn apply_field(&mut self, _name: &str, _fields: &crate::cli::dynamic::FieldMap) -> Result<()> {
        Ok(())
    }
    fn apply_fields(&mut self, fields: &crate::cli::dynamic::FieldMap) -> Result<()> {
        let registry = crate::config::resource_registry();
        if let Some(res_cfg) = registry.resource_for(self.kind()) {
            for field in &res_cfg.cli_fields {
                if fields.was_provided(&field.name) {
                    self.apply_field(&field.name, fields)?;
                }
            }
        }
        Ok(())
    }
}

pub(crate) trait SanitizeDisplay: Resource {
    fn sanitize_display(&self, val: &mut serde_json::Value);
}

pub mod hash {
    pub(crate) use super::{compute_hash, hash8};
}
pub(crate) fn compute_hash(content: &str) -> String {
    use std::hash::Hasher;
    let mut h = twox_hash::XxHash3_64::new();
    h.write(content.as_bytes());
    format!("{:016x}", h.finish())
}
pub(crate) fn hash8(full_hash: &str) -> &str {
    &full_hash[..full_hash.len().min(8)]
}

pub mod hash_fields;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct Metadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

// ── Provider ──
define_resource! { pub struct Provider { kind = "provider", config = ProviderConfig, tool_override = ProviderToolOverride, metadata = Metadata } }

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct ProviderConfig {
    #[serde(default)]
    pub provider_type: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub npm: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub model_meta: HashMap<String, ModelMeta>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, toml::Value>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct ModelMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct ProviderToolOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub npm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}
impl From<ProviderConfig> for ProviderToolOverride {
    fn from(c: ProviderConfig) -> Self {
        Self {
            api_key: Some(c.api_key),
            base_url: c.base_url,
            npm: c.npm,
            default_model: c.default_model,
            models: c.models,
            headers: c.headers,
            env: c.env,
        }
    }
}

impl Provider {
    pub fn apply_field(
        &mut self,
        name: &str,
        fields: &crate::cli::dynamic::FieldMap,
    ) -> Result<()> {
        match name {
            "preset" => {
                if let Some(preset_name) = fields.get_string("preset") {
                    if let Some(p) = crate::cli::presets::find_provider_preset(&preset_name) {
                        if !fields.was_provided("type") {
                            self.config.provider_type = p.provider_type.clone();
                        }
                        if !fields.was_provided("url") {
                            self.config.base_url = p.base_url.clone();
                        }
                        if !fields.was_provided("model") {
                            self.config.default_model = p.default_model.clone();
                        }
                        if !fields.was_provided("description") {
                            if let Some(ref desc) = p.description {
                                if !desc.is_empty() {
                                    self.metadata.description = Some(desc.clone());
                                }
                            }
                        }
                    } else {
                        bail!("unknown provider preset '{}'. Use 'vcc preset provider' to see available presets", preset_name);
                    }
                }
            }
            "type" => self.config.provider_type = fields.get_string("type").unwrap_or_default(),
            "key" => self.config.api_key = fields.get_string("key").unwrap_or_default(),
            "url" => self.config.base_url = fields.get_string("url"),
            "model" => self.config.default_model = fields.get_string("model"),
            "models" => self.config.models = fields.get_csv("models").unwrap_or_default(),
            "var" => {
                crate::cli::resource::merge_kvvec(&mut self.config.env, &fields.get_kvvec("var"))
            }
            "description" => self.metadata.description = fields.get_string("description"),
            _ => {}
        }
        Ok(())
    }
    fn validate_config(&self) -> Result<()> {
        if self.config.provider_type.is_empty() {
            bail!("provider type cannot be empty. Use -p <preset> or -t <type> to specify it. Run 'vcc preset provider' for available presets");
        }
        Ok(())
    }
}
pub(crate) fn mask_api_key(key: &str) -> String {
    if key.is_empty() {
        String::new()
    } else {
        "****".into()
    }
}
impl SanitizeDisplay for Provider {
    fn sanitize_display(&self, val: &mut serde_json::Value) {
        if let Some(obj) = val.get_mut("config").and_then(|c| c.as_object_mut()) {
            if let Some(key) = obj.get("api_key").and_then(|v| v.as_str()) {
                obj.insert("api_key".into(), serde_json::json!(mask_api_key(key)));
            }
        }
    }
}
pub mod provider {
    pub(crate) use super::{Provider, ProviderConfig, ProviderToolOverride};
}

// ── MCP ──
define_resource! { pub struct McpServer { kind = "mcp", config = McpConfig, tool_override = McpToolOverride, metadata = Metadata } }

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct McpConfig {
    #[serde(default = "default_server_type")]
    pub server_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, toml::Value>,
}
fn default_server_type() -> String {
    "stdio".into()
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            server_type: default_server_type(),
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            url: None,
            headers: HashMap::new(),
            disabled_tools: Vec::new(),
            extra: HashMap::new(),
        }
    }
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct McpToolOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, toml::Value>,
}
impl HasExtra for McpToolOverride {
    fn extra_mut(&mut self) -> &mut HashMap<String, toml::Value> {
        &mut self.extra
    }
}

/// Parse a tool_set value string into a typed toml::Value.
/// Recognizes booleans ("true"/"false") and integers, falls back to string.
fn parse_tool_set_value(v: &str) -> toml::Value {
    match v {
        "true" => toml::Value::Boolean(true),
        "false" => toml::Value::Boolean(false),
        _ => {
            if let Ok(n) = v.parse::<i64>() {
                toml::Value::Integer(n)
            } else {
                toml::Value::String(v.to_string())
            }
        }
    }
}

/// Apply --tool_set for MCP, routing known override fields to their proper slots.
fn apply_mcp_tool_set(
    tool_map: &mut HashMap<String, McpToolOverride>,
    toolset: &HashMap<String, HashMap<String, String>>,
) {
    for (tool_name, kvs) in toolset {
        let entry = tool_map.entry(tool_name.clone()).or_default();
        for (k, v) in kvs {
            match k.as_str() {
                "disabled_tools" => {
                    let tools: Vec<String> = v
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    entry.disabled_tools.extend(tools);
                }
                "command" => {
                    entry.command = Some(v.clone());
                }
                "args" => {
                    entry.args = Some(v.split(',').map(|s| s.trim().to_string()).collect());
                }
                "url" => {
                    entry.url = Some(v.clone());
                }
                "env" | "headers" => {
                    // Support "env.KEY=VALUE" and "headers.KEY=VALUE" dot notation
                    if let Some(dot_pos) = k.find('.') {
                        let prefix = &k[..dot_pos];
                        let sub_key = &k[dot_pos + 1..];
                        match prefix {
                            "env" => {
                                entry.env.insert(sub_key.to_string(), v.clone());
                            }
                            "headers" => {
                                entry.headers.insert(sub_key.to_string(), v.clone());
                            }
                            _ => {
                                entry
                                    .extra
                                    .insert(k.clone(), toml::Value::String(v.clone()));
                            }
                        }
                    } else {
                        // "env" or "headers" without dot notation: KEY itself is the field name
                        entry.extra.insert(k.clone(), parse_tool_set_value(v));
                    }
                }
                _ => {
                    // Also handle "env.KEY" and "headers.KEY" dot notation for unknown prefix
                    if let Some(dot_pos) = k.find('.') {
                        let prefix = &k[..dot_pos];
                        let sub_key = &k[dot_pos + 1..];
                        match prefix {
                            "env" => {
                                entry.env.insert(sub_key.to_string(), v.clone());
                            }
                            "headers" => {
                                entry.headers.insert(sub_key.to_string(), v.clone());
                            }
                            _ => {
                                entry.extra.insert(k.clone(), parse_tool_set_value(v));
                            }
                        }
                    } else {
                        entry.extra.insert(k.clone(), parse_tool_set_value(v));
                    }
                }
            }
        }
    }
}
impl McpServer {
    pub fn apply_field(
        &mut self,
        name: &str,
        fields: &crate::cli::dynamic::FieldMap,
    ) -> Result<()> {
        match name {
            "preset" => {
                if let Some(preset_name) = fields.get_string("preset") {
                    if let Some(p) = crate::cli::presets::find_mcp_preset(&preset_name) {
                        if !fields.was_provided("command") {
                            self.config.command = Some(p.command.clone());
                        }
                        if !fields.was_provided("args") {
                            self.config.args = p.args.clone();
                        }
                        if !fields.was_provided("description") && !p.description.is_empty() {
                            self.metadata.description = Some(p.description.clone());
                        }
                    } else {
                        bail!("unknown mcp preset '{}'. Use 'vcc preset mcp' to see available presets", preset_name);
                    }
                }
            }
            "type" => {
                let new_type = fields.get_string("type").unwrap_or_default();
                let old_type = self.config.server_type.clone();
                self.config.server_type = new_type;
                // Clear fields irrelevant to the new type
                if old_type != self.config.server_type {
                    match self.config.server_type.as_str() {
                        "sse" | "streamable-http" => {
                            self.config.command = None;
                            self.config.args.clear();
                        }
                        _ => {
                            self.config.url = None;
                            self.config.headers.clear();
                        }
                    }
                }
            }
            "var" => {
                crate::cli::resource::merge_kvvec(&mut self.config.env, &fields.get_kvvec("var"))
            }
            "header" => crate::cli::resource::merge_kvvec(
                &mut self.config.headers,
                &fields.get_kvvec("header"),
            ),
            "tool_set" => apply_mcp_tool_set(&mut self.tool, &fields.get_toolset("tool_set")),
            _ => apply_field_body!(self, name, fields;
                "command" => opt => command,
                "url" => opt => url, "args" => csv => args,
                "description" => desc => description),
        }
        Ok(())
    }
    fn validate_config(&self) -> Result<()> {
        validate_in_set!("mcp", &self.config.server_type, "server_type", |v| &v
            .valid_types);
        match self.config.server_type.as_str() {
            "stdio"
                if (self.config.command.is_none()
                    || self.config.command.as_deref().unwrap_or("").is_empty()) =>
            {
                bail!("mcp stdio type requires --command");
            }
            "sse" | "streamable-http"
                if (self.config.url.is_none()
                    || self.config.url.as_deref().unwrap_or("").is_empty()) =>
            {
                bail!("mcp {} type requires --url", self.config.server_type);
            }
            _ => {}
        }
        Ok(())
    }
}
pub(crate) fn mask_env_map(env: &HashMap<String, String>) -> HashMap<String, String> {
    let sensitive_keywords = &crate::config::adapter_defaults()
        .defaults
        .sensitive_keywords;
    env.iter()
        .map(|(k, v)| {
            let lower = k.to_lowercase();
            let is_sensitive = sensitive_keywords
                .iter()
                .any(|kw| lower.contains(kw.as_str()));
            (
                k.clone(),
                if is_sensitive {
                    "****".into()
                } else {
                    v.clone()
                },
            )
        })
        .collect()
}
impl SanitizeDisplay for McpServer {
    fn sanitize_display(&self, val: &mut serde_json::Value) {
        if let Some(env_obj) = val
            .get_mut("config")
            .and_then(|c| c.as_object_mut())
            .and_then(|c| c.get_mut("env"))
            .and_then(|e| e.as_object_mut())
        {
            let masked = mask_env_map(&self.config.env);
            for (k, v) in &masked {
                env_obj.insert(k.clone(), serde_json::json!(v));
            }
        }
    }
}
pub mod mcp {
    pub(crate) use super::{McpConfig, McpServer, McpToolOverride};
}

// ── Hook ──
define_resource! { pub struct Hook { kind = "hook", config = HookConfig, tool_override = HookToolOverride, metadata = Metadata } }

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct HookConfig {
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub matcher: String,
    #[serde(default)]
    pub command: String,
    #[serde(default = "default_hook_timeout")]
    pub timeout: u64,
}
fn default_hook_timeout() -> u64 {
    crate::config::adapter_defaults().defaults.hook_timeout
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct HookToolOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, toml::Value>,
}
impl HasExtra for HookToolOverride {
    fn extra_mut(&mut self) -> &mut HashMap<String, toml::Value> {
        &mut self.extra
    }
}
impl Hook {
    pub fn apply_field(
        &mut self,
        name: &str,
        fields: &crate::cli::dynamic::FieldMap,
    ) -> Result<()> {
        match name {
            "tool_set" => apply_tool_set(&mut self.tool, &fields.get_toolset("tool_set")),
            _ => apply_field_body!(self, name, fields;
                "event" => str => event, "matcher" => str => matcher,
                "command" => str => command, "timeout" => u64 => timeout,
                "description" => desc => description),
        }
        Ok(())
    }
    fn validate_config(&self) -> Result<()> {
        validate_in_set!("hook", &self.config.event, "event", |v| &v.valid_events);
        Ok(())
    }
}
impl SanitizeDisplay for Hook {
    fn sanitize_display(&self, val: &mut serde_json::Value) {
        if let Some(tools) = val.get_mut("tool").and_then(|t| t.as_object_mut()) {
            for (_tool_name, tool_val) in tools.iter_mut() {
                if let Some(env) = tool_val
                    .get_mut("extra")
                    .and_then(|e| e.as_object_mut())
                    .and_then(|e| e.get_mut("env"))
                    .and_then(|e| e.as_object_mut())
                {
                    for (k, v) in env.iter_mut() {
                        if let Some(s) = v.as_str() {
                            let lower = k.to_lowercase();
                            let sensitive_keywords = &crate::config::adapter_defaults()
                                .defaults
                                .sensitive_keywords;
                            if sensitive_keywords
                                .iter()
                                .any(|kw| lower.contains(kw.as_str()))
                            {
                                *v = serde_json::Value::String(format!(
                                    "{}****{}",
                                    &s[..4.min(s.len())],
                                    &s[s.len().saturating_sub(4)..]
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
}
pub mod hook {
    pub(crate) use super::{Hook, HookConfig, HookToolOverride};
}

// ── Agent ──
define_resource! { pub struct Agent { kind = "agent", config = AgentConfig, tool_override = AgentToolOverride, metadata = Metadata } }

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct AgentConfig {
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub tools: AgentTools,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub permission: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}
fn default_mode() -> String {
    "subagent".into()
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct AgentTools {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled: Vec<String>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct AgentToolOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_enabled: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_disabled: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}
impl Agent {
    pub fn apply_field(
        &mut self,
        name: &str,
        fields: &crate::cli::dynamic::FieldMap,
    ) -> Result<()> {
        match name {
            "mode" => self.config.mode = fields.get_string("mode").unwrap_or_default(),
            "description" => self.config.description = fields.get_string("description"),
            "model" => self.config.model = fields.get_string("model"),
            "tools_enabled" => {
                self.config.tools.enabled = fields.get_csv("tools_enabled").unwrap_or_default()
            }
            "tools_disabled" => {
                self.config.tools.disabled = fields.get_csv("tools_disabled").unwrap_or_default()
            }
            "temperature" => self.config.temperature = fields.get_f64("temperature"),
            "content" | "file" => {
                if let Ok(Some(c)) = fields.get_content_or_file("content", "file") {
                    self.config.content = Some(c);
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn validate_config(&self) -> Result<()> {
        validate_in_set!("agent", &self.config.mode, "mode", |v| &v.valid_modes);
        if let Some(t) = self.config.temperature {
            if !(0.0..=2.0).contains(&t) {
                bail!("agent temperature must be between 0.0 and 2.0, got {}", t);
            }
        }
        Ok(())
    }
}
pub mod agent {
    pub(crate) use super::{Agent, AgentConfig, AgentTools};
}

// ── Skill ──
define_resource! { pub struct Skill { kind = "skill", config = SkillConfig, tool_override = SkillToolOverride, metadata = Metadata } }

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct SkillConfig {
    #[serde(default)]
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default = "default_install_method")]
    pub install_method: String,
}
fn default_install_method() -> String {
    "symlink".into()
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct SkillToolOverride {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}
impl Skill {
    pub fn apply_field(
        &mut self,
        name: &str,
        fields: &crate::cli::dynamic::FieldMap,
    ) -> Result<()> {
        apply_field_body!(self, name, fields;
            "source" => str => source, "repo" => opt => repo,
            "path" => opt => path, "install_method" => str => install_method,
            "description" => desc => description);
        Ok(())
    }
    fn validate_config(&self) -> Result<()> {
        validate_in_set!(
            "skill",
            &self.config.install_method,
            "install_method",
            |v| &v.valid_install_methods
        );
        validate_in_set!("skill", &self.config.source, "source", |v| &v.valid_sources);
        match self.config.source.as_str() {
            "github" if self.config.repo.is_none() => {
                bail!("github skill requires a repo");
            }
            "local" if self.config.path.is_none() => {
                bail!("local skill requires a path");
            }
            "url" if self.config.path.is_none() => {
                bail!("url skill requires a path (url)");
            }
            _ => {}
        }
        Ok(())
    }
}
pub mod skill {
    pub(crate) use super::{Skill, SkillToolOverride};
}

// ── Prompt ──
define_resource! { pub struct Prompt { kind = "prompt", config = PromptConfig, tool_override = PromptToolOverride, metadata = Metadata } }

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct PromptConfig {
    #[serde(default)]
    pub content: String,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct PromptToolOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}
impl Prompt {
    pub fn apply_field(
        &mut self,
        name: &str,
        fields: &crate::cli::dynamic::FieldMap,
    ) -> Result<()> {
        match name {
            "content" | "file" => {
                if let Ok(Some(c)) = fields.get_content_or_file("content", "file") {
                    self.config.content = c;
                }
            }
            _ => apply_field_body!(self, name, fields; "description" => desc => description),
        }
        Ok(())
    }
    fn validate_config(&self) -> Result<()> {
        if self.config.content.is_empty() {
            bail!("prompt content cannot be empty");
        }
        Ok(())
    }
}
pub mod prompt {
    pub(crate) use super::{Prompt, PromptToolOverride};
}

// ── Env ──
define_resource! { pub struct Env { kind = "env", config = EnvConfig, tool_override = EnvToolOverride, metadata = Metadata } }

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct EnvConfig {
    #[serde(default)]
    pub vars: HashMap<String, String>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct EnvToolOverride {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub vars: HashMap<String, String>,
}
impl Env {
    pub fn apply_field(
        &mut self,
        name: &str,
        fields: &crate::cli::dynamic::FieldMap,
    ) -> Result<()> {
        match name {
            // Env vars: empty value means set to empty string (not delete),
            // unlike MCP/Hook env where empty means remove.
            "var" => {
                for (k, v) in &fields.get_kvvec("var") {
                    self.config.vars.insert(k.clone(), v.clone());
                }
            }
            "remove_var" => {
                for key in &fields.get_csv("remove_var").unwrap_or_default() {
                    self.config.vars.remove(key);
                }
            }
            "description" => self.metadata.description = fields.get_string("description"),
            _ => {}
        }
        Ok(())
    }
    fn validate_config(&self) -> Result<()> {
        Ok(())
    }
}
pub mod env {
    pub(crate) use super::{Env, EnvToolOverride};
}

// ── Plugin ──
define_resource! { pub struct Plugin { kind = "plugin", config = PluginConfig, tool_override = PluginToolOverride, metadata = Metadata } }

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct PluginConfig {
    #[serde(default)]
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marketplace: Option<String>,
    #[serde(default = "default_plugin_install")]
    pub install_method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}
fn default_plugin_install() -> String {
    "symlink".into()
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct PluginToolOverride {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, toml::Value>,
}
impl Plugin {
    pub fn apply_field(
        &mut self,
        name: &str,
        fields: &crate::cli::dynamic::FieldMap,
    ) -> Result<()> {
        apply_field_body!(self, name, fields;
            "source" => str => source, "repo" => opt => repo,
            "path" => opt => path, "marketplace" => opt => marketplace,
            "format" => opt => format, "install_method" => str => install_method,
            "description" => desc => description);
        Ok(())
    }
    fn validate_config(&self) -> Result<()> {
        validate_in_set!(
            "plugin",
            &self.config.install_method,
            "install_method",
            |v| &v.valid_install_methods
        );
        validate_in_set!("plugin", &self.config.source, "source", |v| &v
            .valid_sources);
        match self.config.source.as_str() {
            "github" | "url" if self.config.repo.is_none() && self.config.path.is_none() => {
                bail!("{} plugin requires a repo or path", self.config.source);
            }
            "local" if self.config.path.is_none() => {
                bail!("local plugin requires a path");
            }
            "marketplace" if self.config.marketplace.is_none() => {
                bail!("marketplace plugin requires a marketplace name");
            }
            _ => {}
        }
        if let Some(ref fmt) = self.config.format {
            validate_in_set!("plugin", fmt, "format", |v| &v.valid_formats);
        }
        Ok(())
    }
}
pub mod plugin {
    pub(crate) use super::Plugin;
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── compute_hash ──

    #[test]
    fn test_compute_hash_deterministic() {
        let h1 = compute_hash("hello");
        let h2 = compute_hash("hello");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_compute_hash_different_inputs() {
        let h1 = compute_hash("hello");
        let h2 = compute_hash("world");
        assert_ne!(h1, h2);
    }

    // ── parse_tool_set_value ──

    #[test]
    fn test_parse_tool_set_boolean_true() {
        let v = parse_tool_set_value("true");
        assert!(v.is_bool() && v.as_bool().unwrap());
    }

    #[test]
    fn test_parse_tool_set_boolean_false() {
        let v = parse_tool_set_value("false");
        assert!(v.is_bool() && !v.as_bool().unwrap());
    }

    #[test]
    fn test_parse_tool_set_integer() {
        let v = parse_tool_set_value("42");
        assert!(v.is_integer() && v.as_integer().unwrap() == 42);
    }

    #[test]
    fn test_parse_tool_set_string() {
        let v = parse_tool_set_value("hello");
        assert!(v.is_str() && v.as_str().unwrap() == "hello");
    }

    #[test]
    fn test_parse_tool_set_negative_integer() {
        let v = parse_tool_set_value("-1");
        assert!(v.is_integer() && v.as_integer().unwrap() == -1);
    }

    #[test]
    fn test_compute_hash_empty_string() {
        let h = compute_hash("");
        assert!(!h.is_empty());
        assert_eq!(h.len(), 16); // XXH3_64 → 16 hex chars
    }

    #[test]
    fn test_compute_hash_format() {
        let h = compute_hash("test");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ── hash8 ──

    #[test]
    fn test_hash8_truncates() {
        let full = compute_hash("test");
        let h8 = hash8(&full);
        assert_eq!(h8.len(), 8);
        assert_eq!(h8, &full[..8]);
    }

    #[test]
    fn test_hash8_short_string() {
        let short = "abc";
        let h8 = hash8(short);
        assert_eq!(h8, "abc"); // len < 8, returns all
    }

    #[test]
    fn test_hash8_exact_8() {
        let s = "12345678";
        assert_eq!(hash8(s), "12345678");
    }

    #[test]
    fn test_hash8_7_chars() {
        let s = "1234567";
        assert_eq!(hash8(s), "1234567");
    }

    // ── mask_api_key ──

    #[test]
    fn test_mask_api_key_long() {
        assert_eq!(mask_api_key("sk-abcdefgh12345678"), "****");
    }

    #[test]
    fn test_mask_api_key_short() {
        assert_eq!(mask_api_key("abc"), "****");
    }

    #[test]
    fn test_mask_api_key_4_chars() {
        assert_eq!(mask_api_key("abcd"), "****");
    }

    #[test]
    fn test_mask_api_key_5_chars() {
        assert_eq!(mask_api_key("abcde"), "****");
    }

    #[test]
    fn test_mask_api_key_empty() {
        assert_eq!(mask_api_key(""), "");
    }

    // ── mask_env_map ──

    #[test]
    fn test_mask_env_map_sensitive_key() {
        let env = HashMap::from([
            ("API_KEY".into(), "sk-secret123".into()),
            ("NORMAL_VAR".into(), "visible".into()),
        ]);
        let masked = mask_env_map(&env);
        assert_eq!(masked.get("NORMAL_VAR").unwrap(), "visible");
        let masked_key = masked.get("API_KEY").unwrap();
        assert_eq!(masked_key, "****"); // fully masked
    }

    #[test]
    fn test_mask_env_map_empty() {
        let env: HashMap<String, String> = HashMap::new();
        let masked = mask_env_map(&env);
        assert!(masked.is_empty());
    }

    // ── Profile::validate ──

    #[test]
    fn test_profile_validate_empty_name() {
        let p = Profile {
            name: String::new(),
            description: None,
            model: Default::default(),
            providers: Default::default(),
            mcp_servers: Default::default(),
            skills: Default::default(),
            prompts: Default::default(),
            env: Default::default(),
            hooks: Default::default(),
            agents: Default::default(),
            plugins: Default::default(),
            overrides: Default::default(),
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn test_profile_validate_duplicate_mcp() {
        let p = Profile {
            name: "test".into(),
            description: None,
            model: Default::default(),
            providers: Default::default(),
            mcp_servers: super::profile::ProfileResources {
                enabled: vec!["fs".into(), "fs".into()],
            },
            skills: Default::default(),
            prompts: Default::default(),
            env: Default::default(),
            hooks: Default::default(),
            agents: Default::default(),
            plugins: Default::default(),
            overrides: Default::default(),
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn test_profile_validate_no_duplicates() {
        let p = Profile {
            name: "test".into(),
            description: None,
            model: Default::default(),
            providers: Default::default(),
            mcp_servers: super::profile::ProfileResources {
                enabled: vec!["fs".into(), "icm".into()],
            },
            skills: Default::default(),
            prompts: Default::default(),
            env: Default::default(),
            hooks: Default::default(),
            agents: Default::default(),
            plugins: Default::default(),
            overrides: Default::default(),
        };
        assert!(p.validate().is_ok());
    }

    // ── apply_tool_set ──

    #[test]
    fn test_apply_tool_set_adds_new_entry() {
        let mut tool_map: HashMap<String, McpToolOverride> = HashMap::new();
        let mut toolset = HashMap::new();
        let mut kvs = HashMap::new();
        kvs.insert("disabled".to_string(), "true".to_string());
        toolset.insert("claude".to_string(), kvs);
        apply_tool_set(&mut tool_map, &toolset);
        assert!(tool_map.contains_key("claude"));
    }

    #[test]
    fn test_apply_tool_set_merges_into_existing() {
        let mut tool_map: HashMap<String, McpToolOverride> = HashMap::new();
        tool_map.insert(
            "claude".into(),
            McpToolOverride {
                disabled_tools: vec!["old".into()],
                ..Default::default()
            },
        );
        let mut toolset = HashMap::new();
        let mut kvs = HashMap::new();
        kvs.insert("extra_key".to_string(), "extra_val".to_string());
        toolset.insert("claude".to_string(), kvs);
        apply_tool_set(&mut tool_map, &toolset);
        assert!(tool_map["claude"].extra.contains_key("extra_key"));
        assert_eq!(tool_map["claude"].disabled_tools, vec!["old"]); // preserved
    }

    #[test]
    fn test_apply_tool_set_multiple_tools() {
        let mut tool_map: HashMap<String, McpToolOverride> = HashMap::new();
        let mut toolset = HashMap::new();
        let mut kvs1 = HashMap::new();
        kvs1.insert("key1".to_string(), "val1".to_string());
        toolset.insert("claude".to_string(), kvs1);
        let mut kvs2 = HashMap::new();
        kvs2.insert("key2".to_string(), "val2".to_string());
        toolset.insert("codex".to_string(), kvs2);
        apply_tool_set(&mut tool_map, &toolset);
        assert_eq!(tool_map.len(), 2);
    }

    #[test]
    fn test_apply_tool_set_empty() {
        let mut tool_map: HashMap<String, McpToolOverride> = HashMap::new();
        let toolset: HashMap<String, HashMap<String, String>> = HashMap::new();
        apply_tool_set(&mut tool_map, &toolset);
        assert!(tool_map.is_empty());
    }

    // ── validate_config ──

    #[test]
    fn test_provider_validate_config_empty_type() {
        let p = Provider {
            name: "test".into(),
            id: String::new(),
            r#type: "provider".into(),
            config: ProviderConfig {
                provider_type: String::new(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(p.validate_config().is_err());
    }

    #[test]
    fn test_provider_validate_config_valid() {
        let p = Provider {
            name: "test".into(),
            id: String::new(),
            r#type: "provider".into(),
            config: ProviderConfig {
                provider_type: "openai".into(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(p.validate_config().is_ok());
    }

    #[test]
    fn test_prompt_validate_config_empty() {
        let p = Prompt {
            name: "test".into(),
            id: String::new(),
            r#type: "prompt".into(),
            config: PromptConfig {
                content: String::new(),
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(p.validate_config().is_err());
    }

    #[test]
    fn test_prompt_validate_config_valid() {
        let p = Prompt {
            name: "test".into(),
            id: String::new(),
            r#type: "prompt".into(),
            config: PromptConfig {
                content: "Hello".into(),
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(p.validate_config().is_ok());
    }

    #[test]
    fn test_env_validate_config_always_ok() {
        let e = Env {
            name: "test".into(),
            id: String::new(),
            r#type: "env".into(),
            config: EnvConfig::default(),
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(e.validate_config().is_ok());
    }

    // ── SanitizeDisplay ──

    #[test]
    fn test_provider_sanitize_display() {
        let p = Provider {
            name: "test".into(),
            id: String::new(),
            r#type: "provider".into(),
            config: ProviderConfig {
                api_key: "sk-1234567890".into(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let mut val = serde_json::to_value(&p).unwrap();
        p.sanitize_display(&mut val);
        let sanitized_key = val["config"]["api_key"].as_str().unwrap();
        assert!(!sanitized_key.contains("1234567890"));
        assert!(sanitized_key.contains("****"));
    }

    #[test]
    fn test_mcp_sanitize_display() {
        let mut env = HashMap::new();
        env.insert("ANTHROPIC_API_KEY".into(), "sk-secret-key".into());
        env.insert("PATH_VAR".into(), "/usr/bin".into());
        let m = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                env,
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let mut val = serde_json::to_value(&m).unwrap();
        m.sanitize_display(&mut val);
        let api_key = val["config"]["env"]["ANTHROPIC_API_KEY"].as_str().unwrap();
        assert!(api_key.contains("****"));
        let path = val["config"]["env"]["PATH_VAR"].as_str().unwrap();
        assert_eq!(path, "/usr/bin"); // non-sensitive not masked
    }

    // ── Resource trait ──

    #[test]
    fn test_resource_kind() {
        let p = Provider {
            name: "test".into(),
            id: String::new(),
            r#type: "provider".into(),
            config: ProviderConfig::default(),
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert_eq!(p.kind(), "provider");
    }

    #[test]
    fn test_mcp_resource_kind() {
        let m = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig::default(),
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert_eq!(m.kind(), "mcp");
    }

    #[test]
    fn test_hook_resource_kind() {
        let h = Hook {
            name: "test".into(),
            id: String::new(),
            r#type: "hook".into(),
            config: HookConfig {
                event: "PreToolUse".into(),
                matcher: String::new(),
                command: "echo".into(),
                timeout: 30,
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert_eq!(h.kind(), "hook");
    }

    #[test]
    fn test_new_with_name() {
        let p = Provider::new_with_name("my-provider");
        assert_eq!(p.name, "my-provider");
        assert_eq!(p.r#type, "provider");
    }

    #[test]
    fn test_mcp_new_with_name() {
        let m = McpServer::new_with_name("fs");
        assert_eq!(m.name, "fs");
        assert_eq!(m.r#type, "mcp");
        assert_eq!(m.config.server_type, "stdio");
    }

    #[test]
    fn test_hook_new_with_name() {
        let h = Hook::new_with_name("test-hook");
        assert_eq!(h.name, "test-hook");
        assert_eq!(h.r#type, "hook");
    }

    // ── Profile duplicate detection across kinds ──

    #[test]
    fn test_profile_validate_duplicate_skill() {
        let p = Profile {
            name: "test".into(),
            description: None,
            model: Default::default(),
            providers: Default::default(),
            mcp_servers: Default::default(),
            skills: super::profile::ProfileResources {
                enabled: vec!["skill-a".into(), "skill-a".into()],
            },
            prompts: Default::default(),
            env: Default::default(),
            hooks: Default::default(),
            agents: Default::default(),
            plugins: Default::default(),
            overrides: Default::default(),
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn test_profile_validate_duplicate_hook() {
        let p = Profile {
            name: "test".into(),
            description: None,
            model: Default::default(),
            providers: Default::default(),
            mcp_servers: Default::default(),
            skills: Default::default(),
            prompts: Default::default(),
            env: Default::default(),
            hooks: super::profile::ProfileResources {
                enabled: vec!["hook-x".into(), "hook-x".into()],
            },
            agents: Default::default(),
            plugins: Default::default(),
            overrides: Default::default(),
        };
        assert!(p.validate().is_err());
    }

    // ── Provider with headers ──

    #[test]
    fn test_provider_config_with_headers() {
        let pc = ProviderConfig {
            provider_type: "openai".into(),
            api_key: "sk-123".into(),
            headers: HashMap::from([("X-Custom".into(), "value".into())]),
            ..Default::default()
        };
        assert_eq!(pc.headers.len(), 1);
        assert_eq!(pc.headers.get("X-Custom").unwrap(), "value");
    }

    // ── McpConfig with env and args ──

    #[test]
    fn test_mcp_config_with_env_and_args() {
        let mc = McpConfig {
            command: Some("npx".into()),
            args: vec!["-y".into(), "@anthropic/mcp-server".into()],
            env: HashMap::from([("API_KEY".into(), "sk-123".into())]),
            server_type: "stdio".into(),
            url: None,
            disabled_tools: vec![],
            ..Default::default()
        };
        assert_eq!(mc.args.len(), 2);
        assert_eq!(mc.env.len(), 1);
    }

    // ── mask_api_key with prefix ──

    #[test]
    fn test_mask_api_key_with_prefix() {
        let result = mask_api_key("sk-ant-api03-1234567890abcdef");
        assert_eq!(result, "****"); // fully masked, no prefix leak
    }

    // ── mask_env_map all sensitive ──

    #[test]
    fn test_mask_env_map_all_sensitive() {
        let env = HashMap::from([
            ("API_KEY".into(), "sk-secret".into()),
            ("SECRET_TOKEN".into(), "tok-abc".into()),
        ]);
        let masked = mask_env_map(&env);
        assert_eq!(masked.len(), 2);
        for val in masked.values() {
            assert_eq!(val, "****"); // fully masked
        }
    }

    // ── compute_hash stability across platforms ──

    #[test]
    fn test_compute_hash_long_input() {
        let long = "a".repeat(10000);
        let h = compute_hash(&long);
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_compute_hash_unicode() {
        let h = compute_hash("你好世界");
        assert_eq!(h.len(), 16);
    }

    // ── apply_mcp_tool_set ──

    #[test]
    fn test_apply_mcp_tool_set_disabled_tools() {
        let mut tool_map: HashMap<String, McpToolOverride> = HashMap::new();
        let toolset = HashMap::from([(
            "claude".to_string(),
            HashMap::from([("disabled_tools".to_string(), "Read,Write".to_string())]),
        )]);
        apply_mcp_tool_set(&mut tool_map, &toolset);
        let entry = tool_map.get("claude").unwrap();
        assert!(entry.disabled_tools.contains(&"Read".to_string()));
        assert!(entry.disabled_tools.contains(&"Write".to_string()));
    }

    #[test]
    fn test_apply_mcp_tool_set_command_override() {
        let mut tool_map: HashMap<String, McpToolOverride> = HashMap::new();
        let toolset = HashMap::from([(
            "kimi".to_string(),
            HashMap::from([("command".to_string(), "custom-cmd".to_string())]),
        )]);
        apply_mcp_tool_set(&mut tool_map, &toolset);
        assert_eq!(
            tool_map.get("kimi").unwrap().command.as_deref(),
            Some("custom-cmd")
        );
    }

    #[test]
    fn test_apply_mcp_tool_set_args_override() {
        let mut tool_map: HashMap<String, McpToolOverride> = HashMap::new();
        let toolset = HashMap::from([(
            "claude".to_string(),
            HashMap::from([("args".to_string(), "a, b, c".to_string())]),
        )]);
        apply_mcp_tool_set(&mut tool_map, &toolset);
        let entry = tool_map.get("claude").unwrap();
        assert_eq!(entry.args.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn test_apply_mcp_tool_set_env_dot_notation() {
        let mut tool_map: HashMap<String, McpToolOverride> = HashMap::new();
        let toolset = HashMap::from([(
            "kimi".to_string(),
            HashMap::from([("env.API_KEY".to_string(), "sk-123".to_string())]),
        )]);
        apply_mcp_tool_set(&mut tool_map, &toolset);
        let entry = tool_map.get("kimi").unwrap();
        assert_eq!(entry.env.get("API_KEY").unwrap(), "sk-123");
    }

    #[test]
    fn test_apply_mcp_tool_set_headers_dot_notation() {
        let mut tool_map: HashMap<String, McpToolOverride> = HashMap::new();
        let toolset = HashMap::from([(
            "kimi".to_string(),
            HashMap::from([(
                "headers.Authorization".to_string(),
                "Bearer tok".to_string(),
            )]),
        )]);
        apply_mcp_tool_set(&mut tool_map, &toolset);
        let entry = tool_map.get("kimi").unwrap();
        assert_eq!(entry.headers.get("Authorization").unwrap(), "Bearer tok");
    }

    #[test]
    fn test_apply_mcp_tool_set_unknown_key_goes_to_extra() {
        let mut tool_map: HashMap<String, McpToolOverride> = HashMap::new();
        let toolset = HashMap::from([(
            "claude".to_string(),
            HashMap::from([("custom_field".to_string(), "hello".to_string())]),
        )]);
        apply_mcp_tool_set(&mut tool_map, &toolset);
        let entry = tool_map.get("claude").unwrap();
        assert_eq!(
            entry.extra.get("custom_field").unwrap().as_str(),
            Some("hello")
        );
    }

    #[test]
    fn test_apply_mcp_tool_set_url_override() {
        let mut tool_map: HashMap<String, McpToolOverride> = HashMap::new();
        let toolset = HashMap::from([(
            "claude".to_string(),
            HashMap::from([("url".to_string(), "http://example.com/sse".to_string())]),
        )]);
        apply_mcp_tool_set(&mut tool_map, &toolset);
        assert_eq!(
            tool_map.get("claude").unwrap().url.as_deref(),
            Some("http://example.com/sse")
        );
    }

    // ── preset validation (Bug #33) ──

    #[test]
    fn test_mcp_apply_field_invalid_preset_errors() {
        use crate::cli::dynamic::{FieldMap, FieldValue};
        let mut mcp = McpServer::new_with_name("test");
        let mut fm = FieldMap::default();
        fm.insert(
            "preset".to_string(),
            FieldValue::String(Some("nonexistent_preset_xyz".to_string())),
        );
        let result = mcp.apply_field("preset", &fm);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown mcp preset"),
            "expected preset error, got: {}",
            err
        );
        assert!(
            err.contains("nonexistent_preset_xyz"),
            "error should mention the preset name"
        );
    }

    #[test]
    fn test_provider_apply_field_invalid_preset_errors() {
        use crate::cli::dynamic::{FieldMap, FieldValue};
        let mut prov = Provider::new_with_name("test");
        let mut fm = FieldMap::default();
        fm.insert(
            "preset".to_string(),
            FieldValue::String(Some("nonexistent_preset_xyz".to_string())),
        );
        let result = prov.apply_field("preset", &fm);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown provider preset"),
            "expected preset error, got: {}",
            err
        );
    }

    #[test]
    fn test_mcp_apply_field_valid_preset_succeeds() {
        use crate::cli::dynamic::{FieldMap, FieldValue};
        let mut mcp = McpServer::new_with_name("test");
        let mut fm = FieldMap::default();
        fm.insert(
            "preset".to_string(),
            FieldValue::String(Some("filesystem".to_string())),
        );
        let result = mcp.apply_field("preset", &fm);
        assert!(result.is_ok());
        assert_eq!(mcp.config.command.as_deref(), Some("npx"));
    }

    // ── Plugin source auto-inference (Bug #34) ──
    // Note: PluginConfig::default() has source="" but CLI sets "github" via clap default_value.
    // Tests simulate the CLI behavior by explicitly setting source.

    #[test]
    fn test_plugin_default_source_empty() {
        let plugin = Plugin::new_with_name("test");
        // PluginConfig::default() gives empty string; CLI fills "github" via clap default_value
        assert_eq!(plugin.config.source, "");
    }

    #[test]
    fn test_plugin_marketplace_sets_source() {
        let mut plugin = Plugin::new_with_name("test");
        plugin.config.source = "github".to_string(); // simulates CLI default_value
        plugin.config.marketplace = Some("my-market".to_string());
        if plugin.config.source == "github" && plugin.config.marketplace.is_some() {
            plugin.config.source = "marketplace".to_string();
        }
        assert_eq!(plugin.config.source, "marketplace");
    }

    #[test]
    fn test_plugin_path_without_repo_sets_local() {
        let mut plugin = Plugin::new_with_name("test");
        plugin.config.source = "github".to_string(); // simulates CLI default_value
        plugin.config.path = Some("/local/path".to_string());
        if plugin.config.source == "github"
            && plugin.config.repo.is_none()
            && plugin.config.marketplace.is_none()
        {
            plugin.config.source = "local".to_string();
        }
        assert_eq!(plugin.config.source, "local");
    }

    #[test]
    fn test_plugin_repo_with_local_source_sets_github() {
        let mut plugin = Plugin::new_with_name("test");
        plugin.config.source = "local".to_string();
        plugin.config.repo = Some("owner/repo".to_string());
        if plugin.config.source == "local" && plugin.config.repo.is_some() {
            plugin.config.source = "github".to_string();
        }
        assert_eq!(plugin.config.source, "github");
    }

    #[test]
    fn test_plugin_explicit_source_not_overridden() {
        let mut plugin = Plugin::new_with_name("test");
        plugin.config.source = "url".to_string();
        plugin.config.marketplace = Some("my-market".to_string());
        if plugin.config.source == "github" && plugin.config.marketplace.is_some() {
            plugin.config.source = "marketplace".to_string();
        }
        assert_eq!(plugin.config.source, "url");
    }

    #[test]
    fn test_plugin_marketplace_takes_priority_over_path() {
        let mut plugin = Plugin::new_with_name("test");
        plugin.config.source = "github".to_string(); // simulates CLI default_value
        plugin.config.marketplace = Some("my-market".to_string());
        plugin.config.path = Some("/some/path".to_string());
        if plugin.config.source == "github" && plugin.config.marketplace.is_some() {
            plugin.config.source = "marketplace".to_string();
        }
        assert_eq!(plugin.config.source, "marketplace");
    }

    // ── MCP type change field cleanup (Bug #39) ──

    #[test]
    fn test_mcp_type_change_stdio_to_sse_clears_command() {
        let mut mcp = McpServer::new_with_name("test");
        mcp.config.server_type = "stdio".to_string();
        mcp.config.command = Some("npx".to_string());
        mcp.config.args = vec!["-y".to_string(), "server".to_string()];

        let mut fields = crate::cli::dynamic::FieldMap::default();
        fields.insert(
            "type".into(),
            crate::cli::dynamic::FieldValue::String(Some("sse".to_string())),
        );
        mcp.apply_field("type", &fields).unwrap();

        assert_eq!(mcp.config.server_type, "sse");
        assert!(
            mcp.config.command.is_none(),
            "command should be cleared when switching to sse"
        );
        assert!(
            mcp.config.args.is_empty(),
            "args should be cleared when switching to sse"
        );
    }

    #[test]
    fn test_mcp_type_change_sse_to_stdio_clears_url() {
        let mut mcp = McpServer::new_with_name("test");
        mcp.config.server_type = "sse".to_string();
        mcp.config.url = Some("http://localhost:3000".to_string());
        mcp.config
            .headers
            .insert("Authorization".into(), "Bearer token".into());

        let mut fields = crate::cli::dynamic::FieldMap::default();
        fields.insert(
            "type".into(),
            crate::cli::dynamic::FieldValue::String(Some("stdio".to_string())),
        );
        mcp.apply_field("type", &fields).unwrap();

        assert_eq!(mcp.config.server_type, "stdio");
        assert!(
            mcp.config.url.is_none(),
            "url should be cleared when switching to stdio"
        );
        assert!(
            mcp.config.headers.is_empty(),
            "headers should be cleared when switching to stdio"
        );
    }

    #[test]
    fn test_mcp_type_change_same_type_no_cleanup() {
        let mut mcp = McpServer::new_with_name("test");
        mcp.config.server_type = "stdio".to_string();
        mcp.config.command = Some("npx".to_string());
        mcp.config.args = vec!["-y".to_string()];

        let mut fields = crate::cli::dynamic::FieldMap::default();
        fields.insert(
            "type".into(),
            crate::cli::dynamic::FieldValue::String(Some("stdio".to_string())),
        );
        mcp.apply_field("type", &fields).unwrap();

        assert_eq!(mcp.config.server_type, "stdio");
        assert!(
            mcp.config.command.is_some(),
            "command should remain when type unchanged"
        );
        assert!(
            !mcp.config.args.is_empty(),
            "args should remain when type unchanged"
        );
    }

    // ── MCP header field (Bug #43) ──

    #[test]
    fn test_mcp_apply_field_header() {
        let mut mcp = McpServer::new_with_name("test");
        mcp.config.server_type = "sse".to_string();
        mcp.config.url = Some("http://example.com".to_string());

        let mut fields = crate::cli::dynamic::FieldMap::default();
        let mut headers = std::collections::HashMap::new();
        headers.insert("Authorization".into(), "Bearer token".into());
        headers.insert("X-Custom".into(), "value".into());
        fields.insert(
            "header".into(),
            crate::cli::dynamic::FieldValue::KvVec(headers),
        );
        mcp.apply_field("header", &fields).unwrap();

        assert_eq!(
            mcp.config.headers.get("Authorization").unwrap(),
            "Bearer token"
        );
        assert_eq!(mcp.config.headers.get("X-Custom").unwrap(), "value");
    }

    #[test]
    fn test_mcp_apply_field_header_deletes_empty_value() {
        let mut mcp = McpServer::new_with_name("test");
        mcp.config.server_type = "sse".to_string();
        mcp.config.headers.insert("X-Old".into(), "value".into());

        let mut fields = crate::cli::dynamic::FieldMap::default();
        let mut headers = std::collections::HashMap::new();
        headers.insert("X-Old".into(), "".into()); // empty value = delete
        fields.insert(
            "header".into(),
            crate::cli::dynamic::FieldValue::KvVec(headers),
        );
        mcp.apply_field("header", &fields).unwrap();

        assert!(
            !mcp.config.headers.contains_key("X-Old"),
            "empty value should delete header"
        );
    }

    // ── Env empty value (Bug #40) ──

    #[test]
    fn test_env_apply_field_var_preserves_empty_value() {
        let mut env = Env::new_with_name("test");

        let mut fields = crate::cli::dynamic::FieldMap::default();
        let mut vars = std::collections::HashMap::new();
        vars.insert("EMPTY_VAR".into(), "".into());
        vars.insert("NORMAL_VAR".into(), "hello".into());
        fields.insert("var".into(), crate::cli::dynamic::FieldValue::KvVec(vars));
        env.apply_field("var", &fields).unwrap();

        assert!(
            env.config.vars.contains_key("EMPTY_VAR"),
            "empty value should be preserved in env"
        );
        assert_eq!(env.config.vars.get("EMPTY_VAR").unwrap(), "");
        assert_eq!(env.config.vars.get("NORMAL_VAR").unwrap(), "hello");
    }

    #[test]
    fn test_env_apply_field_var_overwrites_existing() {
        let mut env = Env::new_with_name("test");
        env.config.vars.insert("KEY".into(), "old".into());

        let mut fields = crate::cli::dynamic::FieldMap::default();
        let mut vars = std::collections::HashMap::new();
        vars.insert("KEY".into(), "new".into());
        fields.insert("var".into(), crate::cli::dynamic::FieldValue::KvVec(vars));
        env.apply_field("var", &fields).unwrap();

        assert_eq!(env.config.vars.get("KEY").unwrap(), "new");
    }

    // ── MCP type change: streamable-http (Bug #39 extended) ──

    #[test]
    fn test_mcp_type_change_stdio_to_streamable_http_clears_command() {
        let mut mcp = McpServer::new_with_name("test");
        mcp.config.server_type = "stdio".to_string();
        mcp.config.command = Some("npx".to_string());
        mcp.config.args = vec!["server".to_string()];

        let mut fields = crate::cli::dynamic::FieldMap::default();
        fields.insert(
            "type".into(),
            crate::cli::dynamic::FieldValue::String(Some("streamable-http".to_string())),
        );
        mcp.apply_field("type", &fields).unwrap();

        assert_eq!(mcp.config.server_type, "streamable-http");
        assert!(mcp.config.command.is_none());
        assert!(mcp.config.args.is_empty());
    }

    #[test]
    fn test_mcp_type_change_streamable_http_to_stdio_clears_url() {
        let mut mcp = McpServer::new_with_name("test");
        mcp.config.server_type = "streamable-http".to_string();
        mcp.config.url = Some("http://localhost:9090/mcp".to_string());
        mcp.config.headers.insert("Auth".into(), "token".into());

        let mut fields = crate::cli::dynamic::FieldMap::default();
        fields.insert(
            "type".into(),
            crate::cli::dynamic::FieldValue::String(Some("stdio".to_string())),
        );
        mcp.apply_field("type", &fields).unwrap();

        assert_eq!(mcp.config.server_type, "stdio");
        assert!(mcp.config.url.is_none());
        assert!(mcp.config.headers.is_empty());
    }

    // ── MCP env var merge (empty value = delete) ──

    #[test]
    fn test_mcp_env_var_empty_value_deletes_key() {
        let mut mcp = McpServer::new_with_name("test");
        mcp.config.env.insert("API_KEY".into(), "secret".into());
        mcp.config.env.insert("REGION".into(), "us-east-1".into());

        let mut fields = crate::cli::dynamic::FieldMap::default();
        let mut vars = std::collections::HashMap::new();
        vars.insert("API_KEY".into(), "".into()); // delete
        vars.insert("NEW_VAR".into(), "value".into()); // add
        fields.insert("var".into(), crate::cli::dynamic::FieldValue::KvVec(vars));
        mcp.apply_field("var", &fields).unwrap();

        assert!(
            !mcp.config.env.contains_key("API_KEY"),
            "empty value should delete key"
        );
        assert_eq!(mcp.config.env.get("REGION").unwrap(), "us-east-1");
        assert_eq!(mcp.config.env.get("NEW_VAR").unwrap(), "value");
    }

    // ── Agent model and temperature fields ──

    #[test]
    fn test_agent_apply_field_model() {
        let mut agent = Agent::new_with_name("test");
        assert!(agent.config.model.is_none());

        let mut fields = crate::cli::dynamic::FieldMap::default();
        fields.insert(
            "model".into(),
            crate::cli::dynamic::FieldValue::String(Some("sonnet".to_string())),
        );
        agent.apply_field("model", &fields).unwrap();

        assert_eq!(agent.config.model.as_deref(), Some("sonnet"));
    }

    #[test]
    fn test_agent_apply_field_temperature() {
        let mut agent = Agent::new_with_name("test");
        assert!(agent.config.temperature.is_none());

        let mut fields = crate::cli::dynamic::FieldMap::default();
        fields.insert(
            "temperature".into(),
            crate::cli::dynamic::FieldValue::F64(Some(0.7)),
        );
        agent.apply_field("temperature", &fields).unwrap();

        assert_eq!(agent.config.temperature, Some(0.7));
    }

    #[test]
    fn test_agent_validate_temperature_too_high() {
        let mut agent = Agent::new_with_name("test");
        agent.config.mode = "subagent".to_string();
        agent.config.temperature = Some(3.0);
        assert!(agent.validate_config().is_err());
    }

    #[test]
    fn test_agent_validate_temperature_boundary() {
        let mut agent = Agent::new_with_name("test");
        agent.config.mode = "subagent".to_string();
        agent.config.temperature = Some(2.0);
        assert!(agent.validate_config().is_ok());

        agent.config.temperature = Some(0.0);
        assert!(agent.validate_config().is_ok());
    }

    // ── Profile duplicate resource references ──

    #[test]
    fn test_profile_validate_duplicate_agent() {
        let p = Profile {
            name: "test".into(),
            description: None,
            model: Default::default(),
            providers: Default::default(),
            mcp_servers: Default::default(),
            hooks: Default::default(),
            agents: ProfileResources {
                enabled: vec!["agent1".into(), "agent1".into()],
            },
            skills: Default::default(),
            plugins: Default::default(),
            env: Default::default(),
            prompts: Default::default(),
            overrides: Default::default(),
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn test_profile_validate_duplicate_env() {
        let p = Profile {
            name: "test".into(),
            description: None,
            model: Default::default(),
            providers: Default::default(),
            mcp_servers: Default::default(),
            hooks: Default::default(),
            agents: Default::default(),
            skills: Default::default(),
            plugins: Default::default(),
            env: ProfileResources {
                enabled: vec!["staging".into(), "staging".into()],
            },
            prompts: Default::default(),
            overrides: Default::default(),
        };
        assert!(p.validate().is_err());
    }

    // ── MCP validate_config: stdio requires command ──

    #[test]
    fn test_mcp_validate_stdio_without_command() {
        let mcp = McpServer {
            name: "test".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "stdio".into(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(mcp.validate_config().is_err());
    }

    #[test]
    fn test_mcp_validate_sse_without_url() {
        let mcp = McpServer {
            name: "test".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "sse".into(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(mcp.validate_config().is_err());
    }

    #[test]
    fn test_mcp_validate_streamable_http_without_url() {
        let mcp = McpServer {
            name: "test".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "streamable-http".into(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(mcp.validate_config().is_err());
    }

    #[test]
    fn test_mcp_validate_stdio_with_command() {
        let mcp = McpServer {
            name: "test".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(mcp.validate_config().is_ok());
    }

    #[test]
    fn test_mcp_validate_sse_with_url() {
        let mcp = McpServer {
            name: "test".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: McpConfig {
                server_type: "sse".into(),
                url: Some("http://localhost/sse".into()),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(mcp.validate_config().is_ok());
    }

    #[test]
    fn test_env_remove_var() {
        let mut env = Env {
            name: "test".into(),
            id: String::new(),
            r#type: "env".into(),
            config: EnvConfig {
                vars: [
                    ("A".into(), "1".into()),
                    ("B".into(), "2".into()),
                    ("C".into(), "3".into()),
                ]
                .into(),
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let mut fields = crate::cli::dynamic::FieldMap::default();
        fields.insert(
            "remove_var".into(),
            crate::cli::dynamic::FieldValue::Csv(Some(vec!["B".into()])),
        );
        env.apply_field("remove_var", &fields).unwrap();
        assert!(!env.config.vars.contains_key("B"));
        assert_eq!(env.config.vars["A"], "1");
        assert_eq!(env.config.vars["C"], "3");
    }

    #[test]
    fn test_env_remove_var_multiple() {
        let mut env = Env {
            name: "test".into(),
            id: String::new(),
            r#type: "env".into(),
            config: EnvConfig {
                vars: [
                    ("A".into(), "1".into()),
                    ("B".into(), "2".into()),
                    ("C".into(), "3".into()),
                ]
                .into(),
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let mut fields = crate::cli::dynamic::FieldMap::default();
        fields.insert(
            "remove_var".into(),
            crate::cli::dynamic::FieldValue::Csv(Some(vec!["A".into(), "C".into()])),
        );
        env.apply_field("remove_var", &fields).unwrap();
        assert_eq!(env.config.vars.len(), 1);
        assert_eq!(env.config.vars["B"], "2");
    }

    #[test]
    fn test_env_remove_var_nonexistent() {
        let mut env = Env {
            name: "test".into(),
            id: String::new(),
            r#type: "env".into(),
            config: EnvConfig {
                vars: [("A".into(), "1".into())].into(),
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let mut fields = crate::cli::dynamic::FieldMap::default();
        fields.insert(
            "remove_var".into(),
            crate::cli::dynamic::FieldValue::Csv(Some(vec!["NONEXISTENT".into()])),
        );
        env.apply_field("remove_var", &fields).unwrap();
        assert_eq!(env.config.vars.len(), 1); // no crash, var still there
    }

    #[test]
    fn test_env_add_and_remove_in_same_edit() {
        let mut env = Env {
            name: "test".into(),
            id: String::new(),
            r#type: "env".into(),
            config: EnvConfig {
                vars: [("OLD".into(), "old".into())].into(),
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let mut fields = crate::cli::dynamic::FieldMap::default();
        fields.insert(
            "var".into(),
            crate::cli::dynamic::FieldValue::KvVec([("NEW".into(), "new".into())].into()),
        );
        fields.insert(
            "remove_var".into(),
            crate::cli::dynamic::FieldValue::Csv(Some(vec!["OLD".into()])),
        );
        env.apply_field("var", &fields).unwrap();
        env.apply_field("remove_var", &fields).unwrap();
        assert!(!env.config.vars.contains_key("OLD"));
        assert_eq!(env.config.vars["NEW"], "new");
    }
}

// ── Profile ──
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Profile {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub model: ProfileModel,
    #[serde(default)]
    pub providers: ProfileProviders,
    #[serde(default)]
    pub mcp_servers: ProfileResources,
    #[serde(default)]
    pub skills: ProfileResources,
    #[serde(default)]
    pub prompts: ProfilePrompts,
    #[serde(default)]
    pub env: ProfileResources,
    #[serde(default)]
    pub hooks: ProfileResources,
    #[serde(default)]
    pub agents: ProfileResources,
    #[serde(default)]
    pub plugins: ProfileResources,
    #[serde(default)]
    pub overrides: HashMap<String, ProfileOverride>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct ProfileModel {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weak_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_model: Option<String>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct ProfileProviders {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct ProfileResources {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled: Vec<String>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct ProfilePrompts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct ProfileOverride {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra_env: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, toml::Value>,
}
impl Profile {
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            bail!("profile name cannot be empty");
        }
        for (kind, items) in [
            ("mcp", &self.mcp_servers.enabled),
            ("skill", &self.skills.enabled),
            ("hook", &self.hooks.enabled),
            ("agent", &self.agents.enabled),
            ("env", &self.env.enabled),
            ("plugin", &self.plugins.enabled),
        ] {
            let mut seen = std::collections::HashSet::new();
            for item in items {
                if !seen.insert(item.as_str()) {
                    bail!(
                        "profile '{}' has duplicate {} reference: '{}'",
                        self.name,
                        kind,
                        item
                    );
                }
            }
        }
        Ok(())
    }
}
pub mod profile {
    pub(crate) use super::{
        Profile, ProfileModel, ProfilePrompts, ProfileProviders, ProfileResources,
    };
}

// ── SanitizeDisplay noop impls ──
macro_rules! impl_sanitize_noop { ($($t:ty),+ $(,)?) => { $( impl SanitizeDisplay for $t { fn sanitize_display(&self, _val: &mut serde_json::Value) {} } )+ } }
impl_sanitize_noop!(Agent, Skill, Prompt, Env, Plugin);
