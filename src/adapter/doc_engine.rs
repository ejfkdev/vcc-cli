use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DocValue {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Array(Vec<DocValue>),
    Object(HashMap<String, DocValue>),
}

#[allow(dead_code)]
impl DocValue {
    pub fn is_null(&self) -> bool {
        matches!(self, DocValue::Null)
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            DocValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            DocValue::Integer(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            DocValue::Float(f) => Some(*f),
            DocValue::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            DocValue::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&Vec<DocValue>> {
        match self {
            DocValue::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&HashMap<String, DocValue>> {
        match self {
            DocValue::Object(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_object_mut(&mut self) -> Option<&mut HashMap<String, DocValue>> {
        match self {
            DocValue::Object(m) => Some(m),
            _ => None,
        }
    }

    pub fn get_path(&self, path: &str) -> Option<&DocValue> {
        let mut cur = self;
        for key in path.split('.') {
            cur = match cur {
                DocValue::Object(m) => m.get(key)?,
                _ => return None,
            };
        }
        Some(cur)
    }
    pub fn get_path_str(&self, path: &str) -> Option<&str> {
        self.get_path(path).and_then(|v| v.as_str())
    }

    pub fn set_path(&mut self, path: &str, value: DocValue) {
        let keys: Vec<&str> = path.split('.').collect();
        if !keys.is_empty() {
            self.set_path_impl(&keys, value);
        }
    }

    fn set_path_impl(&mut self, keys: &[&str], value: DocValue) {
        if keys.len() == 1 {
            if let DocValue::Object(m) = self {
                m.insert(keys[0].to_string(), value);
            }
            return;
        }
        if let DocValue::Object(m) = self {
            if !m.contains_key(keys[0]) {
                m.insert(keys[0].to_string(), DocValue::Object(HashMap::new()));
            }
            if let Some(c) = m.get_mut(keys[0]) {
                c.set_path_impl(&keys[1..], value);
            }
        }
    }

    pub fn remove_path(&mut self, path: &str) -> bool {
        let keys: Vec<&str> = path.split('.').collect();
        !keys.is_empty() && self.remove_path_impl(&keys)
    }
    fn remove_path_impl(&mut self, keys: &[&str]) -> bool {
        match keys.len() {
            1 => {
                if let DocValue::Object(m) = self {
                    m.remove(keys[0]).is_some()
                } else {
                    false
                }
            }
            _ => {
                if let DocValue::Object(m) = self {
                    m.get_mut(keys[0])
                        .is_some_and(|c| c.remove_path_impl(&keys[1..]))
                } else {
                    false
                }
            }
        }
    }

    pub fn path_exists(&self, path: &str) -> bool {
        self.get_path(path).is_some()
    }
    pub fn entries(&self, path: &str) -> Option<Vec<(String, &DocValue)>> {
        self.get_path(path).and_then(|v| match v {
            DocValue::Object(m) => Some(m.iter().map(|(k, v)| (k.clone(), v)).collect()),
            _ => None,
        })
    }
    pub fn push(&mut self, path: &str, value: DocValue) {
        if let Some(DocValue::Array(a)) = self.get_path_mut(path) {
            a.push(value);
            return;
        }
        self.set_path(path, DocValue::Array(vec![value]));
    }
    fn get_path_mut(&mut self, path: &str) -> Option<&mut DocValue> {
        let keys: Vec<&str> = path.split('.').collect();
        let mut cur = self;
        for key in &keys[..keys.len().saturating_sub(1)] {
            cur = match cur {
                DocValue::Object(m) => m.get_mut(*key)?,
                _ => return None,
            };
        }
        let last = keys.last()?;
        if let DocValue::Object(m) = cur {
            m.get_mut(*last)
        } else {
            None
        }
    }
    pub fn ensure_object(&mut self, path: &str) {
        if !self.path_exists(path) {
            self.set_path(path, DocValue::Object(HashMap::new()));
        }
    }
    pub fn retain_in_array(&mut self, path: &str, pred: impl Fn(&DocValue) -> bool) {
        if let Some(DocValue::Array(a)) = self.get_path_mut(path) {
            a.retain(pred);
        }
    }
    /// Extract name→value map from an array where each entry is a string or
    /// an array whose first element is a string.
    pub fn extract_array_name_map(&self) -> HashMap<String, DocValue> {
        match self {
            DocValue::Array(arr) => arr
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(|s: &str| (s.to_string(), item.clone()))
                        .or_else(|| {
                            item.as_array()
                                .and_then(|a| a.first().and_then(|f| f.as_str()))
                                .map(|s: &str| (s.to_string(), item.clone()))
                        })
                })
                .collect(),
            _ => HashMap::new(),
        }
    }
    /// Construct an Array variant by converting each element.
    pub fn from_mapped_array<I, T>(iter: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<DocValue>,
    {
        DocValue::Array(iter.into_iter().map(Into::into).collect())
    }
    /// Construct an Object variant by converting each value.
    pub fn from_mapped_object<I, T>(iter: I) -> Self
    where
        I: IntoIterator<Item = (String, T)>,
        T: Into<DocValue>,
    {
        DocValue::Object(iter.into_iter().map(|(k, v)| (k, v.into())).collect())
    }
}

impl From<serde_json::Value> for DocValue {
    fn from(v: serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => DocValue::Null,
            serde_json::Value::Bool(b) => DocValue::Bool(b),
            serde_json::Value::Number(n) => n
                .as_i64()
                .map(DocValue::Integer)
                .or_else(|| n.as_f64().map(DocValue::Float))
                .unwrap_or_else(|| DocValue::String(n.to_string())),
            serde_json::Value::String(s) => DocValue::String(s),
            serde_json::Value::Array(a) => DocValue::from_mapped_array(a),
            serde_json::Value::Object(o) => DocValue::from_mapped_object(o),
        }
    }
}

impl From<DocValue> for serde_json::Value {
    fn from(v: DocValue) -> Self {
        match v {
            DocValue::Null => serde_json::Value::Null,
            DocValue::Bool(b) => serde_json::Value::Bool(b),
            DocValue::Integer(i) => serde_json::Value::Number(i.into()),
            DocValue::Float(f) => serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            DocValue::String(s) => serde_json::Value::String(s),
            DocValue::Array(a) => serde_json::Value::Array(a.into_iter().map(Into::into).collect()),
            DocValue::Object(m) => {
                serde_json::Value::Object(m.into_iter().map(|(k, v)| (k, v.into())).collect())
            }
        }
    }
}

impl From<toml::Value> for DocValue {
    fn from(v: toml::Value) -> Self {
        match v {
            toml::Value::String(s) => DocValue::String(s),
            toml::Value::Integer(i) => DocValue::Integer(i),
            toml::Value::Float(f) => DocValue::Float(f),
            toml::Value::Boolean(b) => DocValue::Bool(b),
            toml::Value::Datetime(dt) => DocValue::String(dt.to_string()),
            toml::Value::Array(a) => DocValue::from_mapped_array(a),
            toml::Value::Table(t) => DocValue::from_mapped_object(t),
        }
    }
}

impl From<DocValue> for toml::Value {
    fn from(v: DocValue) -> Self {
        match v {
            DocValue::Null => toml::Value::String("null".into()),
            DocValue::Bool(b) => toml::Value::Boolean(b),
            DocValue::Integer(i) => toml::Value::Integer(i),
            DocValue::Float(f) => toml::Value::Float(f),
            DocValue::String(s) => toml::Value::String(s),
            DocValue::Array(a) => toml::Value::Array(a.into_iter().map(Into::into).collect()),
            DocValue::Object(m) => {
                toml::Value::Table(m.into_iter().map(|(k, v)| (k, v.into())).collect())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DocFormat {
    Json,
    Toml,
    Yaml,
    Env,
}

impl DocFormat {
    pub fn from_provider_format(f: &str) -> Self {
        match f {
            "toml_provider_table" => DocFormat::Toml,
            "yaml_flat" => DocFormat::Yaml,
            _ => DocFormat::Json,
        }
    }
    pub fn from_format_str(f: &str) -> Self {
        match f {
            "toml" | "toml_hooks" | "toml_table" => DocFormat::Toml,
            "yaml" | "yaml_flat" => DocFormat::Yaml,
            "env" | "env_file" => DocFormat::Env,
            _ => DocFormat::Json,
        }
    }
    /// Infer format from a filename's extension (e.g. "settings.json" → Json).
    pub fn from_filename(name: &str) -> Self {
        match name.rsplit('.').next().unwrap_or("") {
            "toml" => DocFormat::Toml,
            "yml" | "yaml" => DocFormat::Yaml,
            _ => DocFormat::Json,
        }
    }
    pub fn default_filename(&self) -> &'static str {
        match self {
            DocFormat::Json => "settings.json",
            DocFormat::Toml => "config.toml",
            DocFormat::Yaml => ".aider.conf.yml",
            DocFormat::Env => ".env",
        }
    }
}

pub(crate) struct DocTree {
    root: DocValue,
    format: DocFormat,
    path: PathBuf,
}

impl DocTree {
    /// Create an in-memory DocTree for testing (no file I/O).
    #[cfg(test)]
    pub fn new_test(format: DocFormat, root: DocValue) -> Self {
        DocTree {
            root,
            format,
            path: std::env::temp_dir().join("test"),
        }
    }
}

fn matches_at_suffix(key: &str, name: &str) -> bool {
    key == name || key.starts_with(&format!("{}@", name))
}

fn insert_enabled_entry(
    m: &mut HashMap<String, DocValue>,
    key: &str,
    enabled: bool,
    is_json: bool,
) {
    if is_json {
        m.insert(key.to_string(), DocValue::Bool(enabled));
    } else if let Some(DocValue::Object(em)) = m.get_mut(key) {
        em.insert("enabled".into(), DocValue::Bool(enabled));
    } else {
        m.insert(
            key.to_string(),
            DocValue::Object(HashMap::from([("enabled".into(), DocValue::Bool(enabled))])),
        );
    }
}

fn atomic_write(path: &Path, content: &str, tmp_ext: &str) -> Result<()> {
    let tmp = path.with_extension(tmp_ext);
    std::fs::write(&tmp, content)?;
    // Windows: rename 不覆盖已存在文件，需先删除目标
    #[cfg(not(unix))]
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

impl DocTree {
    pub fn load(format: DocFormat, path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(DocTree {
                root: DocValue::Object(HashMap::new()),
                format,
                path: path.to_path_buf(),
            });
        }
        let root = match format {
            DocFormat::Json => {
                serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path)?)?.into()
            }
            DocFormat::Toml => std::fs::read_to_string(path)?
                .parse::<toml::Value>()?
                .into(),
            DocFormat::Yaml => read_yaml_flat(path),
            DocFormat::Env => read_env_file(path),
        };
        Ok(DocTree {
            root,
            format,
            path: path.to_path_buf(),
        })
    }

    pub fn load_with_options(
        format: DocFormat,
        path: &Path,
        jsonc: bool,
        fallback_paths: &[PathBuf],
    ) -> Result<Self> {
        if format == DocFormat::Json && (jsonc || !fallback_paths.is_empty()) {
            let mut cands = vec![path.to_path_buf()];
            cands.extend(fallback_paths.iter().cloned());
            let (val, actual) = crate::adapter::read_jsonc_with_fallback(&cands)?;
            return Ok(DocTree {
                root: val.into(),
                format,
                path: actual,
            });
        }
        Self::load(format, path)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&self.path)
    }

    pub fn get(&self, p: &str) -> Option<&DocValue> {
        self.root.get_path(p)
    }
    pub fn get_str(&self, p: &str) -> Option<&str> {
        self.root.get_path_str(p)
    }
    pub fn set(&mut self, p: &str, v: DocValue) {
        self.root.set_path(p, v);
    }
    pub fn remove(&mut self, p: &str) -> bool {
        self.root.remove_path(p)
    }
    pub fn exists(&self, p: &str) -> bool {
        self.root.path_exists(p)
    }
    pub fn entries(&self, p: &str) -> Option<Vec<(String, &DocValue)>> {
        self.root.entries(p)
    }
    pub fn push(&mut self, p: &str, v: DocValue) {
        self.root.push(p, v);
    }
    pub fn ensure_object(&mut self, p: &str) {
        self.root.ensure_object(p);
    }
    pub fn retain_in_array(&mut self, p: &str, pred: impl Fn(&DocValue) -> bool) {
        self.root.retain_in_array(p, pred);
    }
    pub fn extract_array_name_map(&self, p: &str) -> HashMap<String, DocValue> {
        self.root
            .get_path(p)
            .map(|v| v.extract_array_name_map())
            .unwrap_or_default()
    }
    pub fn root(&self) -> &DocValue {
        &self.root
    }
    pub fn root_mut(&mut self) -> &mut DocValue {
        &mut self.root
    }
    pub fn format(&self) -> DocFormat {
        self.format
    }
    pub fn is_empty(&self) -> bool {
        matches!(&self.root, DocValue::Object(m) if m.is_empty())
    }
    pub fn clear_section(&mut self, p: &str) {
        self.set(p, DocValue::Object(HashMap::new()));
    }

    pub fn remove_from_object(&mut self, p: &str, name: &str) -> bool {
        self.get_object_mut(p, false)
            .is_some_and(|m| m.remove(name).is_some())
    }
    pub fn remove_matching_from_object(&mut self, p: &str, name: &str) -> usize {
        if let Some(m) = self.get_object_mut(p, false) {
            let keys: Vec<String> = m
                .keys()
                .filter(|k| matches_at_suffix(k, name))
                .cloned()
                .collect();
            let n = keys.len();
            for k in keys {
                m.remove(&k);
            }
            n
        } else {
            0
        }
    }
    pub fn find_matching_key(&self, p: &str, name: &str) -> Option<String> {
        self.entries(p).and_then(|e| {
            e.into_iter()
                .map(|(k, _)| k)
                .find(|k| matches_at_suffix(k, name))
        })
    }
    pub fn get_entry_mut(&mut self, p: &str, key: &str) -> Option<&mut DocValue> {
        self.get_object_mut(p, false).and_then(|m| m.get_mut(key))
    }
    fn get_object_mut(&mut self, p: &str, ensure: bool) -> Option<&mut HashMap<String, DocValue>> {
        if ensure {
            self.ensure_object(p);
        }
        if let Some(DocValue::Object(m)) = self.root.get_path_mut(p) {
            Some(m)
        } else {
            None
        }
    }
    pub fn set_entry(&mut self, p: &str, key: &str, v: DocValue) {
        if let Some(m) = self.get_object_mut(p, true) {
            m.insert(key.to_string(), v);
        }
    }
    /// Insert or update an entry with a boolean enabled flag, format-aware.
    /// JSON format stores `key: bool`, TOML/YAML stores `key: { enabled: bool }`.
    pub fn set_entry_bool(&mut self, p: &str, key: &str, enabled: bool) {
        let is_json = self.format == DocFormat::Json;
        if let Some(m) = self.get_object_mut(p, true) {
            insert_enabled_entry(m, key, enabled, is_json);
        }
    }
    #[allow(dead_code)]
    pub fn set_entry_enabled(&mut self, p: &str, name: &str, enabled: bool) {
        let is_json = self.format == DocFormat::Json;
        if let Some(m) = self.get_object_mut(p, true) {
            let matching: Vec<String> = m
                .keys()
                .filter(|k| matches_at_suffix(k, name))
                .cloned()
                .collect();
            if matching.is_empty() {
                let rn = format!("{}@marketplace", name);
                insert_enabled_entry(m, &rn, enabled, is_json);
            } else {
                for k in matching {
                    insert_enabled_entry(m, &k, enabled, is_json);
                }
            }
        }
    }
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            if !p.exists() {
                std::fs::create_dir_all(p)?;
            }
        }
        match self.format {
            DocFormat::Json => {
                let content =
                    serde_json::to_string_pretty(&serde_json::Value::from(self.root.clone()))?;
                atomic_write(path, &content, "json.tmp")?;
            }
            DocFormat::Toml => {
                let content = toml::to_string_pretty::<toml::Value>(&self.root.clone().into())?;
                atomic_write(path, &content, "toml.tmp")?;
            }
            DocFormat::Yaml => write_yaml_doc(path, &self.root)?,
            DocFormat::Env => write_env_file(path, &self.root)?,
        }
        Ok(())
    }
}

struct KvReadOpts {
    delimiter: char,
    skip_dash: bool,
    require_value: bool,
}

fn read_kv_file(path: &Path, opts: KvReadOpts) -> DocValue {
    let mut map = HashMap::new();
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return DocValue::Object(map),
    };
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || (opts.skip_dash && t.starts_with('-')) {
            continue;
        }
        if let Some((k, v)) = t.split_once(opts.delimiter) {
            let k = k.trim().to_string();
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(v)
                .to_string();
            if !k.is_empty() && (!opts.require_value || !v.is_empty()) {
                map.insert(k, DocValue::String(v));
            }
        }
    }
    DocValue::Object(map)
}

fn read_yaml_flat(path: &Path) -> DocValue {
    read_kv_file(
        path,
        KvReadOpts {
            delimiter: ':',
            skip_dash: true,
            require_value: true,
        },
    )
}

fn yaml_escape_value(v: &str) -> String {
    let q = v.contains(':')
        || v.contains('#')
        || v.contains('{')
        || v.contains('}')
        || v.contains('[')
        || v.contains(']')
        || v.contains(',')
        || v.contains('&')
        || v.contains('*')
        || v.contains('!')
        || v.contains('|')
        || v.contains('>')
        || v.contains('%')
        || v.contains('@')
        || v.contains('`')
        || v.starts_with(' ')
        || v.starts_with('"')
        || v.starts_with('\'')
        || v == "true"
        || v == "false"
        || v == "null"
        || v.parse::<f64>().is_ok();
    if q {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        v.to_string()
    }
}

fn write_yaml_doc(path: &Path, value: &DocValue) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("YAML root must be an object"))?;
    let mut entries = std::collections::HashMap::new();
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            entries.insert(k.clone(), s.to_string());
        } else if let Some(i) = v.as_i64() {
            entries.insert(k.clone(), i.to_string());
        } else if let Some(b) = v.as_bool() {
            entries.insert(k.clone(), b.to_string());
        }
    }
    let mut lines: Vec<String> = Vec::new();
    let mut written: std::collections::HashSet<String> = std::collections::HashSet::new();
    if path.exists() {
        for line in std::fs::read_to_string(path)?.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') || t.starts_with('-') {
                lines.push(line.to_string());
                continue;
            }
            if let Some((k, _)) = t.split_once(':') {
                let k = k.trim().to_string();
                if let Some(nv) = entries.get(&k) {
                    lines.push(format!("{}: {}", k, yaml_escape_value(nv)));
                    written.insert(k);
                } else {
                    lines.push(line.to_string());
                }
            } else {
                lines.push(line.to_string());
            }
        }
    }
    for (k, v) in &entries {
        if !written.contains(k) {
            lines.push(format!("{}: {}", k, yaml_escape_value(v)));
        }
    }
    std::fs::write(path, lines.join("\n") + "\n")?;
    Ok(())
}

fn read_env_file(path: &Path) -> DocValue {
    read_kv_file(
        path,
        KvReadOpts {
            delimiter: '=',
            skip_dash: false,
            require_value: false,
        },
    )
}

fn write_env_file(path: &Path, value: &DocValue) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!(".env root must be an object"))?;
    let lines: Vec<String> = obj
        .iter()
        .filter_map(|(k, v)| {
            v.as_str().map(|s| {
                // Quote values containing spaces, special chars, or shell metacharacters
                if s.contains(' ')
                    || s.contains('"')
                    || s.contains('\'')
                    || s.contains('$')
                    || s.contains('\\')
                    || s.contains('\n')
                    || s.contains('#')
                {
                    let escaped = s
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('\n', "\\n");
                    format!("{}=\"{}\"", k, escaped)
                } else {
                    format!("{}={}", k, s)
                }
            })
        })
        .collect();
    std::fs::write(path, lines.join("\n") + "\n")?;
    Ok(())
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct FieldSpec {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub fallback: Vec<String>,
    #[serde(default)]
    pub write_to: Vec<String>,
    #[serde(default)]
    pub read_strategy: Option<String>,
    #[serde(default)]
    pub write_strategy: Option<String>,
    #[serde(default = "default_scope")]
    pub scope: String,
}
fn default_scope() -> String {
    "entry".to_string()
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct FieldMapConfig {
    #[serde(default)]
    pub entries_path: String,
    #[serde(default)]
    pub fields: HashMap<String, FieldSpec>,
    #[serde(default)]
    pub inject_on_write: HashMap<String, toml::Value>,
    #[serde(default)]
    pub type_fields: Vec<String>,
    #[serde(default)]
    pub model_field: Option<String>,
    #[serde(default)]
    pub enabled_field: Option<String>,
}

fn apply_read_strategy(
    strategy: &str,
    val: &DocValue,
    _spec: &FieldSpec,
    _key: &str,
) -> Option<toml::Value> {
    match strategy {
        "object_keys" => match val {
            DocValue::Object(m) => Some(toml::Value::Array(
                m.keys().map(|k| toml::Value::String(k.clone())).collect(),
            )),
            _ => None,
        },
        "split_slash_last" => val.as_str().and_then(|s| {
            s.rsplit_once('/')
                .map(|(_, l)| toml::Value::String(l.to_string()))
                .or_else(|| Some(toml::Value::String(s.to_string())))
        }),
        "env_resolve" => val
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| toml::Value::String(s.to_string())),
        _ => None,
    }
}

pub(crate) fn sync_entries(
    doc: &DocTree,
    field_map: &FieldMapConfig,
) -> Vec<(String, HashMap<String, toml::Value>)> {
    let entries = match doc.entries(&field_map.entries_path) {
        Some(e) => e,
        None => return vec![],
    };
    entries
        .iter()
        .filter_map(|(key, val)| {
            let mut fields = HashMap::new();
            for (vcc_name, spec) in &field_map.fields {
                let search_root: &DocValue = if spec.scope == "document" {
                    doc.root()
                } else {
                    val
                };
                let found = if !spec.path.is_empty() {
                    search_root.get_path(&spec.path)
                } else {
                    None
                }
                .or_else(|| spec.fallback.iter().find_map(|p| search_root.get_path(p)));
                if let Some(rv) = found {
                    let tv = match &spec.read_strategy {
                        Some(s) => match apply_read_strategy(s, rv, spec, key) {
                            Some(v) => v,
                            None => continue,
                        },
                        None => doc_value_to_toml(rv),
                    };
                    fields.insert(vcc_name.clone(), tv);
                }
            }
            if fields.is_empty() {
                None
            } else {
                Some((key.clone(), fields))
            }
        })
        .collect()
}

pub(crate) fn inspect_entries(
    doc: &DocTree,
    field_map: &FieldMapConfig,
) -> Vec<crate::adapter::InspectItem> {
    let entries = match doc.entries(&field_map.entries_path) {
        Some(e) => e,
        None => return vec![],
    };
    entries
        .iter()
        .map(|(name, val)| {
            let tf = if field_map.type_fields.is_empty() {
                &["provider".to_string(), "type".to_string()] as &[String]
            } else {
                field_map.type_fields.as_slice()
            };
            let p_type = tf
                .iter()
                .find_map(|f| val.get_path_str(f))
                .unwrap_or("unknown");
            let detail = field_map
                .model_field
                .as_ref()
                .and_then(|mf| val.get_path_str(mf))
                .map(|m| format!("type: {}, model: {}", p_type, m))
                .unwrap_or_else(|| format!("type: {}", p_type));
            let enabled = field_map
                .enabled_field
                .as_ref()
                .map(|ef| val.get_path(ef).and_then(|v| v.as_bool()).unwrap_or(true))
                .unwrap_or(true);
            crate::adapter::InspectItem {
                name: name.clone(),
                enabled,
                detail,
            }
        })
        .collect()
}

pub(crate) fn doc_value_to_toml(val: &DocValue) -> toml::Value {
    val.clone().into()
}
pub(crate) fn toml_to_doc_value(val: &toml::Value) -> DocValue {
    val.clone().into()
}
pub(crate) fn str_field(f: &HashMap<String, toml::Value>, k: &str) -> String {
    f.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
}
pub(crate) fn opt_str_field(f: &HashMap<String, toml::Value>, k: &str) -> Option<String> {
    f.get(k)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}
pub(crate) fn vec_field(f: &HashMap<String, toml::Value>, k: &str) -> Vec<String> {
    f.get(k)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}
pub(crate) fn map_field(
    f: &HashMap<String, toml::Value>,
    k: &str,
) -> std::collections::HashMap<String, String> {
    f.get(k)
        .and_then(|v| v.as_table())
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── matches_at_suffix ──

    #[test]
    fn test_matches_at_suffix_exact() {
        assert!(matches_at_suffix("myplugin", "myplugin"));
    }

    #[test]
    fn test_matches_at_suffix_with_at() {
        assert!(matches_at_suffix("myplugin@latest", "myplugin"));
        assert!(matches_at_suffix("myplugin@marketplace", "myplugin"));
    }

    #[test]
    fn test_matches_at_suffix_no_match() {
        assert!(!matches_at_suffix("other", "myplugin"));
        assert!(!matches_at_suffix("myplugin-extra", "myplugin"));
    }

    // ── DocTree: entries on array vs object ──

    fn make_doc_with_object() -> DocTree {
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

    fn make_doc_with_array() -> DocTree {
        let root = DocValue::Object(HashMap::from([(
            "plugin".into(),
            DocValue::Array(vec![
                DocValue::String("oh-my-openagent@latest".into()),
                DocValue::String("opencode-gemini-auth@latest".into()),
            ]),
        )]));
        DocTree::new_test(DocFormat::Json, root)
    }

    #[test]
    fn test_entries_on_object_returns_some() {
        let doc = make_doc_with_object();
        let entries = doc.entries("mcpServers");
        assert!(entries.is_some());
        let e = entries.unwrap();
        assert_eq!(e.len(), 2);
    }

    #[test]
    fn test_entries_on_array_returns_none() {
        let doc = make_doc_with_array();
        // Key bug: entries() only works on objects, returns None for arrays
        assert!(doc.entries("plugin").is_none());
    }

    // ── DocTree: find_matching_key ──

    #[test]
    fn test_find_matching_key_exact() {
        let doc = make_doc_with_object();
        assert_eq!(doc.find_matching_key("mcpServers", "fs"), Some("fs".into()));
        assert_eq!(
            doc.find_matching_key("mcpServers", "icm"),
            Some("icm".into())
        );
    }

    #[test]
    fn test_find_matching_key_not_found() {
        let doc = make_doc_with_object();
        assert_eq!(doc.find_matching_key("mcpServers", "nonexistent"), None);
    }

    #[test]
    fn test_find_matching_key_with_suffix() {
        let root = DocValue::Object(HashMap::from([(
            "mcpServers".into(),
            DocValue::Object(HashMap::from([(
                "fs@marketplace".into(),
                DocValue::Object(HashMap::new()),
            )])),
        )]));
        let doc = DocTree::new_test(DocFormat::Toml, root);
        assert_eq!(
            doc.find_matching_key("mcpServers", "fs"),
            Some("fs@marketplace".into())
        );
    }

    // ── DocTree: push and retain_in_array ──

    #[test]
    fn test_push_to_array() {
        let mut doc = make_doc_with_array();
        doc.push("plugin", DocValue::String("new-plugin@latest".into()));
        let arr = doc.get("plugin").and_then(|v| v.as_array()).unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[2].as_str(), Some("new-plugin@latest"));
    }

    #[test]
    fn test_retain_in_array() {
        let mut doc = make_doc_with_array();
        doc.retain_in_array("plugin", |item| {
            item.as_str()
                .map_or(true, |s| !s.starts_with("oh-my-openagent"))
        });
        let arr = doc.get("plugin").and_then(|v| v.as_array()).unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str(), Some("opencode-gemini-auth@latest"));
    }

    // ── DocTree: set_entry_enabled ──

    #[test]
    fn test_set_entry_enabled_toml() {
        let mut doc = make_doc_with_object();
        doc.set_entry_enabled("mcpServers", "icm", true);
        let icm = doc.get("mcpServers.icm").unwrap();
        // enabled is a Bool, not a String — use as_bool, not get_path_str
        assert_eq!(
            icm.get_path("enabled").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_set_entry_enabled_json() {
        let root = DocValue::Object(HashMap::from([(
            "mcpServers".into(),
            DocValue::Object(HashMap::from([(
                "fs".into(),
                DocValue::Object(HashMap::new()),
            )])),
        )]));
        let mut doc = DocTree::new_test(DocFormat::Json, root);
        doc.set_entry_enabled("mcpServers", "fs", true);
        // JSON format converts entry to bool value
        let fs_val = doc.get("mcpServers.fs").unwrap();
        assert_eq!(fs_val.as_bool(), Some(true));
    }

    // ── DocTree: other operations ──

    #[test]
    fn test_ensure_object() {
        let mut doc = DocTree::new_test(DocFormat::Json, DocValue::Object(HashMap::new()));
        doc.ensure_object("mcpServers");
        assert!(doc.get("mcpServers").unwrap().as_object().is_some());
    }

    #[test]
    fn test_remove_from_object() {
        let mut doc = make_doc_with_object();
        assert!(doc.remove_from_object("mcpServers", "fs"));
        assert!(!doc.remove_from_object("mcpServers", "fs")); // already removed
        assert_eq!(doc.find_matching_key("mcpServers", "fs"), None);
    }

    #[test]
    fn test_extract_array_name_map() {
        let doc = make_doc_with_array();
        let map = doc.extract_array_name_map("plugin");
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("oh-my-openagent@latest"));
        assert!(map.contains_key("opencode-gemini-auth@latest"));
    }

    #[test]
    fn test_get_path() {
        let doc = make_doc_with_object();
        assert_eq!(doc.get_str("mcpServers.fs.command"), Some("npx"));
        assert!(doc
            .get("mcpServers.icm.disabled")
            .unwrap()
            .as_bool()
            .unwrap());
    }

    #[test]
    fn test_doc_value_to_toml_roundtrip() {
        let original = toml::Value::from({
            let mut m = toml::map::Map::new();
            m.insert("name".into(), toml::Value::String("test".into()));
            m.insert("count".into(), toml::Value::Integer(42));
            m.insert("active".into(), toml::Value::Boolean(true));
            m
        });
        let doc_val: DocValue = original.clone().into();
        let back: toml::Value = doc_value_to_toml(&doc_val);
        assert_eq!(original, back);
    }

    // ── DocValue accessors ──

    #[test]
    fn test_doc_value_accessors() {
        assert!(DocValue::Null.is_null());
        assert_eq!(DocValue::Bool(true).as_bool(), Some(true));
        assert_eq!(DocValue::Integer(42).as_i64(), Some(42));
        assert_eq!(DocValue::Float(2.5).as_f64(), Some(2.5));
        assert_eq!(DocValue::String("hi".into()).as_str(), Some("hi"));
        assert!(DocValue::Array(vec![]).as_array().is_some());
        assert!(DocValue::Object(HashMap::new()).as_object().is_some());
    }

    #[test]
    fn test_doc_value_wrong_accessor() {
        assert!(DocValue::Bool(true).as_str().is_none());
        assert!(DocValue::String("hi".into()).as_i64().is_none());
        assert!(DocValue::Integer(42).as_array().is_none());
        assert!(DocValue::Array(vec![]).as_object().is_none());
    }

    #[test]
    fn test_doc_value_integer_as_f64() {
        // Integer can be read as f64
        assert_eq!(DocValue::Integer(42).as_f64(), Some(42.0));
    }

    // ── DocValue From<serde_json::Value> ──

    #[test]
    fn test_from_json_null() {
        let v: DocValue = serde_json::Value::Null.into();
        assert!(v.is_null());
    }

    #[test]
    fn test_from_json_number() {
        let v: DocValue = serde_json::json!(42).into();
        assert_eq!(v.as_i64(), Some(42));
        let v2: DocValue = serde_json::json!(2.5).into();
        assert!(v2.as_f64().is_some());
    }

    #[test]
    fn test_from_json_object() {
        let v: DocValue = serde_json::json!({"key": "val"}).into();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.get("key").unwrap().as_str(), Some("val"));
    }

    #[test]
    fn test_from_json_array() {
        let v: DocValue = serde_json::json!([1, "two", true]).into();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0].as_i64(), Some(1));
        assert_eq!(arr[1].as_str(), Some("two"));
        assert_eq!(arr[2].as_bool(), Some(true));
    }

    // ── DocValue From<toml::Value> ──

    #[test]
    fn test_from_toml_table() {
        let mut m = toml::map::Map::new();
        m.insert("name".into(), toml::Value::String("test".into()));
        let v: DocValue = toml::Value::Table(m).into();
        assert_eq!(
            v.as_object().unwrap().get("name").unwrap().as_str(),
            Some("test")
        );
    }

    #[test]
    fn test_from_toml_datetime() {
        let dt = toml::Value::Datetime(toml::value::Datetime {
            date: None,
            time: None,
            offset: None,
        });
        let v: DocValue = dt.into();
        // Datetime converts to String
        assert!(v.as_str().is_some());
    }

    // ── DocValue → serde_json::Value roundtrip ──

    #[test]
    fn test_doc_value_to_json_roundtrip() {
        let original = serde_json::json!({
            "name": "test",
            "count": 42,
            "active": true,
            "items": [1, 2, 3]
        });
        let doc_val: DocValue = original.clone().into();
        let back: serde_json::Value = doc_val.into();
        assert_eq!(original, back);
    }

    // ── DocValue Null → toml ──

    #[test]
    fn test_doc_value_null_to_toml() {
        let v: toml::Value = DocValue::Null.into();
        assert_eq!(v.as_str(), Some("null")); // Null becomes "null" string in toml
    }

    // ── set_path and remove_path ──

    #[test]
    fn test_set_path_simple() {
        let mut root = DocValue::Object(HashMap::new());
        root.set_path("key", DocValue::String("value".into()));
        assert_eq!(root.get_path_str("key"), Some("value"));
    }

    #[test]
    fn test_set_path_nested() {
        let mut root = DocValue::Object(HashMap::new());
        root.set_path("a.b.c", DocValue::Integer(42));
        assert_eq!(root.get_path("a.b.c").and_then(|v| v.as_i64()), Some(42));
    }

    #[test]
    fn test_set_path_overwrite() {
        let mut root = DocValue::Object(HashMap::new());
        root.set_path("key", DocValue::String("old".into()));
        root.set_path("key", DocValue::String("new".into()));
        assert_eq!(root.get_path_str("key"), Some("new"));
    }

    #[test]
    fn test_remove_path_existing() {
        let mut root = DocValue::Object(HashMap::from([(
            "key".into(),
            DocValue::String("val".into()),
        )]));
        assert!(root.remove_path("key"));
        assert!(root.get_path("key").is_none());
    }

    #[test]
    fn test_remove_path_nonexistent() {
        let root = DocValue::Object(HashMap::new());
        let mut root = root;
        assert!(!root.remove_path("missing"));
    }

    #[test]
    fn test_remove_path_nested() {
        let mut root = DocValue::Object(HashMap::new());
        root.set_path("a.b", DocValue::String("val".into()));
        assert!(root.remove_path("a.b"));
        assert!(root.get_path("a.b").is_none());
        // Parent object still exists
        assert!(root.get_path("a").is_some());
    }

    #[test]
    fn test_path_exists() {
        let mut root = DocValue::Object(HashMap::new());
        root.set_path("x.y", DocValue::Bool(true));
        assert!(root.path_exists("x"));
        assert!(root.path_exists("x.y"));
        assert!(!root.path_exists("x.z"));
        assert!(!root.path_exists("missing"));
    }

    // ── DocTree: set_entry, set_entry_bool, get_entry_mut ──

    #[test]
    fn test_set_entry() {
        let mut doc = DocTree::new_test(DocFormat::Json, DocValue::Object(HashMap::new()));
        doc.ensure_object("mcpServers");
        doc.set_entry("mcpServers", "fs", DocValue::String("npx".into()));
        assert_eq!(doc.get_str("mcpServers.fs"), Some("npx"));
    }

    #[test]
    fn test_set_entry_bool_json() {
        let mut doc = DocTree::new_test(DocFormat::Json, DocValue::Object(HashMap::new()));
        doc.ensure_object("plugins");
        doc.set_entry_bool("plugins", "myplugin", true);
        let val = doc.get("plugins.myplugin").unwrap();
        assert_eq!(val.as_bool(), Some(true));

        doc.set_entry_bool("plugins", "myplugin", false);
        let val = doc.get("plugins.myplugin").unwrap();
        assert_eq!(val.as_bool(), Some(false));
    }

    #[test]
    fn test_set_entry_bool_toml() {
        let mut doc = DocTree::new_test(DocFormat::Toml, DocValue::Object(HashMap::new()));
        doc.ensure_object("plugins");
        doc.set_entry_bool("plugins", "myplugin", true);
        let val = doc.get("plugins.myplugin").unwrap();
        // TOML format wraps in { enabled: bool }
        assert_eq!(
            val.get_path("enabled").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_get_entry_mut_existing() {
        let mut doc = make_doc_with_object();
        if let Some(entry) = doc.get_entry_mut("mcpServers", "fs") {
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("new_field".into(), DocValue::String("hello".into()));
            }
        }
        assert_eq!(doc.get_str("mcpServers.fs.new_field"), Some("hello"));
    }

    #[test]
    fn test_get_entry_mut_nonexistent() {
        let doc = make_doc_with_object();
        let mut doc = doc;
        assert!(doc.get_entry_mut("mcpServers", "nonexistent").is_none());
    }

    // ── DocTree: remove_matching_from_object ──

    #[test]
    fn test_remove_matching_from_object_exact() {
        let mut doc = make_doc_with_object();
        let count = doc.remove_matching_from_object("mcpServers", "fs");
        assert_eq!(count, 1);
        assert!(doc.get("mcpServers.fs").is_none());
    }

    #[test]
    fn test_remove_matching_from_object_with_suffix() {
        let root = DocValue::Object(HashMap::from([(
            "mcpServers".into(),
            DocValue::Object(HashMap::from([
                ("fs@marketplace".into(), DocValue::Object(HashMap::new())),
                ("other".into(), DocValue::Object(HashMap::new())),
            ])),
        )]));
        let mut doc = DocTree::new_test(DocFormat::Toml, root);
        let count = doc.remove_matching_from_object("mcpServers", "fs");
        assert_eq!(count, 1);
        assert!(doc.get("mcpServers.fs@marketplace").is_none());
        assert!(doc.get("mcpServers.other").is_some());
    }

    #[test]
    fn test_remove_matching_from_object_nonexistent() {
        let mut doc = make_doc_with_object();
        let count = doc.remove_matching_from_object("mcpServers", "nonexistent");
        assert_eq!(count, 0);
    }

    // ── DocTree: clear_section, is_empty ──

    #[test]
    fn test_clear_section() {
        let mut doc = make_doc_with_object();
        doc.clear_section("mcpServers");
        // Section should exist but be empty
        let section = doc.get("mcpServers").unwrap();
        assert!(section.as_object().unwrap().is_empty());
    }

    #[test]
    fn test_is_empty_on_new() {
        let doc = DocTree::new_test(DocFormat::Json, DocValue::Object(HashMap::new()));
        assert!(doc.is_empty());
    }

    #[test]
    fn test_is_empty_with_data() {
        let doc = make_doc_with_object();
        assert!(!doc.is_empty());
    }

    // ── DocFormat ──

    #[test]
    fn test_doc_format_from_format_str() {
        assert_eq!(DocFormat::from_format_str("toml"), DocFormat::Toml);
        assert_eq!(DocFormat::from_format_str("toml_hooks"), DocFormat::Toml);
        assert_eq!(DocFormat::from_format_str("toml_table"), DocFormat::Toml);
        assert_eq!(DocFormat::from_format_str("yaml"), DocFormat::Yaml);
        assert_eq!(DocFormat::from_format_str("yaml_flat"), DocFormat::Yaml);
        assert_eq!(DocFormat::from_format_str("env"), DocFormat::Env);
        assert_eq!(DocFormat::from_format_str("env_file"), DocFormat::Env);
        assert_eq!(DocFormat::from_format_str("json"), DocFormat::Json);
        assert_eq!(DocFormat::from_format_str("anything_else"), DocFormat::Json);
    }

    #[test]
    fn test_doc_format_from_filename() {
        assert_eq!(DocFormat::from_filename("settings.json"), DocFormat::Json);
        assert_eq!(DocFormat::from_filename("opencode.json"), DocFormat::Json);
        assert_eq!(DocFormat::from_filename("config.toml"), DocFormat::Toml);
        assert_eq!(DocFormat::from_filename(".aider.conf.yml"), DocFormat::Yaml);
        assert_eq!(DocFormat::from_filename("settings.yaml"), DocFormat::Yaml);
        assert_eq!(DocFormat::from_filename("noext"), DocFormat::Json); // no extension → default
    }

    #[test]
    fn test_doc_format_from_provider_format() {
        assert_eq!(
            DocFormat::from_provider_format("toml_provider_table"),
            DocFormat::Toml
        );
        assert_eq!(
            DocFormat::from_provider_format("yaml_flat"),
            DocFormat::Yaml
        );
        assert_eq!(DocFormat::from_provider_format("json"), DocFormat::Json);
        assert_eq!(DocFormat::from_provider_format("other"), DocFormat::Json);
    }

    #[test]
    fn test_doc_format_default_filename() {
        assert_eq!(DocFormat::Json.default_filename(), "settings.json");
        assert_eq!(DocFormat::Toml.default_filename(), "config.toml");
        assert_eq!(DocFormat::Yaml.default_filename(), ".aider.conf.yml");
        assert_eq!(DocFormat::Env.default_filename(), ".env");
    }

    // ── extract_array_name_map edge cases ──

    #[test]
    fn test_extract_array_name_map_tuple_entries() {
        let arr = DocValue::Array(vec![
            DocValue::Array(vec![DocValue::String("plugin-a".into())]),
            DocValue::String("plugin-b".into()),
        ]);
        let map = arr.extract_array_name_map();
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("plugin-a"));
        assert!(map.contains_key("plugin-b"));
    }

    #[test]
    fn test_extract_array_name_map_empty() {
        let arr = DocValue::Array(vec![]);
        let map = arr.extract_array_name_map();
        assert!(map.is_empty());
    }

    #[test]
    fn test_extract_array_name_map_on_object() {
        let obj = DocValue::Object(HashMap::new());
        let map = obj.extract_array_name_map();
        assert!(map.is_empty());
    }

    // ── Field helpers ──

    #[test]
    fn test_str_field() {
        let mut f = HashMap::new();
        f.insert("name".into(), toml::Value::String("test".into()));
        assert_eq!(str_field(&f, "name"), "test");
        assert_eq!(str_field(&f, "missing"), "");
    }

    #[test]
    fn test_opt_str_field() {
        let mut f = HashMap::new();
        f.insert("name".into(), toml::Value::String("test".into()));
        f.insert("empty".into(), toml::Value::String("".into()));
        assert_eq!(opt_str_field(&f, "name"), Some("test".into()));
        assert_eq!(opt_str_field(&f, "empty"), None);
        assert_eq!(opt_str_field(&f, "missing"), None);
    }

    #[test]
    fn test_vec_field() {
        let mut f = HashMap::new();
        f.insert(
            "items".into(),
            toml::Value::Array(vec![
                toml::Value::String("a".into()),
                toml::Value::String("b".into()),
            ]),
        );
        assert_eq!(vec_field(&f, "items"), vec!["a", "b"]);
        assert!(vec_field(&f, "missing").is_empty());
    }

    #[test]
    fn test_map_field() {
        let mut inner = toml::map::Map::new();
        inner.insert("k1".into(), toml::Value::String("v1".into()));
        let mut f = HashMap::new();
        f.insert("env".into(), toml::Value::Table(inner));
        let m = map_field(&f, "env");
        assert_eq!(m.get("k1").unwrap(), "v1");
        assert!(map_field(&f, "missing").is_empty());
    }

    // ── set_entry_enabled: no matching key creates new entry ──

    #[test]
    fn test_set_entry_enabled_creates_new_toml() {
        let mut doc = DocTree::new_test(DocFormat::Toml, DocValue::Object(HashMap::new()));
        doc.ensure_object("plugins");
        doc.set_entry_enabled("plugins", "newplugin", true);
        // Should create an entry like "newplugin@marketplace: { enabled: true }"
        let entries = doc.entries("plugins").unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].0.starts_with("newplugin"));
    }

    // ── yaml_escape_value ──

    #[test]
    fn test_yaml_escape_value_colon() {
        assert_eq!(yaml_escape_value("key: value"), "\"key: value\"");
    }

    #[test]
    fn test_yaml_escape_value_hash() {
        assert_eq!(yaml_escape_value("#comment"), "\"#comment\"");
    }

    #[test]
    fn test_yaml_escape_value_braces() {
        assert_eq!(yaml_escape_value("{obj}"), "\"{obj}\"");
    }

    #[test]
    fn test_yaml_escape_value_boolean() {
        assert_eq!(yaml_escape_value("true"), "\"true\"");
        assert_eq!(yaml_escape_value("false"), "\"false\"");
    }

    #[test]
    fn test_yaml_escape_value_null() {
        assert_eq!(yaml_escape_value("null"), "\"null\"");
    }

    #[test]
    fn test_yaml_escape_value_number() {
        assert_eq!(yaml_escape_value("42"), "\"42\"");
        assert_eq!(yaml_escape_value("3.14"), "\"3.14\"");
    }

    #[test]
    fn test_yaml_escape_value_plain_string() {
        assert_eq!(yaml_escape_value("hello"), "hello");
        assert_eq!(yaml_escape_value("simple-word"), "simple-word");
    }

    #[test]
    fn test_yaml_escape_value_leading_space() {
        assert_eq!(yaml_escape_value(" leading"), "\" leading\"");
    }

    #[test]
    fn test_yaml_escape_value_quote_in_value() {
        // "hi" contains quote, which doesn't trigger quoting on its own
        // but if the value contains a colon, it gets quoted and backslash-escaped
        let result = yaml_escape_value("say: \"hi\"");
        assert_eq!(result, "\"say: \\\"hi\\\"\"");
    }

    #[test]
    fn test_yaml_escape_value_backslash_in_quoted() {
        // Backslash is only escaped when the value is quoted (e.g. contains colon)
        let result = yaml_escape_value("path\\to:file");
        assert_eq!(result, "\"path\\\\to:file\"");
    }

    #[test]
    fn test_yaml_escape_value_backslash_unquoted() {
        // No quoting trigger → no escaping
        let result = yaml_escape_value("path\\to");
        assert_eq!(result, "path\\to");
    }

    // ── apply_read_strategy ──

    #[test]
    fn test_apply_read_strategy_object_keys() {
        let mut map = std::collections::HashMap::new();
        map.insert("key1".to_string(), DocValue::String("v1".into()));
        map.insert("key2".to_string(), DocValue::String("v2".into()));
        let obj = DocValue::Object(map);
        let spec = FieldSpec {
            path: String::new(),
            fallback: vec![],
            write_to: vec![],
            read_strategy: None,
            write_strategy: None,
            scope: "entry".to_string(),
        };
        let result = apply_read_strategy("object_keys", &obj, &spec, "test");
        let binding = result.unwrap();
        let arr = binding.as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn test_apply_read_strategy_object_keys_non_object() {
        let val = DocValue::String("hello".into());
        let spec = FieldSpec {
            path: String::new(),
            fallback: vec![],
            write_to: vec![],
            read_strategy: None,
            write_strategy: None,
            scope: "entry".to_string(),
        };
        assert!(apply_read_strategy("object_keys", &val, &spec, "test").is_none());
    }

    #[test]
    fn test_apply_read_strategy_split_slash_last() {
        let val = DocValue::String("owner/repo".into());
        let spec = FieldSpec {
            path: String::new(),
            fallback: vec![],
            write_to: vec![],
            read_strategy: None,
            write_strategy: None,
            scope: "entry".to_string(),
        };
        let result = apply_read_strategy("split_slash_last", &val, &spec, "test");
        assert_eq!(result.unwrap().as_str().unwrap(), "repo");
    }

    #[test]
    fn test_apply_read_strategy_split_slash_no_slash() {
        let val = DocValue::String("noslash".into());
        let spec = FieldSpec {
            path: String::new(),
            fallback: vec![],
            write_to: vec![],
            read_strategy: None,
            write_strategy: None,
            scope: "entry".to_string(),
        };
        let result = apply_read_strategy("split_slash_last", &val, &spec, "test");
        assert_eq!(result.unwrap().as_str().unwrap(), "noslash");
    }

    #[test]
    fn test_apply_read_strategy_env_resolve() {
        let val = DocValue::String("sk-123".into());
        let spec = FieldSpec {
            path: String::new(),
            fallback: vec![],
            write_to: vec![],
            read_strategy: None,
            write_strategy: None,
            scope: "entry".to_string(),
        };
        let result = apply_read_strategy("env_resolve", &val, &spec, "test");
        assert_eq!(result.unwrap().as_str().unwrap(), "sk-123");
    }

    #[test]
    fn test_apply_read_strategy_env_resolve_empty() {
        let val = DocValue::String("".into());
        let spec = FieldSpec {
            path: String::new(),
            fallback: vec![],
            write_to: vec![],
            read_strategy: None,
            write_strategy: None,
            scope: "entry".to_string(),
        };
        assert!(apply_read_strategy("env_resolve", &val, &spec, "test").is_none());
    }

    #[test]
    fn test_apply_read_strategy_unknown() {
        let val = DocValue::String("hello".into());
        let spec = FieldSpec {
            path: String::new(),
            fallback: vec![],
            write_to: vec![],
            read_strategy: None,
            write_strategy: None,
            scope: "entry".to_string(),
        };
        assert!(apply_read_strategy("unknown_strategy", &val, &spec, "test").is_none());
    }

    // ── toml_to_doc_value ──

    #[test]
    fn test_toml_to_doc_value_string() {
        let v = toml::Value::String("hello".into());
        assert_eq!(toml_to_doc_value(&v), DocValue::String("hello".into()));
    }

    #[test]
    fn test_toml_to_doc_value_integer() {
        let v = toml::Value::Integer(42);
        assert_eq!(toml_to_doc_value(&v), DocValue::Integer(42));
    }

    #[test]
    fn test_toml_to_doc_value_boolean() {
        let v = toml::Value::Boolean(true);
        assert_eq!(toml_to_doc_value(&v), DocValue::Bool(true));
    }

    #[test]
    fn test_toml_to_doc_value_table() {
        let mut table = toml::map::Map::new();
        table.insert("key".into(), toml::Value::String("val".into()));
        let v = toml::Value::Table(table);
        let result = toml_to_doc_value(&v);
        assert!(result.as_object().is_some());
        assert_eq!(result.get_path_str("key").unwrap(), "val");
    }

    #[test]
    fn test_toml_to_doc_value_array() {
        let v = toml::Value::Array(vec![toml::Value::Integer(1), toml::Value::Integer(2)]);
        let result = toml_to_doc_value(&v);
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    // ── sync_entries ──

    #[test]
    fn test_sync_entries_basic() {
        use crate::adapter::doc_engine::{FieldMapConfig, FieldSpec};
        let root = DocValue::Object(HashMap::from([(
            "providers".into(),
            DocValue::Object(HashMap::from([(
                "openai".into(),
                DocValue::Object(HashMap::from([
                    ("type".into(), DocValue::String("openai".into())),
                    ("api_key".into(), DocValue::String("sk-123".into())),
                ])),
            )])),
        )]));
        let doc = DocTree::new_test(DocFormat::Json, root);
        let fm = FieldMapConfig {
            entries_path: "providers".into(),
            type_fields: vec!["type".into()],
            model_field: None,
            enabled_field: None,
            inject_on_write: std::collections::HashMap::new(),
            fields: HashMap::from([(
                "type".into(),
                FieldSpec {
                    path: "type".into(),
                    fallback: vec![],
                    write_to: vec![],
                    read_strategy: None,
                    write_strategy: None,
                    scope: "entry".into(),
                },
            )]),
        };
        let entries = sync_entries(&doc, &fm);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "openai");
    }

    #[test]
    fn test_sync_entries_empty_path() {
        use crate::adapter::doc_engine::FieldMapConfig;
        let doc = DocTree::new_test(DocFormat::Json, DocValue::Object(HashMap::new()));
        let fm = FieldMapConfig {
            entries_path: "nonexistent".into(),
            type_fields: vec!["type".into()],
            model_field: None,
            enabled_field: None,
            inject_on_write: std::collections::HashMap::new(),
            fields: std::collections::HashMap::new(),
        };
        let entries = sync_entries(&doc, &fm);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_sync_entries_with_fields() {
        use crate::adapter::doc_engine::{FieldMapConfig, FieldSpec};
        let root = DocValue::Object(HashMap::from([(
            "providers".into(),
            DocValue::Object(HashMap::from([(
                "openai".into(),
                DocValue::Object(HashMap::from([
                    ("type".into(), DocValue::String("openai".into())),
                    ("api_key".into(), DocValue::String("sk-abc".into())),
                ])),
            )])),
        )]));
        let doc = DocTree::new_test(DocFormat::Json, root);
        let fm = FieldMapConfig {
            entries_path: "providers".into(),
            type_fields: vec!["type".into()],
            model_field: None,
            enabled_field: None,
            inject_on_write: HashMap::new(),
            fields: HashMap::from([(
                "api_key".into(),
                FieldSpec {
                    path: "api_key".into(),
                    fallback: vec![],
                    write_to: vec![],
                    read_strategy: None,
                    write_strategy: None,
                    scope: "entry".into(),
                },
            )]),
        };
        let entries = sync_entries(&doc, &fm);
        assert_eq!(entries.len(), 1);
        let (_, fields) = &entries[0];
        assert!(fields.contains_key("api_key"));
    }

    #[test]
    fn test_sync_entries_fallback_path() {
        use crate::adapter::doc_engine::{FieldMapConfig, FieldSpec};
        let root = DocValue::Object(HashMap::from([(
            "providers".into(),
            DocValue::Object(HashMap::from([(
                "openai".into(),
                DocValue::Object(HashMap::from([(
                    "apiKey".into(),
                    DocValue::String("sk-fallback".into()),
                )])),
            )])),
        )]));
        let doc = DocTree::new_test(DocFormat::Json, root);
        let fm = FieldMapConfig {
            entries_path: "providers".into(),
            type_fields: vec!["type".into()],
            model_field: None,
            enabled_field: None,
            inject_on_write: HashMap::new(),
            fields: HashMap::from([(
                "api_key".into(),
                FieldSpec {
                    path: "api_key".into(),
                    fallback: vec!["apiKey".into()],
                    write_to: vec![],
                    read_strategy: None,
                    write_strategy: None,
                    scope: "entry".into(),
                },
            )]),
        };
        let entries = sync_entries(&doc, &fm);
        assert_eq!(entries.len(), 1);
        let (_, fields) = &entries[0];
        assert!(fields.contains_key("api_key"));
    }

    // ── inspect_entries ──

    #[test]
    fn test_inspect_entries_basic() {
        use crate::adapter::doc_engine::FieldMapConfig;
        let root = DocValue::Object(HashMap::from([(
            "providers".into(),
            DocValue::Object(HashMap::from([
                (
                    "openai".into(),
                    DocValue::Object(HashMap::from([(
                        "type".into(),
                        DocValue::String("openai".into()),
                    )])),
                ),
                (
                    "anthropic".into(),
                    DocValue::Object(HashMap::from([(
                        "type".into(),
                        DocValue::String("anthropic".into()),
                    )])),
                ),
            ])),
        )]));
        let doc = DocTree::new_test(DocFormat::Json, root);
        let fm = FieldMapConfig {
            entries_path: "providers".into(),
            type_fields: vec!["type".into()],
            model_field: None,
            enabled_field: None,
            inject_on_write: HashMap::new(),
            fields: HashMap::new(),
        };
        let items = inspect_entries(&doc, &fm);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn test_inspect_entries_with_model() {
        use crate::adapter::doc_engine::FieldMapConfig;
        let root = DocValue::Object(HashMap::from([(
            "providers".into(),
            DocValue::Object(HashMap::from([(
                "openai".into(),
                DocValue::Object(HashMap::from([
                    ("type".into(), DocValue::String("openai".into())),
                    ("model".into(), DocValue::String("gpt-4".into())),
                ])),
            )])),
        )]));
        let doc = DocTree::new_test(DocFormat::Json, root);
        let fm = FieldMapConfig {
            entries_path: "providers".into(),
            type_fields: vec!["type".into()],
            model_field: Some("model".into()),
            enabled_field: None,
            inject_on_write: HashMap::new(),
            fields: HashMap::new(),
        };
        let items = inspect_entries(&doc, &fm);
        assert_eq!(items.len(), 1);
        assert!(items[0].detail.contains("gpt-4"));
    }

    #[test]
    fn test_inspect_entries_with_enabled_field() {
        use crate::adapter::doc_engine::FieldMapConfig;
        let root = DocValue::Object(HashMap::from([(
            "providers".into(),
            DocValue::Object(HashMap::from([
                (
                    "active".into(),
                    DocValue::Object(HashMap::from([
                        ("type".into(), DocValue::String("openai".into())),
                        ("enabled".into(), DocValue::Bool(true)),
                    ])),
                ),
                (
                    "inactive".into(),
                    DocValue::Object(HashMap::from([
                        ("type".into(), DocValue::String("anthropic".into())),
                        ("enabled".into(), DocValue::Bool(false)),
                    ])),
                ),
            ])),
        )]));
        let doc = DocTree::new_test(DocFormat::Json, root);
        let fm = FieldMapConfig {
            entries_path: "providers".into(),
            type_fields: vec!["type".into()],
            model_field: None,
            enabled_field: Some("enabled".into()),
            inject_on_write: HashMap::new(),
            fields: HashMap::new(),
        };
        let items = inspect_entries(&doc, &fm);
        assert_eq!(items.len(), 2);
        let active = items.iter().find(|i| i.name == "active").unwrap();
        assert!(active.enabled);
        let inactive = items.iter().find(|i| i.name == "inactive").unwrap();
        assert!(!inactive.enabled);
    }

    #[test]
    fn test_inspect_entries_empty() {
        use crate::adapter::doc_engine::FieldMapConfig;
        let doc = DocTree::new_test(DocFormat::Json, DocValue::Object(HashMap::new()));
        let fm = FieldMapConfig {
            entries_path: "providers".into(),
            type_fields: vec!["type".into()],
            model_field: None,
            enabled_field: None,
            inject_on_write: HashMap::new(),
            fields: HashMap::new(),
        };
        let items = inspect_entries(&doc, &fm);
        assert!(items.is_empty());
    }

    // ── DocTree: from_mapped_array / from_mapped_object via serde_json ──

    #[test]
    fn test_from_mapped_array_via_json() {
        let json_arr = serde_json::json!([1, 2, 3]);
        let doc: DocValue = json_arr.into();
        let arr = doc.as_array().unwrap();
        assert_eq!(arr.len(), 3);
    }

    #[test]
    fn test_from_mapped_object_via_json() {
        let json_obj = serde_json::json!({"a": 1, "b": "hello"});
        let doc: DocValue = json_obj.into();
        let o = doc.as_object().unwrap();
        assert_eq!(o.len(), 2);
        assert_eq!(o.get("a").unwrap().as_i64(), Some(1));
        assert_eq!(o.get("b").unwrap().as_str(), Some("hello"));
    }

    // ── DocValue: get_path on non-object ──

    #[test]
    fn test_get_path_on_scalar() {
        let v = DocValue::String("hello".into());
        assert!(v.get_path("anything").is_none());
    }

    #[test]
    fn test_get_path_str_on_non_string() {
        let v = DocValue::Object(HashMap::from([("num".into(), DocValue::Integer(42))]));
        assert!(v.get_path_str("num").is_none());
    }

    // ── DocTree: remove on nested path ──

    #[test]
    fn test_doc_tree_remove_existing() {
        let mut doc = make_doc_with_object();
        assert!(doc.remove("mcpServers.fs"));
        assert!(doc.get("mcpServers.fs").is_none());
    }

    #[test]
    fn test_doc_tree_remove_nonexistent() {
        let mut doc = make_doc_with_object();
        assert!(!doc.remove("mcpServers.nonexistent"));
    }

    #[test]
    fn test_doc_tree_root_and_format() {
        let doc = make_doc_with_object();
        assert!(doc.root().as_object().is_some());
        assert_eq!(doc.format(), DocFormat::Toml);
    }

    #[test]
    fn test_doc_tree_get_str_shortcut() {
        let doc = make_doc_with_object();
        assert_eq!(doc.get_str("mcpServers.fs.command"), Some("npx"));
        assert_eq!(doc.get_str("mcpServers.fs.nonexistent"), None);
    }
}
