pub mod ccswitch;
pub mod cherry_studio;
pub mod datasource_config;

/// 清理名称为合法的 toml 文件名
pub(crate) fn sanitize_name(name: &str) -> String {
    name.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "-")
        .trim_matches('-')
        .to_string()
}

// ── JSON 值提取 helpers（供 ccswitch 和 cherry_studio 共享）──

pub(crate) fn json_str(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

pub(crate) fn json_str_opt(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

pub(crate) fn json_str_array(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn json_str_map(
    v: &serde_json::Value,
    key: &str,
) -> std::collections::HashMap<String, String> {
    v.get(key)
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn json_str_or(v: &serde_json::Value, key: &str, default: &str) -> String {
    v.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_string()
}

/// 从 JSON value 中提取 McpConfig，供 ccswitch 和 cherry_studio 共享
pub(crate) fn mcp_config_from_json(
    config: &serde_json::Value,
    extra_fields: &[String],
    known_keys: &[String],
) -> crate::model::mcp::McpConfig {
    use crate::adapter::json_to_toml_value;
    use std::collections::HashMap;

    let command = json_str_opt(config, "command");
    let args = json_str_array(config, "args");
    let env = json_str_map(config, "env");
    let url = json_str_opt(config, "baseUrl").or_else(|| json_str_opt(config, "url"));
    let headers = json_str_map(config, "headers");
    let disabled_tools = json_str_array(config, "disabledTools");

    // 推断 server_type
    let server_type = if command.is_some() {
        "stdio".to_string()
    } else if url.is_some() {
        "sse".to_string()
    } else {
        "unknown".to_string()
    };

    // 收集 extra 字段（排除已知 key）
    let mut extra = HashMap::new();
    let known: std::collections::HashSet<&str> = known_keys.iter().map(|s| s.as_str()).collect();
    for field in extra_fields {
        if let Some(val) = config.get(field) {
            extra.insert(field.clone(), json_to_toml_value(val));
        }
    }
    // 自动收集非已知字段
    if let Some(obj) = config.as_object() {
        for (k, v) in obj {
            if !known.contains(k.as_str()) && !extra.contains_key(k) {
                extra.insert(k.clone(), json_to_toml_value(v));
            }
        }
    }

    crate::model::mcp::McpConfig {
        server_type,
        command,
        args,
        env,
        url,
        headers,
        disabled_tools,
        extra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sanitize_name ──

    #[test]
    fn test_sanitize_name_basic() {
        assert_eq!(sanitize_name("my server"), "my-server");
    }

    #[test]
    fn test_sanitize_name_special_chars() {
        // Each special char becomes a separate dash
        assert_eq!(sanitize_name("a!@#b$c%"), "a---b-c");
    }

    #[test]
    fn test_sanitize_name_keeps_alnum_dash_underscore() {
        assert_eq!(sanitize_name("my-server_v2"), "my-server_v2");
    }

    #[test]
    fn test_sanitize_name_trims_leading_trailing_dashes() {
        assert_eq!(sanitize_name("--hello--"), "hello");
    }

    #[test]
    fn test_sanitize_name_empty() {
        assert_eq!(sanitize_name(""), "");
    }

    #[test]
    fn test_sanitize_name_all_special() {
        assert_eq!(sanitize_name("!@#$%"), "");
    }

    // ── json_str ──

    #[test]
    fn test_json_str_found() {
        let v = serde_json::json!({"name": "test"});
        assert_eq!(json_str(&v, "name"), "test");
    }

    #[test]
    fn test_json_str_missing() {
        let v = serde_json::json!({"name": "test"});
        assert_eq!(json_str(&v, "other"), "");
    }

    #[test]
    fn test_json_str_not_string() {
        let v = serde_json::json!({"name": 42});
        assert_eq!(json_str(&v, "name"), "");
    }

    // ── json_str_opt ──

    #[test]
    fn test_json_str_opt_found() {
        let v = serde_json::json!({"name": "test"});
        assert_eq!(json_str_opt(&v, "name"), Some("test".to_string()));
    }

    #[test]
    fn test_json_str_opt_empty_string() {
        let v = serde_json::json!({"name": ""});
        assert_eq!(json_str_opt(&v, "name"), None);
    }

    #[test]
    fn test_json_str_opt_missing() {
        let v = serde_json::json!({"name": "test"});
        assert_eq!(json_str_opt(&v, "other"), None);
    }

    // ── json_str_array ──

    #[test]
    fn test_json_str_array_found() {
        let v = serde_json::json!({"args": ["npx", "-y", "server"]});
        assert_eq!(json_str_array(&v, "args"), vec!["npx", "-y", "server"]);
    }

    #[test]
    fn test_json_str_array_missing() {
        let v = serde_json::json!({"name": "test"});
        assert!(json_str_array(&v, "args").is_empty());
    }

    #[test]
    fn test_json_str_array_non_string_items() {
        let v = serde_json::json!({"args": ["npx", 42, true]});
        assert_eq!(json_str_array(&v, "args"), vec!["npx"]);
    }

    // ── json_str_map ──

    #[test]
    fn test_json_str_map_found() {
        let v = serde_json::json!({"env": {"KEY1": "val1", "KEY2": "val2"}});
        let map = json_str_map(&v, "env");
        assert_eq!(map.get("KEY1").unwrap(), "val1");
        assert_eq!(map.get("KEY2").unwrap(), "val2");
    }

    #[test]
    fn test_json_str_map_missing() {
        let v = serde_json::json!({});
        assert!(json_str_map(&v, "env").is_empty());
    }

    #[test]
    fn test_json_str_map_non_string_values() {
        let v = serde_json::json!({"env": {"KEY1": "val1", "KEY2": 42}});
        let map = json_str_map(&v, "env");
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("KEY1"));
    }

    // ── json_str_or ──

    #[test]
    fn test_json_str_or_found() {
        let v = serde_json::json!({"type": "openai"});
        assert_eq!(json_str_or(&v, "type", "default"), "openai");
    }

    #[test]
    fn test_json_str_or_missing() {
        let v = serde_json::json!({});
        assert_eq!(json_str_or(&v, "type", "default"), "default");
    }

    // ── mcp_config_from_json ──

    #[test]
    fn test_mcp_config_stdio() {
        let config = serde_json::json!({
            "command": "npx",
            "args": ["-y", "server"],
            "env": {"KEY": "val"},
        });
        let mcp = mcp_config_from_json(
            &config,
            &[],
            &["command".to_string(), "args".to_string(), "env".to_string()],
        );
        assert_eq!(mcp.server_type, "stdio");
        assert_eq!(mcp.command.as_deref(), Some("npx"));
        assert_eq!(mcp.args, vec!["-y", "server"]);
        assert_eq!(mcp.env.get("KEY").unwrap(), "val");
    }

    #[test]
    fn test_mcp_config_sse() {
        let config = serde_json::json!({
            "url": "https://example.com/mcp",
            "headers": {"Authorization": "Bearer tok"},
        });
        let mcp = mcp_config_from_json(&config, &[], &["url".to_string(), "headers".to_string()]);
        assert_eq!(mcp.server_type, "sse");
        assert_eq!(mcp.url.as_deref(), Some("https://example.com/mcp"));
    }

    #[test]
    fn test_mcp_config_unknown() {
        let config = serde_json::json!({});
        let mcp = mcp_config_from_json(&config, &[], &[]);
        assert_eq!(mcp.server_type, "unknown");
    }

    #[test]
    fn test_mcp_config_extra_fields() {
        let config = serde_json::json!({
            "command": "npx",
            "custom_field": "hello",
        });
        let mcp = mcp_config_from_json(
            &config,
            &["custom_field".to_string()],
            &["command".to_string()],
        );
        assert!(mcp.extra.contains_key("custom_field"));
    }

    #[test]
    fn test_mcp_config_auto_collect_unknown() {
        let config = serde_json::json!({
            "command": "npx",
            "unknown_key": "val",
        });
        let mcp = mcp_config_from_json(&config, &[], &["command".to_string()]);
        assert!(mcp.extra.contains_key("unknown_key"));
    }

    #[test]
    fn test_mcp_config_disabled_tools() {
        let config = serde_json::json!({
            "command": "npx",
            "disabledTools": ["tool1", "tool2"],
        });
        let mcp = mcp_config_from_json(
            &config,
            &[],
            &["command".to_string(), "disabledTools".to_string()],
        );
        assert_eq!(mcp.disabled_tools, vec!["tool1", "tool2"]);
    }

    // ── Platform-aware sanitize_name tests ──

    #[test]
    fn test_sanitize_name_windows_unsafe_chars() {
        // Windows forbids: < > : " / \ | ? *
        // Our sanitize only keeps alnum, dash, underscore
        assert_eq!(sanitize_name("file:name"), "file-name");
        assert_eq!(sanitize_name("path\\dir"), "path-dir");
        assert_eq!(sanitize_name("a<b>c"), "a-b-c");
        assert_eq!(sanitize_name("quote\"here"), "quote-here");
        assert_eq!(sanitize_name("pipe|amp"), "pipe-amp");
        assert_eq!(sanitize_name("ask?what"), "ask-what");
        assert_eq!(sanitize_name("star*pattern"), "star-pattern");
    }

    #[test]
    fn test_sanitize_name_linux_reserved_names() {
        // Linux forbids / and null in filenames
        assert_eq!(sanitize_name("path/to/file"), "path-to-file");
        // Names starting with dot: dot is replaced with dash, then leading dash trimmed
        assert_eq!(sanitize_name(".hidden"), "hidden");
    }

    #[test]
    fn test_sanitize_name_cross_platform_safe() {
        // These names should be safe on all platforms
        assert_eq!(sanitize_name("my-mcp-server"), "my-mcp-server");
        assert_eq!(sanitize_name("fs_v2"), "fs_v2");
        assert_eq!(sanitize_name("openai-gpt4"), "openai-gpt4");
    }

    #[test]
    fn test_sanitize_name_consecutive_dashes() {
        // Multiple special chars produce consecutive dashes
        assert_eq!(sanitize_name("a!!!b"), "a---b");
    }

    #[test]
    fn test_sanitize_name_windows_drive_letter() {
        // "C:" → "C-" but trailing dash is trimmed → "C"
        assert_eq!(sanitize_name("C:"), "C");
    }

    #[test]
    fn test_sanitize_name_long_name() {
        // Very long name should still work (no truncation in sanitize)
        let long = "a".repeat(255);
        assert_eq!(sanitize_name(&long), long);
    }
}
