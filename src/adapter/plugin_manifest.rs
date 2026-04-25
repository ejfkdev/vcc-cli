use anyhow::Result;

/// 跨工具适配 manifest
pub(crate) fn adapt_plugin_manifest(
    _source_format: &str,
    target_format: &str,
    source_manifest: &serde_json::Value,
) -> Result<serde_json::Value> {
    let name = source_manifest
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let version = source_manifest
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("1.0.0")
        .to_string();
    let description = source_manifest
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    match target_format {
        "claude" | "codex" | "droid" => {
            let mut target = serde_json::json!({
                "name": name,
                "version": version,
            });
            if !description.is_empty() {
                target["description"] = serde_json::json!(description);
            }
            for field in &["skills", "hooks", "agents", "commands"] {
                if let Some(val) = source_manifest.get(*field) {
                    target[*field] = val.clone();
                }
            }
            if let Some(mcp_servers) = source_manifest.get("mcpServers") {
                target["mcpServers"] = mcp_servers.clone();
            }
            if target_format == "codex" {
                if let Some(obj) = target.as_object_mut() {
                    obj.remove("lspServers");
                    obj.remove("outputStyles");
                }
            }
            Ok(target)
        }
        "gemini" => {
            let mut target = serde_json::json!({
                "name": name,
                "version": version,
                "description": description,
                "contextFileName": "GEMINI.md",
            });
            if let Some(mcp_servers) = source_manifest.get("mcpServers") {
                target["mcpServers"] = mcp_servers.clone();
            }
            replace_env_vars_in_json(&mut target, "${CLAUDE_PLUGIN_ROOT}", "${extensionPath}");
            replace_env_vars_in_json(&mut target, "${CODEX_PLUGIN_ROOT}", "${extensionPath}");
            Ok(target)
        }
        "kimi" | "opencode" => Ok(serde_json::json!({
            "name": name,
            "version": version,
            "description": description,
        })),
        _ => Ok(source_manifest.clone()),
    }
}

/// 递归替换 JSON 中的字符串值
pub(crate) fn replace_env_vars_in_json(value: &mut serde_json::Value, from: &str, to: &str) {
    match value {
        serde_json::Value::String(s) => {
            *s = s.replace(from, to);
        }
        serde_json::Value::Object(map) => {
            for v in map.values_mut() {
                replace_env_vars_in_json(v, from, to);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                replace_env_vars_in_json(v, from, to);
            }
        }
        _ => {}
    }
}

/// 获取源格式的 manifest 目录名
pub(crate) fn get_manifest_dir(format: &str) -> String {
    crate::config::adapter_defaults()
        .manifest_dir(format)
        .unwrap_or("")
        .to_string()
}

/// 获取源格式的 manifest 文件名
pub(crate) fn get_manifest_file(format: &str) -> String {
    crate::config::adapter_defaults()
        .manifest_file(format)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── adapt_plugin_manifest ──

    #[test]
    fn test_adapt_claude() {
        let src = serde_json::json!({
            "name": "my-plugin",
            "version": "1.0.0",
            "description": "A test plugin",
            "mcpServers": {"fs": {"command": "npx"}},
            "skills": ["skill1"],
        });
        let result = adapt_plugin_manifest("generic", "claude", &src).unwrap();
        assert_eq!(result["name"], "my-plugin");
        assert_eq!(result["version"], "1.0.0");
        assert_eq!(result["description"], "A test plugin");
        assert!(result.get("mcpServers").is_some());
        assert!(result.get("skills").is_some());
    }

    #[test]
    fn test_adapt_codex_removes_lsp() {
        let src = serde_json::json!({
            "name": "p",
            "version": "2.0",
            "lspServers": {"ls": {}},
            "outputStyles": {"s": {}},
        });
        let result = adapt_plugin_manifest("generic", "codex", &src).unwrap();
        assert!(result.get("lspServers").is_none());
        assert!(result.get("outputStyles").is_none());
    }

    #[test]
    fn test_adapt_gemini_replaces_env_vars() {
        let src = serde_json::json!({
            "name": "p",
            "version": "1.0",
            "description": "desc",
            "mcpServers": {"fs": {"command": "${CLAUDE_PLUGIN_ROOT}/bin/run"}},
        });
        let result = adapt_plugin_manifest("generic", "gemini", &src).unwrap();
        assert_eq!(result["contextFileName"], "GEMINI.md");
        let mcp = result["mcpServers"]["fs"]["command"].as_str().unwrap();
        assert!(mcp.contains("${extensionPath}"));
        assert!(!mcp.contains("${CLAUDE_PLUGIN_ROOT}"));
    }

    #[test]
    fn test_adapt_kimi_minimal() {
        let src = serde_json::json!({
            "name": "p",
            "version": "1.0",
            "description": "desc",
            "mcpServers": {"fs": {}},
        });
        let result = adapt_plugin_manifest("generic", "kimi", &src).unwrap();
        assert_eq!(result["name"], "p");
        assert_eq!(result["version"], "1.0");
        assert!(result.get("mcpServers").is_none());
    }

    #[test]
    fn test_adapt_unknown_returns_clone() {
        let src = serde_json::json!({"name": "p", "custom": 42});
        let result = adapt_plugin_manifest("generic", "unknown_tool", &src).unwrap();
        assert_eq!(result, src);
    }

    #[test]
    fn test_adapt_no_name_defaults_empty() {
        let src = serde_json::json!({"version": "1.0"});
        let result = adapt_plugin_manifest("generic", "claude", &src).unwrap();
        assert_eq!(result["name"], "");
    }

    #[test]
    fn test_adapt_no_description_omitted_for_claude() {
        let src = serde_json::json!({"name": "p", "version": "1.0"});
        let result = adapt_plugin_manifest("generic", "claude", &src).unwrap();
        assert!(result.get("description").is_none());
    }

    // ── replace_env_vars_in_json ──

    #[test]
    fn test_replace_in_string() {
        let mut v = serde_json::json!("${ROOT}/bin/run");
        replace_env_vars_in_json(&mut v, "${ROOT}", "/opt/plugin");
        assert_eq!(v, "/opt/plugin/bin/run");
    }

    #[test]
    fn test_replace_in_object() {
        let mut v = serde_json::json!({"cmd": "${ROOT}/run", "name": "keep"});
        replace_env_vars_in_json(&mut v, "${ROOT}", "/app");
        assert_eq!(v["cmd"], "/app/run");
        assert_eq!(v["name"], "keep");
    }

    #[test]
    fn test_replace_in_array() {
        let mut v = serde_json::json!(["${A}", "${B}"]);
        replace_env_vars_in_json(&mut v, "${A}", "x");
        assert_eq!(v[0], "x");
        assert_eq!(v[1], "${B}");
    }

    #[test]
    fn test_replace_skips_non_string() {
        let mut v = serde_json::json!({"n": 42, "b": true});
        replace_env_vars_in_json(&mut v, "42", "replaced");
        assert_eq!(v["n"], 42);
        assert_eq!(v["b"], true);
    }

    #[test]
    fn test_replace_nested() {
        let mut v = serde_json::json!({
            "outer": {"inner": "${PFX}/path"}
        });
        replace_env_vars_in_json(&mut v, "${PFX}", "/usr");
        assert_eq!(v["outer"]["inner"], "/usr/path");
    }

    #[test]
    fn test_replace_no_match() {
        let mut v = serde_json::json!("hello world");
        replace_env_vars_in_json(&mut v, "${MISSING}", "val");
        assert_eq!(v, "hello world");
    }
}
