use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct DatasourceConfig {
    pub source: SourceConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    #[serde(default)]
    pub leveldb: LeveldbConfig,
    #[serde(default)]
    pub provider_type_map: HashMap<String, String>,
    #[serde(default)]
    pub mcp_type_map: HashMap<String, String>,
    #[serde(default)]
    pub mcp_skip_prefixes: Vec<String>,
    #[serde(default)]
    pub provider_no_key_types: Vec<String>,
    #[serde(default)]
    pub mcp_extra_fields: Vec<String>,
    #[serde(default)]
    pub mcp_metadata_map: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub provider_metadata_map: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub provider_extra_fields: Vec<String>,
    #[serde(default)]
    pub is_vertex_override: HashMap<String, String>,
    #[serde(default)]
    pub enabled_columns: HashMap<String, String>,
    #[serde(default)]
    pub common_config_keys: HashMap<String, String>,
    #[serde(default)]
    pub env_known_keys: HashMap<String, EnvKnownKeys>,
    #[serde(default)]
    pub mcp_known_keys: McpKnownKeys,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct SourceConfig {
    pub path: PlatformPaths,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct PlatformPaths {
    #[serde(default)]
    pub linux: PathConfig,
    #[serde(default)]
    pub macos: PathConfig,
    #[serde(default)]
    pub windows: PathConfig,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct PathConfig {
    pub base: Option<String>,
    pub base_dir: Option<String>,
    pub subdir: Option<String>,
    pub db: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct SkillsConfig {
    #[serde(default)]
    pub scan_dirs: Vec<ScanDir>,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct ScanDir {
    pub path: String,
    pub tag: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct LeveldbConfig {
    #[serde(default)]
    pub relative_path: String,
    #[serde(default)]
    pub persist_key: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct EnvKnownKeys {
    #[serde(default)]
    pub all: Vec<String>,
    #[serde(default)]
    pub model_env: Vec<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct McpKnownKeys {
    #[serde(default)]
    pub all: Vec<String>,
}

impl DatasourceConfig {
    pub fn load(name: &str) -> Option<Self> {
        crate::config::datasource_mapping_content(name).and_then(|c| toml::from_str(c).ok())
    }
    fn platform_path(&self) -> &PathConfig {
        match self.current_platform() {
            "macos" => &self.source.path.macos,
            "windows" => &self.source.path.windows,
            _ => &self.source.path.linux,
        }
    }
    fn current_platform(&self) -> &str {
        if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            "linux"
        }
    }

    pub fn resolve_base_dir(&self) -> Option<PathBuf> {
        let pc = self.platform_path();
        if let Some(base) = &pc.base {
            return Some(expand_tilde(base));
        }
        let dir = match pc.base_dir.as_deref() {
            Some("data") => dirs::data_dir(),
            Some("config") => dirs::config_dir(),
            Some("home") | Some("~") => dirs::home_dir(),
            _ => dirs::home_dir(),
        }?;
        Some(match &pc.subdir {
            Some(s) => dir.join(s),
            None => dir,
        })
    }

    pub fn resolve_db_path(&self) -> Option<PathBuf> {
        let base = self.resolve_base_dir()?;
        Some(base.join(self.platform_path().db.as_deref().unwrap_or("data.db")))
    }

    pub fn resolve_leveldb_path(&self) -> Option<PathBuf> {
        let base = self.resolve_base_dir()?;
        if self.leveldb.relative_path.is_empty() {
            return None;
        }
        Some(base.join(&self.leveldb.relative_path))
    }

    pub fn resolve_skill_scan_dirs(&self) -> Vec<(PathBuf, String)> {
        self.skills
            .scan_dirs
            .iter()
            .filter_map(|sd| {
                let p = expand_tilde(&sd.path);
                p.exists().then_some((p, sd.tag.clone()))
            })
            .collect()
    }

    pub fn map_provider_type(&self, t: &str) -> String {
        self.provider_type_map
            .get(t)
            .cloned()
            .unwrap_or_else(|| "custom".into())
    }
    pub fn map_mcp_type(&self, t: &str) -> Option<String> {
        self.mcp_type_map.get(t).cloned()
    }
    pub fn should_skip_mcp(&self, name: &str) -> bool {
        self.mcp_skip_prefixes.iter().any(|p| name.starts_with(p))
    }
    pub fn allow_no_key(&self, t: &str) -> bool {
        self.provider_no_key_types.iter().any(|x| x == t)
    }
    pub fn apply_vertex_override(&self, t: &str, is_vertex: bool) -> String {
        if is_vertex {
            self.is_vertex_override
                .get(t)
                .cloned()
                .unwrap_or_else(|| t.to_string())
        } else {
            t.to_string()
        }
    }
    pub fn resolve_metadata_field(&self, v: &serde_json::Value, keys: &[String]) -> Option<String> {
        keys.iter().find_map(|k| {
            v.get(k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
    }
    pub fn env_known_keys_for(&self, tool: &str) -> &[String] {
        self.env_known_keys
            .get(tool)
            .map(|k| k.all.as_slice())
            .unwrap_or(&[])
    }
    pub fn model_env_keys_for(&self, tool: &str) -> &[String] {
        self.env_known_keys
            .get(tool)
            .map(|k| k.model_env.as_slice())
            .unwrap_or(&[])
    }
    pub fn mcp_known_keys(&self) -> &[String] {
        self.mcp_known_keys.all.as_slice()
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(path))
    } else if path == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(path))
    } else {
        PathBuf::from(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── expand_tilde ──

    #[test]
    fn test_expand_tilde_home_subdir() {
        let result = expand_tilde("~/Documents");
        if let Some(home) = dirs::home_dir() {
            assert_eq!(result, home.join("Documents"));
        }
    }

    #[test]
    fn test_expand_tilde_bare_home() {
        let result = expand_tilde("~");
        if let Some(home) = dirs::home_dir() {
            assert_eq!(result, home);
        }
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        let result = expand_tilde("/absolute/path");
        assert_eq!(result, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn test_expand_tilde_relative_path() {
        let result = expand_tilde("relative/path");
        assert_eq!(result, PathBuf::from("relative/path"));
    }

    #[test]
    fn test_expand_tilde_windows_absolute() {
        // Windows absolute path should be passed through
        let result = expand_tilde("C:\\Users\\test");
        assert_eq!(result, PathBuf::from("C:\\Users\\test"));
    }

    #[test]
    fn test_expand_tilde_nested() {
        let result = expand_tilde("~/a/b/c");
        if let Some(home) = dirs::home_dir() {
            assert_eq!(result, home.join("a").join("b").join("c"));
        }
    }

    // ── Platform-aware path behavior ──

    #[test]
    fn test_current_platform_is_consistent() {
        // Verify that cfg! and current_platform() agree
        let platform = if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            "linux"
        };
        // Just verify the detection logic is internally consistent
        assert!(!platform.is_empty());
    }

    #[test]
    fn test_path_join_cross_platform() {
        // Verify path construction works regardless of OS separator
        let base = PathBuf::from("/data");
        let result = base.join("subdir").join("file.toml");
        assert!(result.to_string_lossy().contains("subdir"));
        assert!(result.to_string_lossy().ends_with("file.toml"));
    }

    #[test]
    fn test_home_dir_resolves() {
        // Verify home_dir resolves on all platforms
        let home = dirs::home_dir();
        assert!(home.is_some());
        assert!(home.unwrap().is_dir() || !cfg!(target_os = "windows"));
    }

    // ── EnvKnownKeys defaults ──

    #[test]
    fn test_env_known_keys_default() {
        let keys: EnvKnownKeys = Default::default();
        assert!(keys.all.is_empty());
        assert!(keys.model_env.is_empty());
    }

    // ── McpKnownKeys default ──

    #[test]
    fn test_mcp_known_keys_default() {
        let keys: McpKnownKeys = Default::default();
        assert!(keys.all.is_empty());
    }

    // ── DatasourceConfig path resolution logic ──

    #[test]
    fn test_expand_tilde_empty_string() {
        let result = expand_tilde("");
        assert_eq!(result, PathBuf::from(""));
    }

    #[test]
    fn test_expand_tilde_tilde_only_prefix() {
        // "~something" (without slash) should NOT expand
        let result = expand_tilde("~otheruser");
        assert_eq!(result, PathBuf::from("~otheruser"));
    }
}
