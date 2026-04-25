//! Config-driven generic CRUD operations for DocTree-based resources.
//!
//! These functions are independent of specific resource types — they operate
//! on DocTree's generic set_entry/remove_from_object/push/retain_in_array
//! operations, parameterized by section keys and disabled keys from TOML config.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::adapter::doc_engine::{DocFormat, DocTree, DocValue};
use crate::adapter::generic::helpers::{load_or_skip, parse_plugin_ref};
use crate::adapter::mapping::ToolMapping;
use crate::model::env::Env;
use crate::store::TomlStore;

// ── Object-map CRUD ──────────────────────────────────────────────────────────

/// Remove entries from an object-map section by exact key match.
pub(crate) fn object_map_remove(doc: &mut DocTree, section_key: &str, names: &[String]) -> usize {
    names
        .iter()
        .filter(|n| doc.remove_from_object(section_key, n))
        .count()
}

/// Enable entries by removing the disabled key from each entry's object.
pub(crate) fn object_map_enable(
    doc: &mut DocTree,
    section_key: &str,
    toggle_key: &str,
    uses_enabled_semantic: bool,
    names: &[String],
) -> usize {
    doc.ensure_object(section_key);
    let mut applied = 0;
    for name in names {
        if let Some(entry) = doc.get_entry_mut(section_key, name) {
            if let DocValue::Object(map) = entry {
                if uses_enabled_semantic {
                    // "enabled" semantic: set enabled=true
                    map.insert(toggle_key.to_string(), DocValue::Bool(true));
                } else {
                    // "disabled" semantic: remove disabled key
                    map.remove(toggle_key);
                }
            }
            applied += 1;
        }
    }
    applied
}

/// Disable entries by setting the toggle field appropriately.
/// For "disabled" semantic: sets disabled=true.
/// For "enabled" semantic: sets enabled=false.
pub(crate) fn object_map_disable(
    doc: &mut DocTree,
    section_key: &str,
    toggle_key: &str,
    uses_enabled_semantic: bool,
    names: &[String],
) -> usize {
    doc.ensure_object(section_key);
    let mut applied = 0;
    for name in names {
        if let Some(entry) = doc.get_entry_mut(section_key, name) {
            if let DocValue::Object(map) = entry {
                if uses_enabled_semantic {
                    // "enabled" semantic: set enabled=false
                    map.insert(toggle_key.to_string(), DocValue::Bool(false));
                } else {
                    // "disabled" semantic: set disabled=true
                    map.insert(toggle_key.to_string(), DocValue::Bool(true));
                }
            }
            applied += 1;
        } else {
            // Entry not in this tool's config — skip silently; caller summarizes
        }
    }
    applied
}

// ── Env-specific CRUD ────────────────────────────────────────────────────────

/// Set env vars from named env resources into a DocTree section.
/// Delegates to `helpers::set_env_vars_in_doc`.
pub(crate) fn env_upsert(
    doc: &mut DocTree,
    section_key: &str,
    store: &TomlStore,
    names: &[String],
    tool_name: &str,
    resolve_fn: fn(&Env, &str) -> Env,
    extra_env: Option<&HashMap<String, String>>,
) -> usize {
    super::helpers::set_env_vars_in_doc(
        doc,
        section_key,
        store,
        names,
        tool_name,
        resolve_fn,
        extra_env,
    )
}

/// Remove env vars by name from a DocTree section.
/// Loads each env resource, resolves it, and removes all its keys.
pub(crate) fn env_remove(
    doc: &mut DocTree,
    section_key: &str,
    store: &TomlStore,
    names: &[String],
    tool_name: &str,
    resolve_fn: fn(&Env, &str) -> Env,
) -> usize {
    let mut removed = 0;
    for name in names {
        let raw: Env = match load_or_skip(store, "env", name) {
            Some(e) => e,
            None => continue,
        };
        for k in resolve_fn(&raw, tool_name).config.vars.keys() {
            if doc.remove_from_object(section_key, k) {
                removed += 1;
            }
        }
    }
    removed
}

// ── Config path resolution ───────────────────────────────────────────────────

/// Resolve the env config path from mapping config.
/// Returns None if env is not supported (no section_key or no path).
pub(crate) fn env_config_path(mapping: &ToolMapping) -> Option<(PathBuf, DocFormat)> {
    let section_key = mapping.env.section_key.as_deref()?;
    if section_key.is_empty() {
        return None;
    }
    let dir = mapping.resolved_config_dir()?;
    let path = dir.join(
        mapping
            .env
            .path
            .as_deref()
            .unwrap_or(mapping.settings_file()),
    );
    Some((path, DocFormat::from_format_str(mapping.env.format_str())))
}

// ── Plugin ref extraction ────────────────────────────────────────────────────

/// Extract plugin refs from a DocTree based on the plugin format.
/// Returns a list of (name, marketplace, options) tuples.
/// - "enabled_list": array of strings
/// - "enabled_map"/"toml_table": object entries (keys are ref names)
/// - "json_array": array of strings or [name, opts] tuples
#[allow(clippy::type_complexity)]
pub(crate) fn plugin_refs_from_doc(
    doc: &DocTree,
    format: &str,
    pk: &str,
) -> Vec<(
    String,
    Option<String>,
    Option<serde_json::Map<String, serde_json::Value>>,
)> {
    match format {
        "enabled_list" => doc
            .get(pk)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        v.as_str().map(|s| {
                            let (name, mp) = parse_plugin_ref(s);
                            (name, mp, None)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "enabled_map" | "toml_table" => doc
            .entries(pk)
            .map(|e| {
                e.iter()
                    .map(|(k, _)| {
                        let (name, mp) = parse_plugin_ref(k);
                        (name, mp, None)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "json_array" => doc
            .get(pk)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| match item.as_str() {
                        Some(s) => {
                            let (name, mp) = parse_plugin_ref(s);
                            Some((name, mp, None))
                        }
                        None => {
                            let a = item.as_array()?;
                            let n = a.first()?.as_str()?.to_string();
                            let opts: Option<serde_json::Map<String, serde_json::Value>> =
                                a.get(1).and_then(|v| v.as_object()).map(|obj| {
                                    let json_val: serde_json::Value =
                                        DocValue::Object(obj.clone()).into();
                                    json_val.as_object().cloned().unwrap_or_default()
                                });
                            let (name, mp) = parse_plugin_ref(&n);
                            Some((name, mp, opts))
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Inspect-item info for a plugin entry: (name, enabled, detail).
pub(crate) struct PluginInspectInfo {
    pub name: String,
    pub enabled: bool,
    pub detail: String,
}

/// Build inspect info from a DocTree's plugin entries.
/// Returns a list of PluginInspectInfo suitable for the inspect command.
pub(crate) fn plugin_inspect_from_doc(
    doc: &DocTree,
    format: &str,
    pk: &str,
    disabled_key: &str,
) -> Vec<PluginInspectInfo> {
    match format {
        "enabled_list" | "json_array" => doc
            .get(pk)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        item.as_str().map(|s| {
                            let (name, _) = parse_plugin_ref(s);
                            PluginInspectInfo {
                                name,
                                enabled: true,
                                detail: s.to_string(),
                            }
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "enabled_map" => doc
            .entries(pk)
            .map(|e| {
                e.iter()
                    .map(|(ref_name, val)| {
                        let (name, mp) = parse_plugin_ref(ref_name);
                        // For enabled_map, the value is a bool directly (true = enabled)
                        let enabled = val.as_bool().unwrap_or(true);
                        PluginInspectInfo {
                            name,
                            enabled,
                            detail: mp
                                .map(|m| format!("marketplace: {}", m))
                                .unwrap_or_else(|| "configured".into()),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "toml_table" => doc
            .entries(pk)
            .map(|e| {
                e.iter()
                    .map(|(ref_name, val)| {
                        let (name, mp) = parse_plugin_ref(ref_name);
                        PluginInspectInfo {
                            name,
                            enabled: !val
                                .get_path(disabled_key)
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                            detail: mp
                                .map(|m| format!("marketplace: {}", m))
                                .unwrap_or_else(|| "configured".into()),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::doc_engine::{DocFormat, DocTree, DocValue};
    use std::collections::HashMap;

    fn make_object_doc() -> DocTree {
        let root = DocValue::Object(HashMap::from([(
            "mcpServers".into(),
            DocValue::Object(HashMap::from([
                (
                    "fs".into(),
                    DocValue::Object(HashMap::from([(
                        "command".into(),
                        DocValue::String("npx".into()),
                    )])),
                ),
                (
                    "icm".into(),
                    DocValue::Object(HashMap::from([
                        ("command".into(), DocValue::String("icm".into())),
                        ("disabled".into(), DocValue::Bool(true)),
                    ])),
                ),
            ])),
        )]));
        DocTree::new_test(DocFormat::Toml, root)
    }

    fn make_plugin_array_doc() -> DocTree {
        let root = DocValue::Object(HashMap::from([(
            "plugin".into(),
            DocValue::Array(vec![
                DocValue::String("oh-my-openagent@latest".into()),
                DocValue::String("opencode-gemini-auth@latest".into()),
            ]),
        )]));
        DocTree::new_test(DocFormat::Json, root)
    }

    fn make_plugin_map_doc() -> DocTree {
        let root = DocValue::Object(HashMap::from([(
            "plugins".into(),
            DocValue::Object(HashMap::from([
                ("context7".into(), DocValue::Bool(true)),
                ("github".into(), DocValue::Bool(false)),
            ])),
        )]));
        DocTree::new_test(DocFormat::Json, root)
    }

    #[test]
    fn test_object_map_remove_existing() {
        let mut doc = make_object_doc();
        let count = object_map_remove(&mut doc, "mcpServers", &["fs".into()]);
        assert_eq!(count, 1);
        assert!(doc.get("mcpServers.fs").is_none());
        assert!(doc.get("mcpServers.icm").is_some());
    }

    #[test]
    fn test_object_map_remove_nonexistent() {
        let mut doc = make_object_doc();
        let count = object_map_remove(&mut doc, "mcpServers", &["nonexistent".into()]);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_object_map_remove_multiple() {
        let mut doc = make_object_doc();
        let count = object_map_remove(&mut doc, "mcpServers", &["fs".into(), "icm".into()]);
        assert_eq!(count, 2);
    }

    #[test]
    fn test_object_map_enable_disabled_entry() {
        let mut doc = make_object_doc();
        let count = object_map_enable(&mut doc, "mcpServers", "disabled", false, &["icm".into()]);
        assert_eq!(count, 1);
        let icm = doc.get("mcpServers.icm").unwrap();
        assert!(icm.get_path("disabled").is_none());
    }

    #[test]
    fn test_object_map_disable_enabled_entry() {
        let mut doc = make_object_doc();
        let count = object_map_disable(&mut doc, "mcpServers", "disabled", false, &["fs".into()]);
        assert_eq!(count, 1);
        let fs = doc.get("mcpServers.fs").unwrap();
        assert_eq!(
            fs.get_path("disabled").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_object_map_enable_with_enabled_semantic() {
        let root = DocValue::Object(HashMap::from([(
            "mcp".into(),
            DocValue::Object(HashMap::from([(
                "svc".into(),
                DocValue::Object(HashMap::from([
                    ("url".into(), DocValue::String("http://x".into())),
                    ("enabled".into(), DocValue::Bool(false)),
                ])),
            )])),
        )]));
        let mut doc = DocTree::new_test(DocFormat::Json, root);
        let count = object_map_enable(&mut doc, "mcp", "enabled", true, &["svc".into()]);
        assert_eq!(count, 1);
        let svc = doc.get("mcp.svc").unwrap();
        assert_eq!(
            svc.get_path("enabled").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_object_map_disable_with_enabled_semantic() {
        let root = DocValue::Object(HashMap::from([(
            "mcp".into(),
            DocValue::Object(HashMap::from([(
                "svc".into(),
                DocValue::Object(HashMap::from([
                    ("url".into(), DocValue::String("http://x".into())),
                    ("enabled".into(), DocValue::Bool(true)),
                ])),
            )])),
        )]));
        let mut doc = DocTree::new_test(DocFormat::Json, root);
        let count = object_map_disable(&mut doc, "mcp", "enabled", true, &["svc".into()]);
        assert_eq!(count, 1);
        let svc = doc.get("mcp.svc").unwrap();
        assert_eq!(
            svc.get_path("enabled").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn test_plugin_refs_from_doc_json_array() {
        let doc = make_plugin_array_doc();
        let refs = plugin_refs_from_doc(&doc, "json_array", "plugin");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].0, "oh-my-openagent");
        assert_eq!(refs[0].1, Some("latest".into()));
        assert_eq!(refs[1].0, "opencode-gemini-auth");
    }

    #[test]
    fn test_plugin_refs_from_doc_enabled_map() {
        let doc = make_plugin_map_doc();
        let refs = plugin_refs_from_doc(&doc, "enabled_map", "plugins");
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn test_plugin_refs_from_doc_unknown_format() {
        let doc = make_plugin_array_doc();
        let refs = plugin_refs_from_doc(&doc, "unknown_format", "plugin");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_plugin_inspect_from_doc_json_array() {
        let doc = make_plugin_array_doc();
        let items = plugin_inspect_from_doc(&doc, "json_array", "plugin", "disabled");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "oh-my-openagent");
        assert!(items[0].enabled);
    }

    #[test]
    fn test_plugin_inspect_from_doc_enabled_map() {
        let doc = make_plugin_map_doc();
        let items = plugin_inspect_from_doc(&doc, "enabled_map", "plugins", "disabled");
        assert_eq!(items.len(), 2);
        let ctx7 = items.iter().find(|i| i.name == "context7").unwrap();
        assert!(ctx7.enabled);
        let gh = items.iter().find(|i| i.name == "github").unwrap();
        assert!(!gh.enabled);
    }

    // ── object_map_enable / disable edge cases ──

    #[test]
    fn test_object_map_enable_already_enabled() {
        let mut doc = make_object_doc();
        // "fs" has no disabled key → already enabled
        let count = object_map_enable(&mut doc, "mcpServers", "disabled", false, &["fs".into()]);
        assert_eq!(count, 1); // still counts as "processed"
    }

    #[test]
    fn test_object_map_disable_already_disabled() {
        let mut doc = make_object_doc();
        // "icm" already has disabled=true
        let count = object_map_disable(&mut doc, "mcpServers", "disabled", false, &["icm".into()]);
        assert_eq!(count, 1);
        let icm = doc.get("mcpServers.icm").unwrap();
        assert_eq!(
            icm.get_path("disabled").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_object_map_enable_nonexistent() {
        let mut doc = make_object_doc();
        let count = object_map_enable(
            &mut doc,
            "mcpServers",
            "disabled",
            false,
            &["nonexistent".into()],
        );
        assert_eq!(count, 0);
    }

    #[test]
    fn test_object_map_disable_nonexistent() {
        let mut doc = make_object_doc();
        let count = object_map_disable(
            &mut doc,
            "mcpServers",
            "disabled",
            false,
            &["nonexistent".into()],
        );
        assert_eq!(count, 0);
    }

    #[test]
    fn test_object_map_remove_from_missing_section() {
        let doc = DocTree::new_test(DocFormat::Json, DocValue::Object(HashMap::new()));
        let mut doc = doc;
        let count = object_map_remove(&mut doc, "nonexistent", &["fs".into()]);
        assert_eq!(count, 0);
    }

    // ── env_config_path ──

    #[test]
    fn test_env_config_path_no_section_key() {
        let mapping = crate::adapter::mapping::ToolMapping {
            tool: crate::adapter::mapping::ToolInfo {
                name: "test".into(),
                config_dir: "~/.config/test".into(),
            },
            settings_path: None,
            mcp: Default::default(),
            provider: Default::default(),
            prompt: Default::default(),
            capabilities: Default::default(),
            hook: Default::default(),
            env: crate::adapter::mapping::EnvMappingConfig {
                exclude_keys: vec![],
                section_key: None,
                path: None,
                format: String::new(),
            },
            session: Default::default(),
            agent: Default::default(),
            skill: Default::default(),
            plugin: Default::default(),
        };
        assert!(env_config_path(&mapping).is_none());
    }

    #[test]
    fn test_env_config_path_empty_section_key() {
        let mapping = crate::adapter::mapping::ToolMapping {
            tool: crate::adapter::mapping::ToolInfo {
                name: "test".into(),
                config_dir: "~/.config/test".into(),
            },
            settings_path: None,
            mcp: Default::default(),
            provider: Default::default(),
            prompt: Default::default(),
            capabilities: Default::default(),
            hook: Default::default(),
            env: crate::adapter::mapping::EnvMappingConfig {
                exclude_keys: vec![],
                section_key: Some(String::new()),
                path: None,
                format: String::new(),
            },
            session: Default::default(),
            agent: Default::default(),
            skill: Default::default(),
            plugin: Default::default(),
        };
        assert!(env_config_path(&mapping).is_none());
    }

    #[test]
    fn test_env_config_path_with_section_key() {
        let mapping = crate::adapter::mapping::ToolMapping {
            tool: crate::adapter::mapping::ToolInfo {
                name: "test".into(),
                config_dir: "/tmp/test-config".into(),
            },
            settings_path: Some("custom.json".into()),
            mcp: Default::default(),
            provider: Default::default(),
            prompt: Default::default(),
            capabilities: Default::default(),
            hook: Default::default(),
            env: crate::adapter::mapping::EnvMappingConfig {
                exclude_keys: vec![],
                section_key: Some("env".into()),
                path: Some("settings.json".into()),
                format: String::new(),
            },
            session: Default::default(),
            agent: Default::default(),
            skill: Default::default(),
            plugin: Default::default(),
        };
        let result = env_config_path(&mapping);
        assert!(result.is_some());
        let (path, fmt) = result.unwrap();
        assert!(path.to_string_lossy().contains("settings.json"));
        assert_eq!(fmt, DocFormat::Json);
    }

    #[test]
    fn test_env_config_path_uses_settings_file_default() {
        let mapping = crate::adapter::mapping::ToolMapping {
            tool: crate::adapter::mapping::ToolInfo {
                name: "test".into(),
                config_dir: "/tmp/test-config".into(),
            },
            settings_path: Some("my-settings.json".into()),
            mcp: Default::default(),
            provider: Default::default(),
            prompt: Default::default(),
            capabilities: Default::default(),
            hook: Default::default(),
            env: crate::adapter::mapping::EnvMappingConfig {
                exclude_keys: vec![],
                section_key: Some("env".into()),
                path: None, // Falls back to settings_file()
                format: String::new(),
            },
            session: Default::default(),
            agent: Default::default(),
            skill: Default::default(),
            plugin: Default::default(),
        };
        let result = env_config_path(&mapping);
        assert!(result.is_some());
        let (path, _) = result.unwrap();
        assert!(path.to_string_lossy().contains("my-settings.json"));
    }

    // ── plugin_refs_from_doc: json_array with tuple entries ──

    #[test]
    fn test_plugin_refs_from_doc_json_array_with_tuple() {
        let root = DocValue::Object(HashMap::from([(
            "plugin".into(),
            DocValue::Array(vec![
                DocValue::Array(vec![
                    DocValue::String("myplugin@latest".into()),
                    DocValue::Object(HashMap::from([(
                        "autoApprove".into(),
                        DocValue::Array(vec![DocValue::String("all".into())]),
                    )])),
                ]),
                DocValue::String("simple-plugin@v1".into()),
            ]),
        )]));
        let doc = DocTree::new_test(DocFormat::Json, root);
        let refs = plugin_refs_from_doc(&doc, "json_array", "plugin");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].0, "myplugin");
        assert_eq!(refs[0].1, Some("latest".into()));
        assert!(refs[0].2.is_some()); // has options
        assert_eq!(refs[1].0, "simple-plugin");
        assert_eq!(refs[1].1, Some("v1".into()));
        assert!(refs[1].2.is_none()); // no options for plain string
    }

    // ── plugin_inspect_from_doc: enabled_list ──

    #[test]
    fn test_plugin_inspect_from_doc_enabled_list() {
        let root = DocValue::Object(HashMap::from([(
            "plugin".into(),
            DocValue::Array(vec![
                DocValue::String("myplugin@latest".into()),
                DocValue::String("other-plugin".into()),
            ]),
        )]));
        let doc = DocTree::new_test(DocFormat::Json, root);
        let items = plugin_inspect_from_doc(&doc, "enabled_list", "plugin", "disabled");
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.enabled));
        assert_eq!(items[0].name, "myplugin");
    }

    #[test]
    fn test_plugin_inspect_from_doc_unknown_format() {
        let doc = DocTree::new_test(DocFormat::Json, DocValue::Object(HashMap::new()));
        let items = plugin_inspect_from_doc(&doc, "unknown", "plugin", "disabled");
        assert!(items.is_empty());
    }
}
