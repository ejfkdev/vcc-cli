pub mod agent_format;
pub mod doc_engine;
pub mod generic;
pub mod mapping;
pub mod plugin_manifest;
pub mod provider;

use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::model::{
    env::Env, hook::Hook, mcp::McpServer, profile::Profile, prompt::Prompt, provider::Provider,
};
use crate::store::TomlStore;

#[derive(Debug, Default)]
pub(crate) struct SyncResult {
    pub created: Vec<SyncItem>,
    pub updated: Vec<SyncItem>,
    pub skipped: Vec<SyncItem>,
}

#[derive(Debug, Clone)]
pub(crate) struct SyncItem {
    pub category: String,
    pub name: String,
}

impl SyncItem {
    pub fn new(category: &str, name: &str) -> Self {
        Self {
            category: category.to_string(),
            name: name.to_string(),
        }
    }
}
impl SyncResult {
    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.updated.is_empty() && self.skipped.is_empty()
    }
    pub fn merge(&mut self, other: SyncResult) {
        self.created.extend(other.created);
        self.updated.extend(other.updated);
        self.skipped.extend(other.skipped);
    }
}

#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct InspectResult {
    pub tool: String,
    pub config_dir: Option<String>,
    pub sections: Vec<InspectSection>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct InspectSection {
    pub kind: String,
    pub items: Vec<InspectItem>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct InspectItem {
    pub name: String,
    pub enabled: bool,
    pub detail: String,
}

pub(crate) trait Adapter: Send + Sync {
    fn tool_name(&self) -> &str;
    fn config_dir(&self) -> Option<PathBuf>;
    /// Returns true if config directory exists on disk.
    fn has_config_dir(&self) -> bool {
        self.config_dir().is_some_and(|d| d.exists())
    }
    fn apply_defaults(
        &self,
        store: &TomlStore,
        should_apply: &dyn Fn(&str) -> bool,
        dry_run: bool,
    ) -> Result<usize> {
        let _ = (store, should_apply, dry_run);
        Ok(0)
    }
    fn apply_provider(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize>;
    fn apply_mcp(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize>;
    fn apply_hook(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize> {
        let _ = (store, profile, dry_run);
        Ok(0)
    }
    fn apply_env(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize>;
    fn apply_settings_batch(
        &self,
        store: &TomlStore,
        profile: &Profile,
        dry_run: bool,
        should_apply: &dyn Fn(&str) -> bool,
    ) -> Result<usize> {
        let mut total = 0;
        if should_apply("mcp") {
            total += self.apply_mcp(store, profile, dry_run)?;
        }
        if should_apply("hook") {
            total += self.apply_hook(store, profile, dry_run)?;
        }
        if should_apply("env") {
            total += self.apply_env(store, profile, dry_run)?;
        }
        Ok(total)
    }
    fn apply_agent(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize> {
        let _ = (store, profile, dry_run);
        Ok(0)
    }
    fn apply_skill(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize>;
    fn apply_plugin(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize> {
        let _ = (store, profile, dry_run);
        Ok(0)
    }
    fn apply_prompt(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize>;
    fn add_resource(
        &self,
        _kind: &str,
        _store: &TomlStore,
        _names: &[String],
        _dry_run: bool,
    ) -> Result<usize> {
        Ok(0)
    }
    fn remove_resource(
        &self,
        _kind: &str,
        _store: &TomlStore,
        _names: &[String],
        _dry_run: bool,
    ) -> Result<usize> {
        Ok(0)
    }
    fn toggle_resource(
        &self,
        _kind: &str,
        _enable: bool,
        _store: &TomlStore,
        _names: &[String],
        _dry_run: bool,
    ) -> Result<usize> {
        Ok(0)
    }
    fn sync(&self, store: &TomlStore, dry_run: bool) -> Result<SyncResult> {
        let _ = (store, dry_run);
        Ok(SyncResult::default())
    }
    fn inspect(&self) -> Result<InspectResult> {
        Ok(InspectResult::default())
    }
}

pub(crate) fn all_adapters() -> Vec<Box<dyn Adapter>> {
    crate::config::resource_registry()
        .tools
        .adapters
        .iter()
        .filter_map(|name| {
            generic::GenericAdapter::new(name)
                .ok()
                .map(|a| Box::new(a) as Box<dyn Adapter>)
        })
        .collect()
}
pub(crate) fn get_adapter(name: &str) -> Result<Option<Box<dyn Adapter>>> {
    if crate::config::adapter_mapping_content(name).is_some() {
        Ok(Some(
            Box::new(generic::GenericAdapter::new(name)?) as Box<dyn Adapter>
        ))
    } else {
        Ok(None)
    }
}

/// Returns a comma-separated list of supported tool names.
pub(crate) fn supported_tool_names() -> String {
    all_adapters()
        .iter()
        .map(|a| a.tool_name().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Validate that a tool name is supported, returning an error if not.
pub(crate) fn validate_tool_name(tool: &str) -> Result<()> {
    if get_adapter(tool)?.is_none() {
        anyhow::bail!(
            "unsupported tool: '{}'. Supported: {}",
            tool,
            supported_tool_names()
        );
    }
    Ok(())
}

pub(crate) fn read_jsonc_with_fallback(paths: &[PathBuf]) -> Result<(serde_json::Value, PathBuf)> {
    for path in paths {
        if !path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(path)?;
        let stripped = strip_jsonc(&content);
        if let Ok(val) = serde_json::from_str(&stripped) {
            return Ok((val, path.clone()));
        }
    }
    let fallback = paths
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no config paths provided"))?;
    Ok((serde_json::json!({}), fallback))
}

fn strip_jsonc(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            result.push(c);
            if c == '\\' {
                if let Some(escaped) = chars.next() {
                    result.push(escaped);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                result.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                while chars.peek().is_some_and(|&c| c != '\n') {
                    chars.next();
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                loop {
                    match chars.next() {
                        Some('*') if chars.peek() == Some(&'/') => {
                            chars.next();
                            break;
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
            }
            _ => {
                result.push(c);
            }
        }
    }
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r",([\s]*[\]\}])").unwrap());
    re.replace_all(&result, "$1").to_string()
}

fn resolve_with_override<T: Clone, O>(
    resource: &T,
    tool_name: &str,
    get_overrides: impl Fn(&T) -> &HashMap<String, O>,
    merge_fn: impl FnOnce(&mut T, &O),
) -> T {
    let Some(override_) = get_overrides(resource).get(tool_name) else {
        return resource.clone();
    };
    let mut resolved = resource.clone();
    merge_fn(&mut resolved, override_);
    resolved
}

macro_rules! define_resolve {
    ($fn_name:ident, $type:ty, $merge:expr) => {
        pub fn $fn_name(resource: &$type, tool_name: &str) -> $type {
            resolve_with_override(resource, tool_name, |r| &r.tool, $merge)
        }
    };
}
define_resolve!(
    resolve_provider,
    Provider,
    |p: &mut Provider, o: &crate::model::provider::ProviderToolOverride| {
        if let Some(ref v) = o.api_key {
            p.config.api_key = v.clone();
        }
        if let Some(ref v) = o.base_url {
            p.config.base_url = Some(v.clone());
        }
        if let Some(ref v) = o.npm {
            p.config.npm = Some(v.clone());
        }
        if let Some(ref v) = o.default_model {
            p.config.default_model = Some(v.clone());
        }
        if !o.models.is_empty() {
            p.config.models = o.models.clone();
        }
        for (k, v) in &o.headers {
            p.config.headers.insert(k.clone(), v.clone());
        }
        for (k, v) in &o.env {
            p.config.env.insert(k.clone(), v.clone());
        }
    }
);
define_resolve!(
    resolve_mcp,
    McpServer,
    |m: &mut McpServer, o: &crate::model::mcp::McpToolOverride| {
        if let Some(ref v) = o.command {
            m.config.command = Some(v.clone());
        }
        if let Some(ref v) = o.args {
            m.config.args = v.clone();
        }
        for (k, v) in &o.env {
            m.config.env.insert(k.clone(), v.clone());
        }
        if let Some(ref v) = o.url {
            m.config.url = Some(v.clone());
        }
        for (k, v) in &o.headers {
            m.config.headers.insert(k.clone(), v.clone());
        }
        for t in &o.disabled_tools {
            if !m.config.disabled_tools.contains(t) {
                m.config.disabled_tools.push(t.clone());
            }
        }
    }
);
define_resolve!(
    resolve_env,
    Env,
    |e: &mut Env, o: &crate::model::env::EnvToolOverride| {
        for (k, v) in &o.vars {
            e.config.vars.insert(k.clone(), v.clone());
        }
    }
);
define_resolve!(
    resolve_hook,
    Hook,
    |h: &mut Hook, o: &crate::model::hook::HookToolOverride| {
        if let Some(ref v) = o.matcher {
            h.config.matcher = v.clone();
        }
        if let Some(ref v) = o.command {
            h.config.command = v.clone();
        }
        if let Some(v) = o.timeout {
            h.config.timeout = v;
        }
    }
);
define_resolve!(
    resolve_prompt,
    Prompt,
    |p: &mut Prompt, o: &crate::model::prompt::PromptToolOverride| {
        if let Some(ref v) = o.content {
            p.config.content = v.clone();
        }
    }
);

pub(crate) fn load_default<T: crate::model::Resource + Clone>(
    store: &TomlStore,
    kind: &str,
    tool_name: &str,
    resolve: fn(&T, &str) -> T,
) -> Result<Option<T>> {
    let Some(default) = store.load_default_resource::<T>(kind)? else {
        return Ok(None);
    };
    Ok(Some(resolve(&default, tool_name)))
}

/// Get the name of the default resource for a kind, if one exists
pub(crate) fn load_default_name(store: &TomlStore, kind: &str, tool_name: &str) -> Option<String> {
    match kind {
        "env" => load_default::<crate::model::env::Env>(store, kind, tool_name, resolve_env)
            .ok()
            .flatten()
            .map(|e| e.name),
        "mcp" => load_default::<McpServer>(store, kind, tool_name, resolve_mcp)
            .ok()
            .flatten()
            .map(|m| m.name),
        _ => None,
    }
}

pub(crate) fn apply_profile_override(profile: &Profile, tool_name: &str) -> ProfileOverrideResult {
    let mut result = ProfileOverrideResult::default();
    if let Some(o) = profile.overrides.get(tool_name) {
        result.extra_env = o.extra_env.clone();
        result.default_model = o.default_model.clone();
    }
    result
}

#[derive(Default)]
pub(crate) struct ProfileOverrideResult {
    pub extra_env: HashMap<String, String>,
    pub default_model: Option<String>,
}

pub(crate) fn json_to_toml_value(val: &serde_json::Value) -> toml::Value {
    use doc_engine::DocValue;
    let doc: DocValue = val.clone().into();
    doc.into()
}

pub(crate) fn toml_to_json_value(val: &toml::Value) -> serde_json::Value {
    use doc_engine::DocValue;
    let doc: DocValue = val.clone().into();
    doc.into()
}

pub(crate) fn merge_extra_to_json(
    target: &mut serde_json::Map<String, serde_json::Value>,
    extra: &std::collections::HashMap<String, toml::Value>,
) {
    for (k, v) in extra {
        target.insert(k.clone(), toml_to_json_value(v));
    }
}

pub(crate) fn merge_extra_to_json_with_map(
    target: &mut serde_json::Map<String, serde_json::Value>,
    extra: &std::collections::HashMap<String, toml::Value>,
    field_map: &std::collections::HashMap<String, String>,
) {
    for (k, v) in extra {
        let json_key = field_map.get(k).map(|s| s.as_str()).unwrap_or(k);
        target.insert(json_key.to_string(), toml_to_json_value(v));
    }
}

pub(crate) fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let meta = match std::fs::symlink_metadata(&src_path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_symlink() {
            continue;
        }
        let dst_path = dst.join(entry.file_name());
        if meta.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── strip_jsonc ──

    #[test]
    fn test_strip_jsonc_line_comments() {
        let input = r#"{ "key": "value" // comment
}"#;
        let stripped = strip_jsonc(input);
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn test_strip_jsonc_block_comments() {
        let input = r#"{ "key" /* comment */: "value" }"#;
        let stripped = strip_jsonc(input);
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn test_strip_jsonc_multiline_block_comment() {
        let input = "{ \"a\": 1 /* line1\nline2\nline3 */, \"b\": 2 }";
        let stripped = strip_jsonc(input);
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["b"], 2);
    }

    #[test]
    fn test_strip_jsonc_trailing_comma_in_array() {
        let input = "[1, 2, 3,]";
        let stripped = strip_jsonc(input);
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed, serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn test_strip_jsonc_trailing_comma_in_object() {
        let input = r#"{ "key": "value", }"#;
        let stripped = strip_jsonc(input);
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn test_strip_jsonc_comment_in_string_preserved() {
        let input = r#"{ "url": "https://example.com/path" }"#;
        let stripped = strip_jsonc(input);
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed["url"], "https://example.com/path");
    }

    #[test]
    fn test_strip_jsonc_plain_json_unchanged() {
        let input = "{\"key\": \"value\", \"num\": 42}";
        let stripped = strip_jsonc(input);
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed["key"], "value");
        assert_eq!(parsed["num"], 42);
    }

    #[test]
    fn test_strip_jsonc_escaped_quote_in_string() {
        let input = r#"{ "key": "value with \" quote" }"#;
        let stripped = strip_jsonc(input);
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed["key"], "value with \" quote");
    }

    // ── apply_profile_override ──

    #[test]
    fn test_apply_profile_override_with_override() {
        let mut po = crate::model::ProfileOverride::default();
        po.extra_env.insert("KEY".into(), "VALUE".into());
        po.default_model = Some("gpt-4".into());
        let mut override_map = HashMap::new();
        override_map.insert("claude".into(), po);
        let profile = Profile {
            name: "test".into(),
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
            overrides: override_map,
        };
        let result = apply_profile_override(&profile, "claude");
        assert_eq!(result.extra_env.get("KEY").unwrap(), "VALUE");
        assert_eq!(result.default_model, Some("gpt-4".into()));
    }

    #[test]
    fn test_apply_profile_override_no_override() {
        let profile = Profile {
            name: "test".into(),
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
            overrides: HashMap::new(),
        };
        let result = apply_profile_override(&profile, "claude");
        assert!(result.extra_env.is_empty());
        assert!(result.default_model.is_none());
    }

    // ── resolve_provider ──

    #[test]
    fn test_resolve_provider_no_override() {
        let provider = Provider {
            name: "test".into(),
            id: String::new(),
            r#type: "provider".into(),
            config: crate::model::ProviderConfig {
                provider_type: "openai".into(),
                api_key: "sk-123".into(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let resolved = resolve_provider(&provider, "claude");
        assert_eq!(resolved.config.api_key, "sk-123");
    }

    #[test]
    fn test_resolve_provider_with_override() {
        let provider = Provider {
            name: "test".into(),
            id: String::new(),
            r#type: "provider".into(),
            config: crate::model::ProviderConfig {
                provider_type: "openai".into(),
                api_key: "sk-123".into(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::from([(
                "claude".into(),
                crate::model::provider::ProviderToolOverride {
                    api_key: Some("sk-override".into()),
                    ..Default::default()
                },
            )]),
        };
        let resolved = resolve_provider(&provider, "claude");
        assert_eq!(resolved.config.api_key, "sk-override");
    }

    // ── SyncResult ──

    #[test]
    fn test_sync_result_is_empty() {
        let r = SyncResult::default();
        assert!(r.is_empty());
    }

    #[test]
    fn test_sync_result_not_empty() {
        let mut r = SyncResult::default();
        r.created.push(SyncItem::new("mcp", "fs"));
        assert!(!r.is_empty());
    }

    #[test]
    fn test_sync_result_merge() {
        let mut a = SyncResult::default();
        a.created.push(SyncItem::new("mcp", "fs"));
        let mut b = SyncResult::default();
        b.updated.push(SyncItem::new("hook", "test"));
        a.merge(b);
        assert_eq!(a.created.len(), 1);
        assert_eq!(a.updated.len(), 1);
    }

    // ── json_to_toml_value / toml_to_json_value ──

    #[test]
    fn test_json_toml_roundtrip() {
        let json = serde_json::json!({"name": "test", "count": 42, "active": true});
        let toml_val = json_to_toml_value(&json);
        let back = toml_to_json_value(&toml_val);
        assert_eq!(json, back);
    }

    // ── resolve_mcp ──

    #[test]
    fn test_resolve_mcp_no_override() {
        let mcp = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: crate::model::mcp::McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let resolved = resolve_mcp(&mcp, "claude");
        assert_eq!(resolved.config.command.as_deref(), Some("npx"));
    }

    #[test]
    fn test_resolve_mcp_with_override() {
        use crate::model::mcp::McpToolOverride;
        let mut tool = HashMap::new();
        tool.insert(
            "claude".into(),
            McpToolOverride {
                command: Some("python".into()),
                disabled_tools: vec!["*".into()],
                ..Default::default()
            },
        );
        let mcp = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: crate::model::mcp::McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                ..Default::default()
            },
            metadata: Default::default(),
            tool,
        };
        let resolved = resolve_mcp(&mcp, "claude");
        assert_eq!(resolved.config.command.as_deref(), Some("python"));
        assert!(resolved.config.disabled_tools.contains(&"*".to_string()));
    }

    #[test]
    fn test_resolve_mcp_different_tool() {
        use crate::model::mcp::McpToolOverride;
        let mut tool = HashMap::new();
        tool.insert(
            "codex".into(),
            McpToolOverride {
                command: Some("other".into()),
                ..Default::default()
            },
        );
        let mcp = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: crate::model::mcp::McpConfig {
                server_type: "stdio".into(),
                command: Some("npx".into()),
                ..Default::default()
            },
            metadata: Default::default(),
            tool,
        };
        let resolved = resolve_mcp(&mcp, "claude");
        assert_eq!(resolved.config.command.as_deref(), Some("npx")); // no override for claude
    }

    // ── resolve_env ──

    #[test]
    fn test_resolve_env_with_override() {
        use crate::model::env::EnvToolOverride;
        let mut tool = HashMap::new();
        let mut vars = HashMap::new();
        vars.insert("KEY".into(), "override-val".into());
        tool.insert("claude".into(), EnvToolOverride { vars });
        let mut config_vars = HashMap::new();
        config_vars.insert("KEY".into(), "original".into());
        let env = Env {
            name: "test".into(),
            id: String::new(),
            r#type: "env".into(),
            config: crate::model::EnvConfig { vars: config_vars },
            metadata: Default::default(),
            tool,
        };
        let resolved = resolve_env(&env, "claude");
        assert_eq!(resolved.config.vars.get("KEY").unwrap(), "override-val");
    }

    #[test]
    fn test_resolve_env_no_override() {
        let env = Env {
            name: "test".into(),
            id: String::new(),
            r#type: "env".into(),
            config: crate::model::EnvConfig {
                vars: HashMap::new(),
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let resolved = resolve_env(&env, "claude");
        assert!(resolved.config.vars.is_empty());
    }

    // ── resolve_hook ──

    #[test]
    fn test_resolve_hook_with_override() {
        use crate::model::hook::HookToolOverride;
        let mut tool = HashMap::new();
        tool.insert(
            "claude".into(),
            HookToolOverride {
                matcher: Some("Bash".into()),
                command: Some("new-cmd".into()),
                timeout: Some(60),
                extra: HashMap::new(),
            },
        );
        let hook = Hook {
            name: "test".into(),
            id: String::new(),
            r#type: "hook".into(),
            config: crate::model::HookConfig {
                event: "PreToolUse".into(),
                matcher: String::new(),
                command: "old-cmd".into(),
                timeout: 30,
            },
            metadata: Default::default(),
            tool,
        };
        let resolved = resolve_hook(&hook, "claude");
        assert_eq!(resolved.config.command, "new-cmd");
        assert_eq!(resolved.config.matcher, "Bash");
        assert_eq!(resolved.config.timeout, 60);
    }

    #[test]
    fn test_resolve_hook_partial_override() {
        use crate::model::hook::HookToolOverride;
        let mut tool = HashMap::new();
        tool.insert(
            "claude".into(),
            HookToolOverride {
                matcher: None,
                command: None,
                timeout: Some(120),
                extra: HashMap::new(),
            },
        );
        let hook = Hook {
            name: "test".into(),
            id: String::new(),
            r#type: "hook".into(),
            config: crate::model::HookConfig {
                event: "PreToolUse".into(),
                matcher: "Bash".into(),
                command: "cmd".into(),
                timeout: 30,
            },
            metadata: Default::default(),
            tool,
        };
        let resolved = resolve_hook(&hook, "claude");
        assert_eq!(resolved.config.timeout, 120);
        assert_eq!(resolved.config.command, "cmd"); // unchanged
    }

    // ── resolve_prompt ──

    #[test]
    fn test_resolve_prompt_with_override() {
        use crate::model::prompt::PromptToolOverride;
        let mut tool = HashMap::new();
        tool.insert(
            "claude".into(),
            PromptToolOverride {
                content: Some("overridden".into()),
            },
        );
        let prompt = Prompt {
            name: "test".into(),
            id: String::new(),
            r#type: "prompt".into(),
            config: crate::model::PromptConfig {
                content: "original".into(),
            },
            metadata: Default::default(),
            tool,
        };
        let resolved = resolve_prompt(&prompt, "claude");
        assert_eq!(resolved.config.content, "overridden");
    }

    #[test]
    fn test_resolve_prompt_no_override() {
        let prompt = Prompt {
            name: "test".into(),
            id: String::new(),
            r#type: "prompt".into(),
            config: crate::model::PromptConfig {
                content: "original".into(),
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let resolved = resolve_prompt(&prompt, "claude");
        assert_eq!(resolved.config.content, "original");
    }

    // ── merge_extra_to_json ──

    #[test]
    fn test_merge_extra_to_json_basic() {
        let mut target = serde_json::Map::new();
        let mut extra = HashMap::new();
        extra.insert("custom".into(), toml::Value::String("hello".into()));
        merge_extra_to_json(&mut target, &extra);
        assert_eq!(target.get("custom").unwrap(), "hello");
    }

    #[test]
    fn test_merge_extra_to_json_overwrites() {
        let mut target = serde_json::Map::new();
        target.insert("key".into(), serde_json::json!("old"));
        let mut extra = HashMap::new();
        extra.insert("key".into(), toml::Value::String("new".into()));
        merge_extra_to_json(&mut target, &extra);
        assert_eq!(target.get("key").unwrap(), "new");
    }

    #[test]
    fn test_merge_extra_to_json_empty() {
        let mut target = serde_json::Map::new();
        let extra: HashMap<String, toml::Value> = HashMap::new();
        merge_extra_to_json(&mut target, &extra);
        assert!(target.is_empty());
    }

    // ── merge_extra_to_json_with_map ──

    #[test]
    fn test_merge_extra_to_json_with_map_remapped() {
        let mut target = serde_json::Map::new();
        let mut extra = HashMap::new();
        extra.insert("api_key".into(), toml::Value::String("sk-123".into()));
        let mut field_map = HashMap::new();
        field_map.insert("api_key".into(), "apiKey".into());
        merge_extra_to_json_with_map(&mut target, &extra, &field_map);
        assert!(target.get("apiKey").is_some());
        assert!(target.get("api_key").is_none());
    }

    #[test]
    fn test_merge_extra_to_json_with_map_no_mapping() {
        let mut target = serde_json::Map::new();
        let mut extra = HashMap::new();
        extra.insert("custom".into(), toml::Value::String("val".into()));
        let field_map: HashMap<String, String> = HashMap::new();
        merge_extra_to_json_with_map(&mut target, &extra, &field_map);
        assert!(target.get("custom").is_some()); // uses original key
    }

    // ── resolve_with_override (generic) ──

    #[test]
    fn test_resolve_with_override_returns_clone_when_no_match() {
        let p = Provider {
            name: "test".into(),
            id: String::new(),
            r#type: "provider".into(),
            config: crate::model::ProviderConfig {
                api_key: "sk-orig".into(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let resolved = resolve_provider(&p, "nonexistent");
        assert_eq!(resolved.config.api_key, "sk-orig");
    }
}
