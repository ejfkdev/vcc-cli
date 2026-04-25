use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;

use super::{json_str, json_str_array, json_str_map, json_str_opt, json_str_or, sanitize_name};
use crate::datasource::datasource_config::DatasourceConfig;
use crate::model::{
    mcp::{McpConfig, McpServer, McpToolOverride},
    provider::{Provider, ProviderConfig},
    Metadata, Resource,
};

pub(crate) struct CherryStudioSource {
    data_dir: PathBuf,
    config: DatasourceConfig,
}

impl CherryStudioSource {
    pub fn new() -> Option<Self> {
        let config = DatasourceConfig::load("cherry-studio")?;
        let data_dir = config.resolve_base_dir()?;
        data_dir.exists().then_some(Self { data_dir, config })
    }
    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    fn read_redux_state(&self) -> Result<serde_json::Value> {
        let leveldb_path = self
            .config
            .resolve_leveldb_path()
            .unwrap_or_else(|| self.data_dir.join("Local Storage").join("leveldb"));
        if !leveldb_path.exists() {
            anyhow::bail!("Cherry Studio Local Storage/leveldb not found");
        }
        let opts = rusty_leveldb::Options {
            create_if_missing: false,
            ..Default::default()
        };
        let mut db = rusty_leveldb::DB::open(&leveldb_path, opts)
            .map_err(|e| anyhow::anyhow!("failed to open Cherry Studio LevelDB: {:?}", e))?;
        let key_bytes = if self.config.leveldb.persist_key.is_empty() {
            b"_file://\x00\x01persist:cherry-studio".to_vec()
        } else {
            parse_escaped_bytes(&self.config.leveldb.persist_key)
        };
        let value = db
            .get(&key_bytes)
            .ok_or_else(|| anyhow::anyhow!("persist key not found in Cherry Studio LevelDB"))?;
        if value.is_empty() {
            anyhow::bail!("empty value in LevelDB");
        }
        let u16_vec: Vec<u16> = value[1..]
            .chunks(2)
            .map(|c| {
                if c.len() == 2 {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    c[0] as u16
                }
            })
            .collect();
        let decoded = String::from_utf16_lossy(&u16_vec);
        let json_start = decoded
            .find('{')
            .ok_or_else(|| anyhow::anyhow!("no JSON found in LevelDB value"))?;
        serde_json::from_str(&decoded[json_start..])
            .context("failed to parse Cherry Studio redux state JSON")
    }

    /// 解析二次序列化的 JSON slice（Cherry Studio 的 llm/mcp 字段）
    fn parse_json_slice(root: &serde_json::Value, key: &str) -> Option<Vec<serde_json::Value>> {
        let slice_str = root.get(key).and_then(|v| v.as_str())?;
        let parsed: serde_json::Value = serde_json::from_str(slice_str).ok()?;
        let array_key = if key == "llm" { "providers" } else { "servers" };
        parsed.get(array_key).and_then(|v| v.as_array()).cloned()
    }

    fn collect_extra(src: &serde_json::Value, fields: &[String]) -> HashMap<String, toml::Value> {
        fields
            .iter()
            .filter_map(|f| {
                src.get(f)
                    .map(|v| (f.clone(), crate::adapter::json_to_toml_value(v)))
            })
            .collect()
    }

    // Deserialize: fields parsed from external data, not all used
    #[allow(dead_code)]
    pub fn import_providers(&self) -> Result<Vec<Provider>> {
        self.import_providers_from(&self.read_redux_state()?)
    }

    fn import_providers_from(&self, root: &serde_json::Value) -> Result<Vec<Provider>> {
        let cfg = &self.config;
        let providers_arr = match Self::parse_json_slice(root, "llm") {
            Some(a) => a,
            None => return Ok(Vec::new()),
        };
        let mut providers: Vec<Provider> = Vec::new();
        let mut seen_names: HashMap<String, usize> = HashMap::new();
        for p in &providers_arr {
            let name = json_str(p, "name");
            if name.is_empty() {
                continue;
            }
            let api_key = json_str(p, "apiKey");
            let p_type = json_str_or(p, "type", "openai");
            if !p.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true) {
                continue;
            }
            if api_key.is_empty() && !cfg.allow_no_key(&p_type) {
                continue;
            }
            let effective_type = cfg.apply_vertex_override(
                &p_type,
                p.get("isVertex").and_then(|v| v.as_bool()).unwrap_or(false),
            );
            let vcc_type = cfg.map_provider_type(&effective_type);
            let vcc_name = sanitize_name(&name);
            let models: Vec<String> = p
                .get("models")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let config = ProviderConfig {
                provider_type: vcc_type.clone(),
                api_key,
                base_url: json_str_opt(p, "apiHost"),
                default_model: models.first().cloned(),
                models,
                npm: None,
                headers: json_str_map(p, "extra_headers"),
                env: HashMap::new(),
                model_meta: HashMap::new(),
                extra: Self::collect_extra(p, &cfg.provider_extra_fields),
            };
            let homepage = cfg
                .provider_metadata_map
                .get("homepage")
                .and_then(|keys| cfg.resolve_metadata_field(p, keys));
            let metadata = Metadata {
                description: json_str_opt(p, "notes")
                    .or_else(|| Some(format!("Cherry Studio provider ({})", effective_type))),
                homepage,
                tags: vec![
                    crate::config::adapter_defaults().defaults.sync_tag.clone(),
                    effective_type.to_string(),
                ],
            };
            if let Some(&idx) = seen_names.get(&vcc_name) {
                if providers[idx].config.api_key.is_empty() && !config.api_key.is_empty() {
                    let mut p = Provider::new_with_name(&vcc_name);
                    p.config = config;
                    p.metadata = metadata;
                    providers[idx] = p;
                }
            } else {
                seen_names.insert(vcc_name.clone(), providers.len());
                let mut p = Provider::new_with_name(&vcc_name);
                p.config = config;
                p.metadata = metadata;
                providers.push(p);
            }
        }
        Ok(providers)
    }

    // Deserialize: fields parsed from external data, not all used
    #[allow(dead_code)]
    pub fn import_mcp_servers(&self) -> Result<Vec<McpServer>> {
        self.import_mcp_servers_from(&self.read_redux_state()?)
    }

    fn import_mcp_servers_from(&self, root: &serde_json::Value) -> Result<Vec<McpServer>> {
        let cfg = &self.config;
        let servers_arr = match Self::parse_json_slice(root, "mcp") {
            Some(a) => a,
            None => return Ok(Vec::new()),
        };
        let mut servers = Vec::new();
        for s in &servers_arr {
            let name = json_str(s, "name");
            if name.is_empty() || cfg.should_skip_mcp(&name) {
                continue;
            }
            let is_active = s.get("isActive").and_then(|v| v.as_bool()).unwrap_or(false);
            let cs_type = json_str_or(s, "type", "");
            let command = json_str_opt(s, "command");
            let url = json_str_opt(s, "baseUrl").or_else(|| json_str_opt(s, "url"));
            let server_type = match cfg.map_mcp_type(&cs_type) {
                Some(t) => t,
                None => {
                    if command.is_some() {
                        "stdio".to_string()
                    } else if url.is_some() {
                        "sse".to_string()
                    } else {
                        continue;
                    }
                }
            };
            let mcp_config = McpConfig {
                server_type: server_type.clone(),
                command,
                args: json_str_array(s, "args"),
                env: json_str_map(s, "env"),
                url,
                headers: json_str_map(s, "headers"),
                disabled_tools: json_str_array(s, "disabledTools"),
                extra: Self::collect_extra(s, &cfg.mcp_extra_fields),
            };
            let mut tool: HashMap<String, McpToolOverride> = HashMap::new();
            if !is_active {
                tool.insert(
                    "cherry-studio".into(),
                    McpToolOverride {
                        disabled_tools: vec!["*".into()],
                        ..Default::default()
                    },
                );
            }
            let homepage = cfg
                .mcp_metadata_map
                .get("homepage")
                .and_then(|keys| cfg.resolve_metadata_field(s, keys));
            let mut mcp = McpServer::new_with_name(&sanitize_name(&name));
            mcp.config = mcp_config;
            mcp.metadata = Metadata {
                description: json_str_opt(s, "description"),
                homepage,
                tags: vec![crate::config::adapter_defaults().defaults.sync_tag.clone()],
            };
            mcp.tool = tool;
            servers.push(mcp);
        }
        Ok(servers)
    }

    pub fn import_all(&self) -> Result<(Vec<Provider>, Vec<McpServer>)> {
        let root = self.read_redux_state()?;
        Ok((
            self.import_providers_from(&root)?,
            self.import_mcp_servers_from(&root)?,
        ))
    }
}

fn parse_escaped_bytes(s: &str) -> Vec<u8> {
    let mut r = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('x') => {
                    let hex: String = chars.by_ref().take(2).collect();
                    if let Ok(b) = u8::from_str_radix(&hex, 16) {
                        r.push(b);
                    } else {
                        r.extend_from_slice(b"\\x");
                        r.extend(hex.bytes());
                    }
                }
                Some('n') => r.push(b'\n'),
                Some('r') => r.push(b'\r'),
                Some('t') => r.push(b'\t'),
                Some('\\') => r.push(b'\\'),
                Some(o) => {
                    r.push(b'\\');
                    r.push(o as u8);
                }
                None => r.push(b'\\'),
            }
        } else {
            r.push(c as u8);
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_escaped_bytes ──

    #[test]
    fn test_hex_escape() {
        assert_eq!(parse_escaped_bytes("\\x00\\x01"), vec![0x00, 0x01]);
    }

    #[test]
    fn test_hex_escape_ff() {
        assert_eq!(parse_escaped_bytes("\\xff"), vec![0xff]);
    }

    #[test]
    fn test_named_escapes() {
        assert_eq!(parse_escaped_bytes("\\n"), vec![b'\n']);
        assert_eq!(parse_escaped_bytes("\\r"), vec![b'\r']);
        assert_eq!(parse_escaped_bytes("\\t"), vec![b'\t']);
        assert_eq!(parse_escaped_bytes("\\\\"), vec![b'\\']);
    }

    #[test]
    fn test_plain_text() {
        assert_eq!(parse_escaped_bytes("hello"), b"hello".to_vec());
    }

    #[test]
    fn test_mixed() {
        let result = parse_escaped_bytes("a\\x00b\\nc");
        assert_eq!(result, vec![b'a', 0x00, b'b', b'\n', b'c']);
    }

    #[test]
    fn test_invalid_hex() {
        // "zz" is not valid hex → keep literal "\\xzz"
        let result = parse_escaped_bytes("\\xzz");
        assert_eq!(result, b"\\xzz".to_vec());
    }

    #[test]
    fn test_trailing_backslash() {
        let result = parse_escaped_bytes("a\\");
        assert_eq!(result, vec![b'a', b'\\']);
    }

    #[test]
    fn test_unknown_escape() {
        let result = parse_escaped_bytes("\\q");
        assert_eq!(result, vec![b'\\', b'q']);
    }

    #[test]
    fn test_empty() {
        assert!(parse_escaped_bytes("").is_empty());
    }

    #[test]
    fn test_short_hex_escape() {
        // "a" is valid hex → parses as 0x0a = newline
        let result = parse_escaped_bytes("\\xa");
        assert_eq!(result, vec![0x0a]);
    }

    // ── sanitize_name (from super::super) ──

    #[test]
    fn test_sanitize_name_in_cherry_context() {
        assert_eq!(super::super::sanitize_name("My Server!"), "My-Server");
    }
}
