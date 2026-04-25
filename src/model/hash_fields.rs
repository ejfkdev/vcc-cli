/// 每种资源类型参与哈希计算的字段定义
/// 排除 id、name、metadata（外观数据）、tool（来源特定覆盖）
/// 字段定义在 config/hash_fields.toml 配置文件中，编译时通过 crate::config 集中加载
use std::collections::HashMap;
use std::sync::OnceLock;

/// 哈希字段配置：kind → 参与哈希的字段列表
static HASH_FIELDS: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();

fn get_hash_fields() -> &'static HashMap<String, Vec<String>> {
    HASH_FIELDS.get_or_init(|| {
        let content = crate::config::hash_fields_content();
        let value: toml::Value = content
            .parse()
            .expect("hash_fields.toml should be valid TOML");
        let mut map = HashMap::new();
        if let Some(table) = value.as_table() {
            for (kind, v) in table {
                if let Some(fields) = v.get("fields").and_then(|f| f.as_array()) {
                    map.insert(
                        kind.clone(),
                        fields
                            .iter()
                            .filter_map(|f| f.as_str().map(String::from))
                            .collect(),
                    );
                }
            }
        }
        map
    })
}

/// 从 serde_json::Value 中提取参与哈希的字段，返回确定性 JSON 字符串
pub(crate) fn resource_hash_content(kind: &str, resource_json: &serde_json::Value) -> String {
    let fields: Vec<&str> = get_hash_fields()
        .get(kind)
        .map(|v| v.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let filtered = filter_fields(resource_json, &fields);
    let mut obj = match filtered {
        serde_json::Value::Object(m) => m,
        other => return serde_json::to_string(&other).unwrap_or_default(),
    };
    sort_json_object(&mut obj);
    serde_json::to_string(&serde_json::Value::Object(obj)).unwrap_or_default()
}

fn filter_fields(value: &serde_json::Value, fields: &[&str]) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut filtered = serde_json::Map::new();
            for &field in fields {
                if let Some(v) = map.get(field) {
                    filtered.insert(field.to_string(), v.clone());
                }
            }
            if filtered.is_empty() && !fields.is_empty() {
                return serde_json::Value::Object(serde_json::Map::new());
            }
            serde_json::Value::Object(filtered)
        }
        other => other.clone(),
    }
}

fn sort_json_object(obj: &mut serde_json::Map<String, serde_json::Value>) {
    for value in obj.values_mut() {
        match value {
            serde_json::Value::Object(ref mut inner) => sort_json_object(inner),
            serde_json::Value::Array(ref mut arr) => {
                for item in arr.iter_mut() {
                    if let serde_json::Value::Object(ref mut inner) = item {
                        sort_json_object(inner);
                    }
                }
            }
            _ => {}
        }
    }
    let mut entries: Vec<(String, serde_json::Value)> =
        obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    obj.clear();
    obj.extend(entries);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_hash_content() {
        let json = serde_json::json!({
            "name": "test", "type": "provider",
            "config": { "provider_type": "openai", "api_key": "sk-123", "base_url": "https://api.openai.com" },
            "id": "should-be-excluded", "metadata": { "description": "should be excluded" },
            "tool": { "claude": { "api_key": "should be excluded" } }
        });
        let content = resource_hash_content("provider", &json);
        assert!(!content.contains("\"name\""));
        assert!(content.contains("\"config\""));
        assert!(content.contains("provider_type"));
        assert!(!content.contains("should-be-excluded"));
        assert!(!content.contains("metadata"));
        assert!(!content.contains("\"tool\""));
    }

    #[test]
    fn test_deterministic_output() {
        let json1 = serde_json::json!({ "name": "test", "type": "provider", "config": { "provider_type": "openai", "api_key": "sk-123", "base_url": "https://api.openai.com" } });
        let json2 = serde_json::json!({ "type": "provider", "name": "different-name", "config": { "base_url": "https://api.openai.com", "provider_type": "openai", "api_key": "sk-123" } });
        assert_eq!(
            resource_hash_content("provider", &json1),
            resource_hash_content("provider", &json2)
        );
    }

    // ── filter_fields ──

    #[test]
    fn test_filter_fields_selects_specified() {
        let json = serde_json::json!({"a": 1, "b": 2, "c": 3});
        let filtered = filter_fields(&json, &["a", "c"]);
        let obj = filtered.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert_eq!(obj.get("a").unwrap(), 1);
        assert_eq!(obj.get("c").unwrap(), 3);
        assert!(obj.get("b").is_none());
    }

    #[test]
    fn test_filter_fields_no_match() {
        let json = serde_json::json!({"a": 1, "b": 2});
        let filtered = filter_fields(&json, &["x", "y"]);
        assert!(filtered.as_object().unwrap().is_empty());
    }

    #[test]
    fn test_filter_fields_non_object() {
        let json = serde_json::json!(42);
        let filtered = filter_fields(&json, &["a"]);
        assert_eq!(filtered, serde_json::json!(42));
    }

    #[test]
    fn test_filter_fields_empty_fields_returns_empty() {
        let json = serde_json::json!({"a": 1, "b": 2});
        let filtered = filter_fields(&json, &[]);
        // Empty fields → returns empty object (no fields to select)
        let obj = filtered.as_object().unwrap();
        assert!(obj.is_empty());
    }

    // ── sort_json_object ──

    #[test]
    fn test_sort_json_object_sorted() {
        let mut obj = serde_json::Map::new();
        obj.insert("z".into(), serde_json::json!(1));
        obj.insert("a".into(), serde_json::json!(2));
        obj.insert("m".into(), serde_json::json!(3));
        sort_json_object(&mut obj);
        let keys: Vec<&String> = obj.keys().collect();
        assert_eq!(keys, vec!["a", "m", "z"]);
    }

    #[test]
    fn test_sort_json_object_nested() {
        let mut inner = serde_json::Map::new();
        inner.insert("b".into(), serde_json::json!(1));
        inner.insert("a".into(), serde_json::json!(2));
        let mut outer = serde_json::Map::new();
        outer.insert("inner".into(), serde_json::Value::Object(inner));
        sort_json_object(&mut outer);
        let inner_obj = outer.get("inner").unwrap().as_object().unwrap();
        let keys: Vec<&String> = inner_obj.keys().collect();
        assert_eq!(keys, vec!["a", "b"]);
    }

    // ── resource_hash_content for other kinds ──

    #[test]
    fn test_mcp_hash_content() {
        let json = serde_json::json!({
            "name": "fs",
            "type": "mcp",
            "config": { "command": "npx", "server_type": "stdio" },
            "id": "excluded", "metadata": { "description": "excluded" }
        });
        let content = resource_hash_content("mcp", &json);
        assert!(!content.contains("\"name\""));
        assert!(!content.contains("excluded"));
        assert!(content.contains("\"config\""));
        assert!(content.contains("server_type"));
    }

    #[test]
    fn test_hook_hash_content() {
        let json = serde_json::json!({
            "name": "test-hook",
            "type": "hook",
            "config": { "event": "PreToolUse", "matcher": "", "command": "echo", "timeout": 30 },
            "id": "irrelevant"
        });
        let content = resource_hash_content("hook", &json);
        assert!(content.contains("\"command\":\"echo\""));
        assert!(content.contains("\"event\":\"PreToolUse\""));
        assert!(!content.contains("\"name\""));
        assert!(!content.contains("irrelevant"));
    }

    // ── additional hash_fields tests ──

    #[test]
    fn test_unknown_kind_empty_fields() {
        let json = serde_json::json!({"anything": "value"});
        let content = resource_hash_content("nonexistent_kind", &json);
        // Unknown kind → empty fields → empty object
        assert_eq!(content, "{}");
    }

    #[test]
    fn test_filter_fields_preserves_nested() {
        let json = serde_json::json!({
            "config": { "nested": { "deep": true } },
            "other": "ignored"
        });
        let filtered = filter_fields(&json, &["config"]);
        let obj = filtered.as_object().unwrap();
        assert_eq!(obj.len(), 1);
        assert!(obj.get("config").unwrap().get("nested").is_some());
        assert!(obj.get("other").is_none());
    }

    #[test]
    fn test_sort_json_object_with_array() {
        let mut inner = serde_json::Map::new();
        inner.insert("b".into(), serde_json::json!(2));
        inner.insert("a".into(), serde_json::json!(1));
        let mut outer = serde_json::Map::new();
        outer.insert(
            "items".into(),
            serde_json::Value::Array(vec![serde_json::Value::Object(inner)]),
        );
        sort_json_object(&mut outer);
        let arr = outer.get("items").unwrap().as_array().unwrap();
        let sorted_inner = arr[0].as_object().unwrap();
        let keys: Vec<&String> = sorted_inner.keys().collect();
        assert_eq!(keys, vec!["a", "b"]);
    }

    #[test]
    fn test_sort_json_object_empty() {
        let mut obj = serde_json::Map::new();
        sort_json_object(&mut obj);
        assert!(obj.is_empty());
    }

    #[test]
    fn test_resource_hash_content_env() {
        let json = serde_json::json!({
            "name": "test-env",
            "type": "env",
            "config": { "vars": { "API_KEY": "sk-123" } },
            "id": "excluded"
        });
        let content = resource_hash_content("env", &json);
        assert!(content.contains("\"vars\""));
        assert!(!content.contains("\"name\""));
    }
}
