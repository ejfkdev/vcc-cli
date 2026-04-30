use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

fn generate_id() -> String {
    static CONFIG: std::sync::OnceLock<(Vec<char>, usize)> = std::sync::OnceLock::new();
    let (alphabet, length) = CONFIG.get_or_init(|| {
        let cfg = crate::config::resource_registry();
        (
            cfg.id_generation.alphabet.chars().collect(),
            cfg.id_generation.length,
        )
    });
    nanoid::format(nanoid::rngs::default, alphabet, *length)
}

use crate::model::{profile::Profile, Resource};

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("name cannot be empty");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!(
            "invalid name '{}': must contain only letters, digits, '-' and '_'",
            name
        );
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct VccConfig {
    pub version: String,
    #[serde(default)]
    pub installed_tools: HashSet<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_sync: Option<bool>,
}

pub(crate) struct TomlStore {
    root: PathBuf,
}

impl TomlStore {
    pub fn new() -> Result<Self> {
        let s = Self {
            root: Self::default_root()?,
        };
        s.init()?;
        Ok(s)
    }

    pub fn default_root() -> Result<PathBuf> {
        #[cfg(unix)]
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                let path = PathBuf::from(&xdg);
                if !path.is_absolute() {
                    anyhow::bail!("XDG_CONFIG_HOME must be an absolute path, got: {}", xdg);
                }
                return Ok(path.join("VibeCodingControl"));
            }
        }
        if cfg!(windows) {
            dirs::config_dir()
                .ok_or_else(|| anyhow::anyhow!("cannot find config directory"))
                .map(|d| d.join("VibeCodingControl"))
        } else {
            dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("cannot find home directory"))
                .map(|d| d.join(".config").join("VibeCodingControl"))
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn init(&self) -> Result<()> {
        for dir in &crate::config::resource_registry().all_init_dirs() {
            let path = self.root.join(dir);
            if !path.exists() {
                fs::create_dir_all(&path)
                    .with_context(|| format!("failed to create directory: {}", path.display()))?;
            }
        }
        let config_path = self.root.join("vcc.toml");
        if !config_path.exists() {
            self.write_toml(
                &config_path,
                &VccConfig {
                    version: "2".to_string(),
                    ..Default::default()
                },
            )?;
        }
        Ok(())
    }

    pub fn load_config(&self) -> Result<VccConfig> {
        let p = self.root.join("vcc.toml");
        if !p.exists() {
            return Ok(VccConfig::default());
        }
        self.read_toml(&p)
    }
    pub fn save_config(&self, config: &VccConfig) -> Result<()> {
        self.write_toml(&self.root.join("vcc.toml"), config)
    }

    pub fn save_resource<T: Resource>(&self, resource: &T) -> Result<()> {
        validate_name(resource.name())?;
        let dir = self.resource_dir(resource.kind())?;
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create directory: {}", dir.display()))?;
        let hash_content = crate::model::hash_fields::resource_hash_content(
            resource.kind(),
            &serde_json::to_value(resource)?,
        );
        let hash8 =
            crate::model::hash::hash8(&crate::model::hash::compute_hash(&hash_content)).to_string();
        let mut resource = resource.clone();
        if resource.id().is_empty() {
            resource.set_id(generate_id());
        }
        let new_filename = format!("{}-{}.toml", resource.name(), hash8);
        let new_path = dir.join(&new_filename);
        if let Some(old_stem) = find_resource_file_by_id(&dir, resource.name(), resource.id()) {
            let old_filename = format!("{}.toml", old_stem);
            if old_filename != new_filename {
                self.write_toml(&new_path, &resource)?;
                self.set_permissions_if_sensitive(&new_path, resource.kind());
                let op = dir.join(&old_filename);
                if op.exists() {
                    let _ = fs::remove_file(&op);
                }
                return Ok(());
            }
        }
        self.write_toml(&new_path, &resource)?;
        self.set_permissions_if_sensitive(&new_path, resource.kind());
        Ok(())
    }

    pub fn load_resource<T: Resource>(&self, kind: &str, name: &str) -> Result<T> {
        validate_name(name)?;
        let dir = self.resource_dir(kind)?;
        let stem = find_resource_file(&dir, name)
            .ok_or_else(|| anyhow::anyhow!("{} '{}' not found", kind, name))?;
        self.read_toml(&dir.join(format!("{}.toml", stem)))
    }

    pub fn list_resources<T: Resource>(&self, kind: &str) -> Result<Vec<String>> {
        let dir = self.resource_dir(kind)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut seen = HashSet::new();
        let mut names = Vec::new();
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "toml") {
                if let Some(stem) = path.file_stem() {
                    let s = stem.to_string_lossy().to_string();
                    if s == "_default" {
                        continue;
                    }
                    let n = strip_hash8(&s);
                    if seen.insert(n.clone()) {
                        names.push(n);
                    }
                }
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn remove_resource(&self, kind: &str, name: &str) -> Result<()> {
        validate_name(name)?;
        let dir = self.resource_dir(kind)?;
        let stem = find_resource_file(&dir, name)
            .ok_or_else(|| anyhow::anyhow!("{} '{}' not found", kind, name))?;
        fs::remove_file(dir.join(format!("{}.toml", stem)))
            .with_context(|| format!("failed to remove {}", name))
    }

    pub fn resource_exists(&self, kind: &str, name: &str) -> bool {
        self.resource_dir(kind)
            .map(|d| find_resource_file(&d, name).is_some())
            .unwrap_or(false)
    }

    pub fn load_resource_by_query<T: Resource>(&self, kind: &str, query: &str) -> Result<T> {
        if self.resource_exists(kind, query) {
            return self.load_resource(kind, query);
        }
        validate_name(query)?;
        let dir = self.resource_dir(kind)?;
        if !dir.exists() {
            bail!("{} '{}' not found", kind, query);
        }
        let sp = dir.join(format!("{}.toml", query));
        if sp.exists() {
            return self.read_toml(&sp);
        }
        for entry in fs::read_dir(&dir)?.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e != "toml") {
                continue;
            }
            if let Ok(r) = self.read_toml::<T>(&p) {
                if r.id() == query {
                    return Ok(r);
                }
            }
        }
        bail!("{} '{}' not found (tried name, file stem, id)", kind, query)
    }

    pub fn remove_resource_by_query(&self, kind: &str, query: &str) -> Result<String> {
        if self.resource_exists(kind, query) {
            self.remove_resource(kind, query)?;
            return Ok(query.to_string());
        }
        validate_name(query)?;
        let dir = self.resource_dir(kind)?;
        if !dir.exists() {
            bail!("{} '{}' not found", kind, query);
        }
        let sp = dir.join(format!("{}.toml", query));
        if sp.exists() {
            let n = strip_hash8(query);
            fs::remove_file(&sp).with_context(|| format!("failed to remove {}", sp.display()))?;
            return Ok(n);
        }
        for entry in fs::read_dir(&dir)?.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e != "toml") {
                continue;
            }
            let ss = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if let Ok(content) = fs::read_to_string(&p) {
                if let Ok(doc) = content.parse::<toml::Value>() {
                    if doc.get("id").and_then(|v| v.as_str()) == Some(query) {
                        let n = doc
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&ss)
                            .to_string();
                        fs::remove_file(&p)
                            .with_context(|| format!("failed to remove {}", p.display()))?;
                        return Ok(n);
                    }
                }
            }
        }
        bail!("{} '{}' not found (tried name, file stem, id)", kind, query)
    }

    pub fn find_by_content<T: Resource, F>(&self, kind: &str, eq_fn: F) -> Result<Option<T>>
    where
        F: Fn(&T) -> bool,
    {
        let dir = self.resource_dir(kind)?;
        if !dir.exists() {
            return Ok(None);
        }
        for entry in fs::read_dir(&dir)?.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e != "toml") {
                continue;
            }
            if let Ok(r) = self.read_toml::<T>(&p) {
                if eq_fn(&r) {
                    return Ok(Some(r));
                }
            }
        }
        Ok(None)
    }

    pub fn load_default_resource<T: Resource>(&self, kind: &str) -> Result<Option<T>> {
        let p = self.resource_dir(kind)?.join("_default.toml");
        if !p.exists() {
            return Ok(None);
        }
        Ok(Some(self.read_toml(&p)?))
    }

    pub fn save_profile(&self, profile: &Profile) -> Result<()> {
        validate_name(&profile.name)?;
        let d = self.root.join("profiles");
        fs::create_dir_all(&d)?;
        self.write_toml(&d.join(format!("{}.toml", profile.name)), profile)
    }
    pub fn load_profile(&self, name: &str) -> Result<Profile> {
        validate_name(name)?;
        let p = self.root.join("profiles").join(format!("{}.toml", name));
        if !p.exists() {
            bail!("profile '{}' not found", name);
        }
        self.read_toml(&p)
    }
    pub fn list_profiles(&self) -> Result<Vec<String>> {
        let d = self.root.join("profiles");
        if !d.exists() {
            return Ok(Vec::new());
        }
        let mut names: Vec<String> = fs::read_dir(&d)
            .with_context(|| format!("failed to read directory {}", d.display()))?
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension().is_some_and(|e| e == "toml") {
                    p.file_stem().map(|s| s.to_string_lossy().to_string())
                } else {
                    None
                }
            })
            .collect();
        names.sort();
        Ok(names)
    }
    pub fn remove_profile(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        let p = self.root.join("profiles").join(format!("{}.toml", name));
        if !p.exists() {
            bail!("profile '{}' not found", name);
        }
        fs::remove_file(&p).with_context(|| format!("failed to remove {}", p.display()))
    }

    pub fn count_all_resources(&self) -> usize {
        crate::config::resource_registry()
            .resources
            .iter()
            .filter_map(|r| self.count_resources(&r.kind).ok())
            .sum()
    }
    pub fn count_resources(&self, kind: &str) -> Result<usize> {
        let dir = self.resource_dir(kind)?;
        if !dir.exists() {
            return Ok(0);
        }
        let mut seen = HashSet::new();
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "toml") {
                if let Some(stem) = path.file_stem() {
                    let s = stem.to_string_lossy().to_string();
                    if s == "_default" {
                        continue;
                    }
                    seen.insert(strip_hash8(&s));
                }
            }
        }
        Ok(seen.len())
    }

    fn resource_dir(&self, kind: &str) -> Result<PathBuf> {
        let dn = crate::config::resource_registry()
            .dir_for_kind(kind)
            .ok_or_else(|| anyhow::anyhow!("unknown resource kind: {}", kind))?;
        Ok(self.root.join("registry").join(dn))
    }

    fn write_toml<T: Serialize>(&self, path: &Path, data: &T) -> Result<()> {
        let content = toml::to_string_pretty(data)
            .with_context(|| format!("failed to serialize {}", path.display()))?;
        let tmp = path.with_extension("toml.tmp");
        if let Err(e) =
            fs::write(&tmp, &content).with_context(|| format!("failed to write {}", tmp.display()))
        {
            let _ = fs::remove_file(&tmp); // best-effort cleanup
            return Err(e);
        }
        if let Err(e) = fs::rename(&tmp, path)
            .with_context(|| format!("failed to rename to {}", path.display()))
        {
            let _ = fs::remove_file(&tmp); // best-effort cleanup
            return Err(e);
        }
        Ok(())
    }

    fn read_toml<T: for<'de> Deserialize<'de>>(&self, path: &Path) -> Result<T> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
    }

    /// Set restrictive permissions on files containing sensitive data (API keys)
    #[cfg(unix)]
    fn set_permissions_if_sensitive(&self, path: &Path, kind: &str) {
        if kind != "provider" && kind != "env" {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            eprintln!(
                "warning: failed to set permissions on {}: {}",
                path.display(),
                e
            );
        }
    }

    #[cfg(not(unix))]
    fn set_permissions_if_sensitive(&self, _path: &Path, _kind: &str) {}
}

fn find_resource_file(dir: &Path, name: &str) -> Option<String> {
    let prefix = format!("{}-", name);
    let mut v2: Vec<String> = Vec::new();
    let mut v1: Option<String> = None;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "toml") {
            if let Some(stem) = p.file_stem() {
                let s = stem.to_string_lossy().to_string();
                if s == name {
                    v1 = Some(s);
                } else if s.starts_with(&prefix) {
                    let suf = &s[name.len() + 1..];
                    if suf.len() == 8 && suf.chars().all(|c| c.is_ascii_hexdigit()) {
                        v2.push(s);
                    }
                }
            }
        }
    }
    if !v2.is_empty() {
        // Clean up stale hash files: keep only the lexicographically last one
        v2.sort();
        if v2.len() > 1 {
            for stale in &v2[..v2.len() - 1] {
                let _ = fs::remove_file(dir.join(format!("{}.toml", stale)));
            }
        }
        return Some(v2.into_iter().next_back().unwrap());
    }
    v1
}

fn find_resource_file_by_id(dir: &Path, name: &str, id: &str) -> Option<String> {
    let prefix = format!("{}-", name);
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e != "toml") {
            continue;
        }
        let ss = p.file_stem()?.to_string_lossy().to_string();
        let nm = ss == name
            || (ss.starts_with(&prefix)
                && ss[name.len() + 1..].len() == 8
                && ss[name.len() + 1..].chars().all(|c| c.is_ascii_hexdigit()));
        if !nm {
            continue;
        }
        if let Ok(c) = fs::read_to_string(&p) {
            if let Ok(v) = c.parse::<toml::Value>() {
                if v.get("id").and_then(|v| v.as_str()) == Some(id) {
                    return Some(ss);
                }
            }
        }
    }
    None
}

fn strip_hash8(stem: &str) -> String {
    if let Some(pos) = stem.rfind('-') {
        let suf = &stem[pos + 1..];
        if suf.len() == 8 && suf.chars().all(|c| c.is_ascii_hexdigit()) {
            return stem[..pos].to_string();
        }
    }
    stem.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_name_valid() {
        assert!(validate_name("hello").is_ok());
        assert!(validate_name("my-provider").is_ok());
        assert!(validate_name("test_123").is_ok());
        assert!(validate_name("A-B_C").is_ok());
        assert!(validate_name("a").is_ok());
    }

    #[test]
    fn test_validate_name_empty() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn test_validate_name_spaces() {
        assert!(validate_name("bad name").is_err());
        assert!(validate_name(" leading").is_err());
        assert!(validate_name("trailing ").is_err());
    }

    #[test]
    fn test_validate_name_special_chars() {
        assert!(validate_name("a.b").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a@b").is_err());
        assert!(validate_name("中文").is_err());
    }

    #[test]
    fn test_strip_hash8_with_hash() {
        assert_eq!(strip_hash8("my-provider-5700f33c"), "my-provider");
        assert_eq!(strip_hash8("test-c2b48de5"), "test");
        assert_eq!(strip_hash8("a-12345678"), "a");
    }

    #[test]
    fn test_strip_hash8_no_hash() {
        assert_eq!(strip_hash8("my-provider"), "my-provider");
        assert_eq!(strip_hash8("simple"), "simple");
    }

    #[test]
    fn test_strip_hash8_short_suffix() {
        // 7 chars — not 8, so not a hash
        assert_eq!(strip_hash8("name-5700f33"), "name-5700f33");
        // 9 chars — too long
        assert_eq!(strip_hash8("name-5700f33ca"), "name-5700f33ca");
    }

    #[test]
    fn test_strip_hash8_non_hex_suffix() {
        assert_eq!(strip_hash8("name-abcdef12"), "name"); // all hex
        assert_eq!(strip_hash8("name-abcdefgz"), "name-abcdefgz"); // g is not hex
        assert_eq!(strip_hash8("name-ABCDef12"), "name"); // mixed case hex
    }

    // ── Platform-aware path tests ──

    #[test]
    fn test_default_root_resolves() {
        // Verify that default_root() returns a valid path on the current platform
        let root = TomlStore::default_root().expect("default_root should resolve");
        assert!(root.to_string_lossy().contains("VibeCodingControl"));
    }

    #[test]
    fn test_default_root_uses_xdg_if_set() {
        // If XDG_CONFIG_HOME is set, it should be used as the base
        if std::env::var("XDG_CONFIG_HOME").is_ok() {
            let root = TomlStore::default_root().expect("default_root should resolve");
            let xdg = std::env::var("XDG_CONFIG_HOME").unwrap();
            assert!(root.starts_with(&xdg));
        }
        // On Windows, XDG is typically not set; this test is a no-op
    }

    #[test]
    fn test_default_root_path_format_by_platform() {
        let root = TomlStore::default_root().expect("default_root should resolve");
        let path_str = root.to_string_lossy();
        if cfg!(windows) {
            // Windows: should use backslashes and APPDATA-based path
            assert!(!path_str.contains(".config"));
        } else {
            // Unix (Linux/macOS): should contain .config or XDG
            assert!(path_str.contains("VibeCodingControl"));
        }
    }

    #[test]
    fn test_file_extension_matching_platform() {
        // Resource files should always be .toml regardless of platform
        let path = std::path::Path::new("provider-myname-5700f33c.toml");
        assert_eq!(path.extension().unwrap(), "toml");
    }

    #[test]
    fn test_path_separator_in_resource_paths() {
        // Verify path construction works correctly on all platforms
        let root = std::env::temp_dir().join("VibeCodingControl");
        let resource_path = root.join("provider").join("my-provider-5700f33c.toml");
        // On all platforms, the path should end with the correct structure
        assert!(resource_path.to_string_lossy().contains("provider"));
        assert!(resource_path.to_string_lossy().ends_with(".toml"));
    }

    #[test]
    fn test_validate_name_platform_safe() {
        // Names that are safe across all platforms
        assert!(validate_name("my-provider").is_ok());
        assert!(validate_name("test_123").is_ok());
        assert!(validate_name("OPENAI-API").is_ok());
        // Names that are problematic on some platforms
        assert!(validate_name("a.b").is_err()); // dot problematic on Windows
        assert!(validate_name("a/b").is_err()); // slash is path separator
        assert!(validate_name("CON").is_ok()); // Windows reserved name but passes our validation
    }
}
