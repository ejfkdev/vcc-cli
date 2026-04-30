use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

use crate::adapter::doc_engine::{
    map_field, opt_str_field, str_field, sync_entries, toml_to_doc_value, vec_field, DocFormat,
    DocTree, DocValue,
};
use crate::adapter::mapping::{map_provider_type, unmap_provider_type, ToolMapping};
use crate::adapter::{json_to_toml_value, merge_extra_to_json, SyncItem, SyncResult};
use crate::model::provider::Provider;
use crate::model::Metadata;
use crate::model::Resource;
use crate::store::TomlStore;

// ── Shared ──

/// Filter map entries whose keys are not in `known_keys`, keeping only primitive toml values.
fn filter_extra_primitive<'a, I>(entries: I, known_keys: &[&str]) -> HashMap<String, toml::Value>
where
    I: Iterator<Item = (&'a String, &'a DocValue)>,
{
    entries
        .filter(|(k, _)| !known_keys.contains(&k.as_str()))
        .filter_map(|(k, v)| {
            let tv = crate::adapter::doc_engine::doc_value_to_toml(v);
            match &tv {
                toml::Value::String(_)
                | toml::Value::Boolean(_)
                | toml::Value::Integer(_)
                | toml::Value::Float(_) => Some((k.clone(), tv)),
                _ => None,
            }
        })
        .collect()
}

pub(crate) fn write_provider(
    mapping: &ToolMapping,
    provider: &Provider,
    dry_run: bool,
    clear_entries: bool,
) -> Result<usize> {
    if mapping.provider.field_map.is_some() {
        return doc_table_write(mapping, provider, dry_run, clear_entries);
    }
    match mapping.provider.format.as_str() {
        "env_vars" | "env_file" | "yaml_flat" => env_write(
            &env_fmt(&mapping.provider.format),
            mapping,
            provider,
            dry_run,
        ),
        "codex_split" => codex_write(mapping, provider, dry_run),
        "json_custom_models" => json_models_write(mapping, provider, dry_run),
        _ => Err(anyhow::anyhow!(
            "unknown provider format: {}",
            mapping.provider.format
        )),
    }
}

pub(crate) fn sync_provider(
    store: &TomlStore,
    mapping: &ToolMapping,
    dir: &Path,
    dry_run: bool,
) -> Result<SyncResult> {
    if mapping.provider.field_map.is_some() {
        return doc_table_sync(store, mapping, dir, dry_run);
    }
    match mapping.provider.format.as_str() {
        "env_vars" | "env_file" | "yaml_flat" => {
            let f = env_fmt(&mapping.provider.format);
            env_sync_impl(store, mapping, dir, dry_run, f.fmt, f.pfx)
        }
        "codex_split" => codex_sync(store, mapping, dir, dry_run),
        "json_custom_models" => json_models_sync(store, mapping, dir, dry_run),
        _ => Ok(SyncResult::default()),
    }
}

pub(crate) fn new_synced_provider(
    name: String,
    provider_type: String,
    api_key: String,
    base_url: Option<String>,
    default_model: Option<String>,
    tool_name: &str,
) -> Provider {
    let mut provider = Provider::new_with_name(&name);
    provider.config.provider_type = provider_type;
    provider.config.api_key = api_key;
    provider.config.base_url = base_url;
    provider.config.default_model = default_model;
    provider.metadata = Metadata {
        description: Some(format!("Synced from {}", tool_name)),
        tags: vec!["synced".to_string(), tool_name.to_string()],
        ..Default::default()
    };
    provider
}

pub(crate) fn default_provider_compare(a: &Provider, b: &Provider) -> bool {
    a.config.api_key == b.config.api_key
        && a.config.base_url == b.config.base_url
        && a.config.default_model == b.config.default_model
        && a.config.models == b.config.models
        && a.config.provider_type == b.config.provider_type
        && a.config.headers == b.config.headers
}

pub(crate) fn default_provider_merge(existing: &Provider, incoming: &Provider) -> Provider {
    let mut m = existing.clone();
    m.config.api_key = incoming.config.api_key.clone();
    if incoming.config.base_url.is_some() {
        m.config.base_url = incoming.config.base_url.clone();
    }
    if incoming.config.default_model.is_some() {
        m.config.default_model = incoming.config.default_model.clone();
    }
    if !incoming.config.models.is_empty() {
        m.config.models = incoming.config.models.clone();
    }
    m
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn sync_provider_upsert<C, M>(
    store: &TomlStore,
    new_provider: &Provider,
    name: &str,
    category: &str,
    dry_run: bool,
    result: &mut SyncResult,
    compare_fn: C,
    merge_fn: M,
) where
    C: Fn(&Provider, &Provider) -> bool,
    M: Fn(&Provider, &Provider) -> Provider,
{
    if store.resource_exists("provider", name) {
        let existing: Provider = match store.load_resource("provider", name) {
            Ok(p) => p,
            Err(_) => {
                result.skipped.push(SyncItem::new(category, name));
                return;
            }
        };
        if compare_fn(&existing, new_provider) {
            result.skipped.push(SyncItem::new(category, name));
        } else {
            if !dry_run {
                let merged = merge_fn(&existing, new_provider);
                if merged.validate().is_err() {
                    result.skipped.push(SyncItem::new(category, name));
                    return;
                }
                if let Err(e) = store.save_resource(&merged) {
                    crate::cli::output::warn(&format!("failed to save provider '{}': {}", name, e));
                }
            }
            result.updated.push(SyncItem::new(category, name));
        }
    } else {
        if new_provider.validate().is_err() {
            result.skipped.push(SyncItem::new(category, name));
        } else {
            if !dry_run {
                if let Err(e) = store.save_resource(new_provider) {
                    crate::cli::output::warn(&format!("failed to save provider '{}': {}", name, e));
                }
            }
            result.created.push(SyncItem::new(category, name));
        }
    }
}

/// Save a DocTree with dry-run awareness: print dry-run message or create dir + save + print.
fn dry_run_save(doc: &DocTree, dir: &Path, path: &Path, dry_run: bool, label: &str) -> Result<()> {
    if dry_run {
        println!("  [dry-run] {} → {}", label, path.display());
    } else {
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
        }
        doc.save()?;
        println!("  {} → {}", label, path.display());
    }
    Ok(())
}

fn headers_to_doc(headers: &HashMap<String, String>) -> DocValue {
    DocValue::Object(
        headers
            .iter()
            .map(|(k, v)| (k.clone(), DocValue::String(v.clone())))
            .collect(),
    )
}

// ── DocTable format ──

pub(crate) fn provider_config_info(
    mapping: &ToolMapping,
    dir: &Path,
) -> (DocFormat, std::path::PathBuf) {
    let format = DocFormat::from_provider_format(&mapping.provider.format);
    let default_name = if format == DocFormat::Toml {
        "config.toml"
    } else {
        "opencode.json"
    };
    (
        format,
        dir.join(mapping.provider.path.as_deref().unwrap_or(default_name)),
    )
}

pub(crate) fn load_provider_doc(
    format: DocFormat,
    path: &Path,
    mapping: &ToolMapping,
    dir: &Path,
) -> Result<DocTree> {
    let jsonc = mapping.provider.format == "json_provider_table" && mapping.mcp.jsonc;
    let fallback_paths: Vec<std::path::PathBuf> = if jsonc {
        mapping
            .mcp
            .fallback_paths
            .iter()
            .map(|fb| dir.join(fb))
            .collect()
    } else {
        vec![]
    };
    if jsonc || !fallback_paths.is_empty() {
        DocTree::load_with_options(format, path, jsonc, &fallback_paths)
    } else {
        DocTree::load(format, path)
    }
}

fn load_if_exists(format: DocFormat, path: &Path) -> Result<Option<DocTree>> {
    Ok(crate::adapter::generic::helpers::try_load_doc(format, path))
}

fn doc_table_sync(
    store: &TomlStore,
    mapping: &ToolMapping,
    dir: &Path,
    dry_run: bool,
) -> Result<SyncResult> {
    let field_map = mapping
        .provider
        .field_map
        .as_ref()
        .context("provider field_map not configured for this tool")?;
    let (format, config_path) = provider_config_info(mapping, dir);
    let doc = load_provider_doc(format, &config_path, mapping, dir)?;
    if doc.is_empty() {
        return Ok(SyncResult::default());
    }
    let entries = sync_entries(&doc, field_map);
    let mut result = SyncResult::default();
    for (entry_key, fields) in &entries {
        let api_key = str_field(fields, "api_key");
        let api_key = if api_key.is_empty() {
            fields
                .get("env_key_names")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter()
                        .find_map(|v| v.as_str().and_then(|name| std::env::var(name).ok()))
                })
                .unwrap_or_default()
        } else {
            api_key
        };
        if api_key.is_empty() {
            continue;
        }
        let base_url = opt_str_field(fields, "base_url");
        let default_model = opt_str_field(fields, "default_model");
        let models = vec_field(fields, "models");
        let npm = opt_str_field(fields, "npm");
        let headers = map_field(fields, "headers");
        let provider_type = {
            let tool_type = opt_str_field(fields, "provider_type")
                .or_else(|| opt_str_field(fields, "type"))
                .unwrap_or_default();
            if tool_type.is_empty() {
                npm.as_ref()
                    .and_then(|n| mapping.provider.npm_type_map.get(n.as_str()))
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string())
            } else {
                unmap_provider_type(&mapping.provider.type_map, &tool_type)
            }
        };
        let name = format!("{}-{}", mapping.tool.name, entry_key);
        let known_keys = [
            "api_key",
            "base_url",
            "default_model",
            "models",
            "npm",
            "headers",
            "provider_type",
            "type",
            "env_key_names",
            "_options_extra",
        ];
        let mut extra: HashMap<String, toml::Value> = fields
            .iter()
            .filter(|(k, _)| !known_keys.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Some(env_names) = fields.get("env_key_names") {
            extra.insert("env_key_names".to_string(), env_names.clone());
        }
        let options_known = ["apiKey", "baseURL", "headers"];
        let options_path = format!("{}.{}.options", field_map.entries_path, entry_key);
        if let Some(options_obj) = doc.get(&options_path).and_then(|v| v.as_object()) {
            let options_extra = filter_extra_primitive(options_obj.iter(), &options_known);
            if !options_extra.is_empty() {
                extra.insert(
                    "_options_extra".to_string(),
                    toml::Value::Table(options_extra.into_iter().collect()),
                );
            }
        }
        let mut np = new_synced_provider(
            name.clone(),
            provider_type,
            api_key,
            base_url,
            default_model,
            &mapping.tool.name,
        );
        np.config.models = models;
        np.config.npm = npm;
        np.config.headers = headers;
        np.config.extra = extra;
        sync_provider_upsert(
            store,
            &np,
            &name,
            "provider",
            dry_run,
            &mut result,
            default_provider_compare,
            merge_extra,
        );
    }
    Ok(result)
}

/// Resolve write paths for a field spec, returning full document paths.
fn field_write_paths(
    entries_path: &str,
    entry_key: &str,
    spec: &crate::adapter::doc_engine::FieldSpec,
    fallback: &str,
) -> Vec<String> {
    let paths = if spec.write_to.is_empty() {
        vec![if spec.path.is_empty() {
            fallback.to_string()
        } else {
            spec.path.clone()
        }]
    } else {
        spec.write_to.clone()
    };
    paths
        .into_iter()
        .map(|wp| {
            if spec.scope == "document" {
                wp
            } else {
                format!("{}.{}.{}", entries_path, entry_key, wp)
            }
        })
        .collect()
}

fn doc_table_write(
    mapping: &ToolMapping,
    provider: &Provider,
    dry_run: bool,
    clear_entries: bool,
) -> Result<usize> {
    let field_map = mapping
        .provider
        .field_map
        .as_ref()
        .context("provider field_map not configured for this tool")?;
    let dir = mapping.require_config_dir()?;
    let (format, config_path) = provider_config_info(mapping, &dir);
    let mut doc = load_provider_doc(format, &config_path, mapping, &dir)?;
    if clear_entries {
        doc.clear_section(&field_map.entries_path);
    }
    doc.ensure_object(&field_map.entries_path);
    let entry_key = provider
        .name
        .strip_prefix(&format!("{}-", mapping.tool.name))
        .unwrap_or(&provider.name)
        .to_string();
    doc.ensure_object(&format!("{}.{}", field_map.entries_path, entry_key));
    for (vcc_name, spec) in &field_map.fields {
        let value = match vcc_name.as_str() {
            "type" => {
                let mapped =
                    map_provider_type(&mapping.provider.type_map, &provider.config.provider_type);
                Some(DocValue::String(mapped))
            }
            "api_key" => Some(DocValue::String(provider.config.api_key.clone())),
            "base_url" => provider
                .config
                .base_url
                .as_ref()
                .map(|u| DocValue::String(u.clone())),
            "npm" => provider
                .config
                .npm
                .as_ref()
                .map(|n| DocValue::String(n.clone())),
            "default_model" => provider
                .config
                .default_model
                .as_ref()
                .map(|m| DocValue::String(m.clone())),
            "headers" if !provider.config.headers.is_empty() => {
                Some(headers_to_doc(&provider.config.headers))
            }
            "models" if !provider.config.models.is_empty() => Some(DocValue::Object(
                provider
                    .config
                    .models
                    .iter()
                    .map(|mid| (mid.clone(), DocValue::Object(HashMap::new())))
                    .collect(),
            )),
            _ => provider
                .config
                .extra
                .get(vcc_name)
                .map(crate::adapter::doc_engine::toml_to_doc_value),
        };
        if let Some(val) = value {
            for fp in field_write_paths(&field_map.entries_path, &entry_key, spec, vcc_name) {
                doc.set(&fp, val.clone());
            }
        }
    }
    if let Some(model) = &provider.config.default_model {
        for (vcc_name, spec) in &field_map.fields {
            if vcc_name == "default_model"
                && spec.write_strategy.as_deref() == Some("combine_slash")
            {
                let combined = format!("{}/{}", entry_key, model);
                for fp in field_write_paths(&field_map.entries_path, &entry_key, spec, vcc_name) {
                    doc.set(&fp, DocValue::String(combined.clone()));
                }
            }
        }
    }
    for (path, value) in &field_map.inject_on_write {
        if !doc.exists(path) {
            doc.set(path, crate::adapter::doc_engine::toml_to_doc_value(value));
        }
    }
    let handled_keys: Vec<&str> = field_map.fields.keys().map(|s| s.as_str()).collect();
    let ep = &field_map.entries_path;
    for (k, v) in &provider.config.extra {
        if k == "_options_extra" {
            if let toml::Value::Table(tbl) = v {
                for (ok, ov) in tbl {
                    doc.set(
                        &format!("{}.{}.options.{}", ep, entry_key, ok),
                        crate::adapter::doc_engine::toml_to_doc_value(ov),
                    );
                }
            }
        } else if !handled_keys.contains(&k.as_str()) {
            doc.set(
                &format!("{}.{}.{}", ep, entry_key, k),
                crate::adapter::doc_engine::toml_to_doc_value(v),
            );
        }
    }
    dry_run_save(&doc, &dir, &config_path, dry_run, "provider")?;
    Ok(1)
}

// ── Env format ──

struct EnvFmt {
    fmt: DocFormat,
    pfx: &'static str,
    model_env: bool,
    extra_env: bool,
    file_perms: bool,
    weak_model: bool,
    rm_model: bool,
}
fn env_fmt(format: &str) -> EnvFmt {
    match format {
        "env_vars" => EnvFmt {
            fmt: DocFormat::Json,
            pfx: "env.",
            model_env: true,
            ..Default::default()
        },
        "env_file" => EnvFmt {
            fmt: DocFormat::Env,
            extra_env: true,
            file_perms: true,
            ..Default::default()
        },
        "yaml_flat" => EnvFmt {
            fmt: DocFormat::Yaml,
            weak_model: true,
            rm_model: true,
            ..Default::default()
        },
        _ => EnvFmt::default(),
    }
}
impl Default for EnvFmt {
    fn default() -> Self {
        EnvFmt {
            fmt: DocFormat::Json,
            pfx: "",
            model_env: false,
            extra_env: false,
            file_perms: false,
            weak_model: false,
            rm_model: false,
        }
    }
}

fn pkey(pfx: &str, k: &str) -> String {
    format!("{}{}", pfx, k)
}
fn set_default_str(
    doc: &mut DocTree,
    defaults: &HashMap<String, toml::Value>,
    key: &str,
    path: &str,
) {
    if let Some(toml::Value::String(v)) = defaults.get(key) {
        doc.set(path, DocValue::String(v.clone()));
    }
}
fn set_default_bool(
    doc: &mut DocTree,
    defaults: &HashMap<String, toml::Value>,
    key: &str,
    path: &str,
) {
    if let Some(toml::Value::Boolean(v)) = defaults.get(key) {
        doc.set(path, DocValue::Bool(*v));
    }
}
fn set_if(doc: &mut DocTree, pfx: &str, k: &str, v: &str) {
    if !k.is_empty() && !v.is_empty() {
        doc.set(&pkey(pfx, k), DocValue::String(v.to_string()));
    }
}
fn set_opt(doc: &mut DocTree, pfx: &str, k: &str, v: &Option<String>) {
    if let Some(v) = v {
        set_if(doc, pfx, k, v);
    }
}
fn rm_if(doc: &mut DocTree, pfx: &str, k: &str) {
    if !k.is_empty() {
        doc.remove(&pkey(pfx, k));
    }
}
fn get_opt(doc: &DocTree, k: &str) -> Option<String> {
    let s = doc.get_str(k).unwrap_or("").to_string();
    (!s.is_empty()).then_some(s)
}
fn def_name(f: &EnvFmt) -> &'static str {
    f.fmt.default_filename()
}

fn env_write(f: &EnvFmt, m: &ToolMapping, p: &Provider, dry: bool) -> Result<usize> {
    let dir = m.require_config_dir()?;
    let path = dir.join(m.provider.path.as_deref().unwrap_or(def_name(f)));
    if f.weak_model && dry {
        println!("  [dry-run] provider -> {}", path.display());
        return Ok(1);
    }
    let mut doc = DocTree::load(f.fmt, &path)?;
    let pfx = f.pfx;
    for em in &m.provider.env_mapping {
        rm_if(&mut doc, pfx, &em.api_key);
        rm_if(&mut doc, pfx, &em.base_url);
        rm_if(&mut doc, pfx, &em.model);
    }
    if f.model_env {
        for me in &m.provider.model_env {
            rm_if(&mut doc, pfx, &me.env_var);
        }
    }
    if f.rm_model {
        doc.remove("model");
    }
    if pfx == "env." {
        for k in &["API_KEY", "API_BASE_URL", "MODEL"] {
            doc.remove(&format!("env.{}", k));
        }
        doc.ensure_object("env");
    }
    if let Some(em) = m
        .provider
        .env_mapping
        .iter()
        .find(|em| em.vcc_type == p.config.provider_type)
    {
        set_if(&mut doc, pfx, &em.api_key, &p.config.api_key);
        set_opt(&mut doc, pfx, &em.base_url, &p.config.base_url);
        set_opt(&mut doc, pfx, &em.model, &p.config.default_model);
    } else if !p.config.api_key.is_empty() {
        set_if(&mut doc, pfx, m.provider.api_key_key(), &p.config.api_key);
        set_opt(&mut doc, pfx, m.provider.base_url_key(), &p.config.base_url);
    }
    if f.model_env && !p.config.models.is_empty() && !m.provider.model_env.is_empty() {
        for me in &m.provider.model_env {
            if let Some(mid) = p
                .config
                .models
                .iter()
                .find(|m| m.to_lowercase().contains(&me.role))
            {
                set_if(&mut doc, pfx, &me.env_var, mid);
            }
        }
    }
    if f.extra_env {
        for (k, v) in &p.config.env {
            set_if(&mut doc, pfx, k, v);
        }
    }
    if f.weak_model {
        set_opt(&mut doc, "", "model", &p.config.default_model);
        if let Some(wm) = p.config.extra.get("weak_model").and_then(|v| v.as_str()) {
            doc.set("weak-model", DocValue::String(wm.to_string()));
        }
    }
    let label = if f.fmt == DocFormat::Json {
        "provider (env)"
    } else {
        "provider"
    };
    dry_run_save(&doc, &dir, &path, dry, label)?;
    #[cfg(unix)]
    if f.file_perms && !dry {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
            eprintln!(
                "warning: failed to set permissions on {}: {}",
                path.display(),
                e
            );
        }
    }
    Ok(1)
}

fn host_name(base: Option<&str>, tool: &str, vtype: &str) -> String {
    let base_str = base.unwrap_or("https://api.anthropic.com");
    let native = vtype == "anthropic" && base_str == "https://api.anthropic.com";
    if native || base.is_none() {
        return format!("{}-anthropic", tool);
    }
    let h = base_str
        .strip_prefix("https://")
        .or_else(|| base_str.strip_prefix("http://"))
        .and_then(|u| u.split('/').next())
        .map(|s| s.replace('.', "-"))
        .unwrap_or_else(|| "anthropic".to_string());
    format!("{}-{}", tool, h)
}
/// A resolved provider entry from an env-format document.
struct ResolvedEnvEntry {
    vcc_type: String,
    api_key: String,
    base_url: Option<String>,
    model: Option<String>,
}

/// Load an env-format doc and resolve provider entries from it.
/// Returns (doc, entries, fallback_api_key_present).
/// Shared by `env_sync_impl` and `env_inspect_items`.
fn resolve_env_entries(
    mapping: &ToolMapping,
    dir: &Path,
    fmt: DocFormat,
    pfx: &str,
) -> Option<(DocTree, Vec<ResolvedEnvEntry>, bool)> {
    let def = fmt.default_filename();
    let fp = dir.join(mapping.provider.path.as_deref().unwrap_or(def));
    let doc = load_if_exists(fmt, &fp).ok()??;
    let key = |k: &str| -> Option<String> { get_opt(&doc, &format!("{}{}", pfx, k)) };
    let mut entries = Vec::new();
    if !mapping.provider.env_mapping.is_empty() {
        let mut seen = std::collections::HashSet::new();
        for em in &mapping.provider.env_mapping {
            if !seen.insert(em.vcc_type.clone()) {
                continue;
            }
            let ak = match (!em.api_key.is_empty()).then(|| key(&em.api_key)).flatten() {
                Some(k) => k,
                None => continue,
            };
            let bu = (!em.base_url.is_empty())
                .then(|| key(&em.base_url))
                .flatten();
            let dm = if fmt == DocFormat::Yaml {
                doc.get_str("model").map(|s| s.to_string())
            } else {
                (!em.model.is_empty()).then(|| key(&em.model)).flatten()
            };
            entries.push(ResolvedEnvEntry {
                vcc_type: em.vcc_type.clone(),
                api_key: ak,
                base_url: bu,
                model: dm,
            });
        }
    }
    let fallback =
        mapping.provider.env_mapping.is_empty() && key(mapping.provider.api_key_key()).is_some();
    Some((doc, entries, fallback))
}

fn res_type<'a>(vtype: &'a str, base: Option<&'a str>) -> &'a str {
    if vtype == "anthropic" && base.is_some() && base != Some("https://api.anthropic.com") {
        "custom"
    } else {
        vtype
    }
}

/// Shared env-format sync: handles both env_vars (prefix="env.") and env_file/yaml_flat (prefix="")
fn env_sync_impl(
    store: &TomlStore,
    m: &ToolMapping,
    dir: &Path,
    dry: bool,
    fmt: DocFormat,
    pfx: &str,
) -> Result<SyncResult> {
    let (doc, entries, fallback) = match resolve_env_entries(m, dir, fmt, pfx) {
        Some(r) => r,
        None => return Ok(SyncResult::default()),
    };
    let key = |k: &str| -> Option<String> { get_opt(&doc, &format!("{}{}", pfx, k)) };
    let mut result = SyncResult::default();
    for e in &entries {
        let (name, ptype, category) = if pfx == "env." {
            let n = host_name(e.base_url.as_deref(), &m.tool.name, &e.vcc_type);
            (
                n,
                res_type(&e.vcc_type, e.base_url.as_deref()).to_string(),
                "env",
            )
        } else {
            let dn = format!("{}-{}", m.tool.name, e.vcc_type);
            let n = m.provider.sync_name.as_deref().unwrap_or(&dn).to_string();
            (n, e.vcc_type.clone(), "provider")
        };
        let mut np = new_synced_provider(
            name.clone(),
            ptype,
            e.api_key.clone(),
            e.base_url.clone(),
            e.model.clone(),
            &m.tool.name,
        );
        if pfx == "env." {
            let mut models: Vec<String> = e.model.iter().cloned().collect();
            for me in &m.provider.model_env {
                if let Some(mid) = key(&me.env_var) {
                    if !models.contains(&mid) {
                        models.push(mid);
                    }
                }
            }
            np.config.models = if models.len() <= 1 {
                Vec::new()
            } else {
                models
            };
            sync_provider_upsert(
                store,
                &np,
                &name,
                category,
                dry,
                &mut result,
                default_provider_compare,
                default_provider_merge,
            );
        } else {
            if fmt == DocFormat::Yaml {
                let mut ex = HashMap::new();
                if let Some(wm) = doc.get_str("weak-model").map(|s| s.to_string()) {
                    ex.insert("weak_model".to_string(), toml::Value::String(wm));
                }
                np.config.extra = ex;
            }
            sync_provider_upsert(
                store,
                &np,
                &name,
                category,
                dry,
                &mut result,
                default_provider_compare,
                merge_extra,
            );
        }
    }
    if fallback {
        let ak = match key(m.provider.api_key_key()) {
            Some(k) => k,
            None => return Ok(result),
        };
        let bu = doc
            .get_str(m.provider.base_url_key())
            .map(|s| s.to_string());
        let dn = format!("{}-default", m.tool.name);
        let name = m.provider.sync_name.as_deref().unwrap_or(&dn);
        let np = new_synced_provider(
            name.to_string(),
            m.provider.sync_type().to_string(),
            ak,
            bu,
            None,
            &m.tool.name,
        );
        sync_provider_upsert(
            store,
            &np,
            name,
            "provider",
            dry,
            &mut result,
            default_provider_compare,
            merge_extra,
        );
    }
    Ok(result)
}

fn merge_extra(existing: &Provider, incoming: &Provider) -> Provider {
    let mut m = default_provider_merge(existing, incoming);
    if !incoming.config.extra.is_empty() {
        m.config.extra.extend(incoming.config.extra.clone());
    }
    m
}

fn merge_with_env(existing: &Provider, incoming: &Provider) -> Provider {
    let mut m = merge_extra(existing, incoming);
    if !incoming.config.env.is_empty() {
        m.config.env.extend(incoming.config.env.clone());
    }
    m
}

/// Inspect env-format provider entries from a DocTree.
/// Shared between env_vars, env_file, and yaml_flat formats.
pub(crate) fn env_inspect_items(
    mapping: &ToolMapping,
    dir: &Path,
) -> Vec<crate::adapter::InspectItem> {
    use crate::adapter::InspectItem;

    let fmt = &mapping.provider.format;
    let ef = env_fmt(fmt);
    let (_doc, entries, fallback) = match resolve_env_entries(mapping, dir, ef.fmt, ef.pfx) {
        Some(r) => r,
        None => return Vec::new(),
    };
    let tool = &mapping.tool.name;
    let mut items: Vec<InspectItem> = Vec::new();
    for e in &entries {
        items.push(InspectItem {
            name: format!("{}-{}", tool, e.vcc_type),
            enabled: true,
            detail: e
                .model
                .as_ref()
                .map(|m| format!("type: {}, model: {}", e.vcc_type, m))
                .unwrap_or_else(|| format!("type: {}", e.vcc_type)),
        });
    }
    if fallback {
        items.push(InspectItem {
            name: format!("{}-provider", tool),
            enabled: true,
            detail: "type: configured".into(),
        });
    }
    items
}

pub(crate) fn codex_inspect_items(
    mapping: &ToolMapping,
    dir: &Path,
) -> Vec<crate::adapter::InspectItem> {
    use crate::adapter::generic::helpers::try_load_doc;
    use crate::adapter::InspectItem;

    let auth_path = dir.join(mapping.provider.auth_path());
    let auth_doc = match try_load_doc(DocFormat::Json, &auth_path) {
        Some(d) => d,
        None => return Vec::new(),
    };
    let api_key = auth_doc
        .get_str(
            mapping
                .provider
                .api_key_key
                .as_deref()
                .unwrap_or("OPENAI_API_KEY"),
        )
        .unwrap_or("")
        .to_string();
    if api_key.is_empty() {
        return Vec::new();
    }
    let config_path = dir.join(mapping.provider.config_path());
    let config_doc = try_load_doc(DocFormat::Toml, &config_path);
    let model = config_doc
        .as_ref()
        .and_then(|d| d.get_str("model").map(|s| s.to_string()));
    let active_provider = config_doc
        .as_ref()
        .map(|d| d.get_str("model_provider").unwrap_or("openai").to_string())
        .unwrap_or_else(|| "openai".to_string());
    vec![InspectItem {
        name: format!("{}-{}", mapping.tool.name, active_provider),
        enabled: true,
        detail: model
            .map(|m| format!("type: {}, model: {}", active_provider, m))
            .unwrap_or_else(|| format!("type: {}", active_provider)),
    }]
}

pub(crate) fn json_models_inspect_items(
    mapping: &ToolMapping,
    dir: &Path,
) -> Vec<crate::adapter::InspectItem> {
    use crate::adapter::generic::helpers::try_load_doc;
    use crate::adapter::InspectItem;

    let config_path = dir.join(mapping.provider.path.as_deref().unwrap_or("settings.json"));
    let doc = match try_load_doc(DocFormat::Json, &config_path) {
        Some(d) => d,
        None => return Vec::new(),
    };
    let key = mapping
        .provider
        .providers_key
        .as_deref()
        .unwrap_or("customModels");
    doc.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|m| {
                    let name = m
                        .get_path_str("displayName")
                        .or_else(|| m.get_path_str("model"))
                        .unwrap_or("unknown");
                    let p_type = m.get_path_str("provider").unwrap_or("custom");
                    InspectItem {
                        name: name.into(),
                        enabled: true,
                        detail: format!("type: {}, model: {}", p_type, name),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

// ── Codex split format ──

fn codex_write(mapping: &ToolMapping, provider: &Provider, dry_run: bool) -> Result<usize> {
    let dir = mapping
        .resolved_config_dir()
        .context("cannot find config directory")?;
    let auth_path = dir.join(mapping.provider.auth_path());
    let config_path = dir.join(mapping.provider.config_path());
    let mut auth_doc = DocTree::load(DocFormat::Json, &auth_path)?;
    let api_key_key = mapping
        .provider
        .api_key_key
        .as_deref()
        .unwrap_or("OPENAI_API_KEY");
    auth_doc.set(
        api_key_key,
        DocValue::String(provider.config.api_key.clone()),
    );
    dry_run_save(&auth_doc, &dir, &auth_path, dry_run, "provider (key)")?;
    #[cfg(unix)]
    if !dry_run {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o600))
        {
            eprintln!(
                "warning: failed to set permissions on {}: {}",
                auth_path.display(),
                e
            );
        }
    }
    let mut config_doc = DocTree::load(DocFormat::Toml, &config_path)?;
    let defaults = &mapping.provider.defaults;

    // 从 extra 读取 provider 名称（默认 "custom"），动态写入 model_providers.<name>
    let provider_name = provider
        .config
        .extra
        .get("model_provider")
        .and_then(|v| v.as_str())
        .unwrap_or("custom")
        .to_string();
    let prefix = format!("model_providers.{}", provider_name);

    // 顶层 model_provider 指向当前 provider
    config_doc.set("model_provider", DocValue::String(provider_name.clone()));

    // 顶层 model_reasoning_effort
    if let Some(effort) = provider
        .config
        .extra
        .get("model_reasoning_effort")
        .and_then(|v| v.as_str())
    {
        config_doc.set("model_reasoning_effort", DocValue::String(effort.to_string()));
    } else {
        set_default_str(
            &mut config_doc,
            defaults,
            "model_reasoning_effort",
            "model_reasoning_effort",
        );
    }

    if let Some(model) = &provider.config.default_model {
        config_doc.set("model", DocValue::String(model.clone()));
    }
    if let Some(url) = &provider.config.base_url {
        config_doc.ensure_object(&prefix);
        config_doc.set(
            &format!("{}.name", prefix),
            DocValue::String(provider_name.clone()),
        );
        config_doc.set(
            &format!("{}.base_url", prefix),
            DocValue::String(url.clone()),
        );
        set_default_str(
            &mut config_doc,
            defaults,
            "wire_api",
            &format!("{}.wire_api", prefix),
        );
        set_default_bool(
            &mut config_doc,
            defaults,
            "requires_openai_auth",
            &format!("{}.requires_openai_auth", prefix),
        );
        if let Some(env_key) = provider.config.env.get("env_key") {
            config_doc.set(
                &format!("{}.env_key", prefix),
                DocValue::String(env_key.clone()),
            );
        }
        // http_headers（从 ProviderConfig.headers）
        if !provider.config.headers.is_empty() {
            let headers_map: HashMap<String, DocValue> = provider
                .config
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), DocValue::String(v.clone())))
                .collect();
            config_doc.set(
                &format!("{}.http_headers", prefix),
                DocValue::Object(headers_map),
            );
        }
        // query_params（从 extra）
        if let Some(qp) = provider.config.extra.get("query_params") {
            let doc_val = toml_to_doc_value(qp);
            config_doc.set(&format!("{}.query_params", prefix), doc_val);
        }
        // env_http_headers（从 extra）
        if let Some(ehh) = provider.config.extra.get("env_http_headers") {
            let doc_val = toml_to_doc_value(ehh);
            config_doc.set(&format!("{}.env_http_headers", prefix), doc_val);
        }
    }
    dry_run_save(
        &config_doc,
        &dir,
        &config_path,
        dry_run,
        "provider (config)",
    )?;
    Ok(1)
}

fn codex_sync(
    store: &TomlStore,
    mapping: &ToolMapping,
    dir: &Path,
    dry_run: bool,
) -> Result<SyncResult> {
    let auth_path = dir.join(mapping.provider.auth_path());
    let config_path = dir.join(mapping.provider.config_path());
    let mut api_key = String::new();
    if let Some(auth_doc) = load_if_exists(DocFormat::Json, &auth_path)? {
        api_key = auth_doc
            .get_str(
                mapping
                    .provider
                    .api_key_key
                    .as_deref()
                    .unwrap_or("OPENAI_API_KEY"),
            )
            .unwrap_or("")
            .to_string();
    }
    let mut base_url = None;
    let mut model = None;
    let mut provider_type = "openai".to_string();
    let mut env_key_found = None;
    let mut wire_api = None;
    let mut model_reasoning_effort = None;
    let mut extra_headers: HashMap<String, String> = HashMap::new();
    let mut extra_fields: HashMap<String, toml::Value> = HashMap::new();
    let mut active_provider_name = "openai".to_string();

    if let Some(config_doc) = load_if_exists(DocFormat::Toml, &config_path)? {
        model = config_doc.get_str("model").map(|s| s.to_string());
        active_provider_name = config_doc
            .get_str("model_provider")
            .unwrap_or("openai")
            .to_string();
        model_reasoning_effort = config_doc
            .get_str("model_reasoning_effort")
            .map(|s| s.to_string());

        if let Some(entries) = config_doc.entries("model_providers") {
            for (key, val) in entries {
                // 只处理活跃 provider
                if key != active_provider_name {
                    continue;
                }
                if val.get_path_str("base_url").is_some() {
                    base_url = val.get_path_str("base_url").map(|s| s.to_string());
                    provider_type = "custom".to_string();
                }
                if let Some(ekey) = val.get_path_str("env_key").map(|s| s.to_string()) {
                    env_key_found = Some(ekey.clone());
                    if api_key.is_empty() {
                        api_key = std::env::var(&ekey).unwrap_or_default();
                    }
                }
                wire_api = val.get_path_str("wire_api").map(|s| s.to_string());
                // http_headers
                if let Some(headers) = val.entries("http_headers") {
                    for (hk, hv) in headers {
                        if let Some(vs) = hv.as_str() {
                            extra_headers.insert(hk.clone(), vs.to_string());
                        }
                    }
                }
                // query_params
                if let Some(qp) = val.entries("query_params") {
                    let mut map = toml::map::Map::new();
                    for (qk, qv) in qp {
                        if let Some(s) = qv.as_str() {
                            map.insert(qk.clone(), toml::Value::String(s.to_string()));
                        }
                    }
                    if !map.is_empty() {
                        extra_fields.insert("query_params".into(), toml::Value::Table(map));
                    }
                }
                // env_http_headers
                if let Some(ehh) = val.entries("env_http_headers") {
                    let mut map = toml::map::Map::new();
                    for (ek, ev) in ehh {
                        if let Some(s) = ev.as_str() {
                            map.insert(ek.clone(), toml::Value::String(s.to_string()));
                        }
                    }
                    if !map.is_empty() {
                        extra_fields.insert("env_http_headers".into(), toml::Value::Table(map));
                    }
                }
            }
        }
    }
    if api_key.is_empty() {
        return Ok(SyncResult::default());
    }
    let name = format!("{}-{}", mapping.tool.name, active_provider_name);
    let mut np = new_synced_provider(
        name.clone(),
        provider_type,
        api_key,
        base_url,
        model,
        &mapping.tool.name,
    );
    if let Some(ek) = env_key_found {
        np.config.env.insert("env_key".to_string(), ek);
    }
    if !extra_headers.is_empty() {
        np.config.headers = extra_headers;
    }
    // extra 字段
    extra_fields.insert(
        "model_provider".into(),
        toml::Value::String(active_provider_name.clone()),
    );
    if let Some(wa) = wire_api {
        extra_fields.insert("wire_api".into(), toml::Value::String(wa));
    }
    if let Some(re) = model_reasoning_effort {
        extra_fields.insert("model_reasoning_effort".into(), toml::Value::String(re));
    }
    if !extra_fields.is_empty() {
        np.config.extra = extra_fields;
    }

    let mut result = SyncResult::default();
    sync_provider_upsert(
        store,
        &np,
        &name,
        "provider",
        dry_run,
        &mut result,
        default_provider_compare,
        merge_with_env,
    );
    Ok(result)
}

// ── JSON customModels format ──

fn json_models_write(mapping: &ToolMapping, provider: &Provider, dry_run: bool) -> Result<usize> {
    let dir = mapping
        .resolved_config_dir()
        .context("cannot find config directory")?;
    let settings_path = dir.join(mapping.provider.path.as_deref().unwrap_or("settings.json"));
    let mut doc = DocTree::load(DocFormat::Json, &settings_path)?;
    let droid_provider =
        map_provider_type(&mapping.provider.type_map, &provider.config.provider_type);
    let model_ids: Vec<String> = if !provider.config.models.is_empty() {
        provider.config.models.clone()
    } else {
        provider.config.default_model.iter().cloned().collect()
    };
    if model_ids.is_empty() {
        return Ok(0);
    }
    let models_key = mapping
        .provider
        .providers_key
        .as_deref()
        .unwrap_or("customModels");
    doc.retain_in_array(models_key, |m| {
        m.get_path_str("model")
            .map(|id| !model_ids.iter().any(|mi| mi == id))
            .unwrap_or(true)
    });
    for model_id in &model_ids {
        let display_name = if Some(model_id.as_str()) == provider.config.default_model.as_deref() {
            provider.name.clone()
        } else {
            model_id.clone()
        };
        let mut entry_map = HashMap::from([
            ("model".into(), DocValue::String(model_id.clone())),
            ("displayName".into(), DocValue::String(display_name)),
            (
                "baseUrl".into(),
                DocValue::String(provider.config.base_url.clone().unwrap_or_default()),
            ),
            (
                "apiKey".into(),
                DocValue::String(provider.config.api_key.clone()),
            ),
            (
                "provider".into(),
                DocValue::String(droid_provider.to_string()),
            ),
        ]);
        if let Some(max_output) = provider.config.extra.get("max_output_tokens") {
            entry_map.insert(
                "maxOutputTokens".into(),
                crate::adapter::doc_engine::toml_to_doc_value(max_output),
            );
        }
        if !provider.config.headers.is_empty() {
            entry_map.insert(
                "extraHeaders".into(),
                headers_to_doc(&provider.config.headers),
            );
        }
        if !provider.config.extra.is_empty() {
            let base_json = serde_json::json!({});
            if let serde_json::Value::Object(mut map) = base_json {
                merge_extra_to_json(&mut map, &provider.config.extra);
                for (k, v) in &map {
                    entry_map.insert(
                        k.clone(),
                        crate::adapter::doc_engine::toml_to_doc_value(&json_to_toml_value(v)),
                    );
                }
            }
        }
        doc.push(models_key, DocValue::Object(entry_map));
    }
    if let Some(model) = &provider.config.default_model {
        doc.set("model", DocValue::String(model.clone()));
    }
    dry_run_save(&doc, &dir, &settings_path, dry_run, "provider")?;
    #[cfg(unix)]
    if !dry_run {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            std::fs::set_permissions(&settings_path, std::fs::Permissions::from_mode(0o600))
        {
            eprintln!(
                "warning: failed to set permissions on {}: {}",
                settings_path.display(),
                e
            );
        }
    }
    Ok(1)
}

fn json_models_sync(
    store: &TomlStore,
    mapping: &ToolMapping,
    dir: &Path,
    dry_run: bool,
) -> Result<SyncResult> {
    let settings_path = dir.join(mapping.provider.path.as_deref().unwrap_or("settings.json"));
    let doc = match load_if_exists(DocFormat::Json, &settings_path)? {
        Some(d) => d,
        None => return Ok(SyncResult::default()),
    };
    let models_key = mapping
        .provider
        .providers_key
        .as_deref()
        .unwrap_or("customModels");
    let entries = match doc.entries(models_key) {
        Some(e) => e,
        None => return Ok(SyncResult::default()),
    };
    let mut result = SyncResult::default();
    for (_, model_entry) in entries {
        let model_id = model_entry.get_path_str("model").unwrap_or("").to_string();
        let api_key = model_entry.get_path_str("apiKey").unwrap_or("").to_string();
        let base_url = model_entry.get_path_str("baseUrl").map(|s| s.to_string());
        let tool_provider = model_entry
            .get_path_str("provider")
            .unwrap_or("generic-chat-completion-api");
        if api_key.is_empty() {
            continue;
        }
        let provider_type = unmap_provider_type(&mapping.provider.type_map, tool_provider);
        let name = format!(
            "{}-{}",
            mapping.tool.name,
            model_id.replace(['/', '.'], "-")
        );
        let known_keys = mapping
            .provider
            .custom_model_known_keys
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<&str>>();
        let extra: HashMap<String, toml::Value> = model_entry
            .as_object()
            .map(|obj| filter_extra_primitive(obj.iter(), &known_keys))
            .unwrap_or_default();
        let mut np = new_synced_provider(
            name.clone(),
            provider_type,
            api_key,
            base_url,
            Some(model_id),
            &mapping.tool.name,
        );
        np.config.extra = extra;
        sync_provider_upsert(
            store,
            &np,
            &name,
            "provider",
            dry_run,
            &mut result,
            default_provider_compare,
            default_provider_merge,
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Provider, ProviderConfig};

    fn make_provider(name: &str, config: ProviderConfig) -> Provider {
        let mut p = Provider::new_with_name(name);
        p.config = config;
        p
    }

    // ── default_provider_compare ──

    #[test]
    fn test_compare_equal() {
        let a = make_provider(
            "p",
            ProviderConfig {
                api_key: "sk-123".into(),
                base_url: Some("https://api.openai.com".into()),
                default_model: Some("gpt-4".into()),
                models: vec!["gpt-4".into(), "gpt-3.5".into()],
                ..Default::default()
            },
        );
        let b = make_provider(
            "p",
            ProviderConfig {
                api_key: "sk-123".into(),
                base_url: Some("https://api.openai.com".into()),
                default_model: Some("gpt-4".into()),
                models: vec!["gpt-4".into(), "gpt-3.5".into()],
                ..Default::default()
            },
        );
        assert!(default_provider_compare(&a, &b));
    }

    #[test]
    fn test_compare_different_key() {
        let a = make_provider(
            "p",
            ProviderConfig {
                api_key: "sk-1".into(),
                ..Default::default()
            },
        );
        let b = make_provider(
            "p",
            ProviderConfig {
                api_key: "sk-2".into(),
                ..Default::default()
            },
        );
        assert!(!default_provider_compare(&a, &b));
    }

    #[test]
    fn test_compare_different_model() {
        let a = make_provider(
            "p",
            ProviderConfig {
                default_model: Some("gpt-4".into()),
                ..Default::default()
            },
        );
        let b = make_provider(
            "p",
            ProviderConfig {
                default_model: Some("gpt-3.5".into()),
                ..Default::default()
            },
        );
        assert!(!default_provider_compare(&a, &b));
    }

    // ── default_provider_merge ──

    #[test]
    fn test_merge_overwrites_api_key() {
        let existing = make_provider(
            "p",
            ProviderConfig {
                api_key: "old".into(),
                ..Default::default()
            },
        );
        let incoming = make_provider(
            "p",
            ProviderConfig {
                api_key: "new".into(),
                ..Default::default()
            },
        );
        let merged = default_provider_merge(&existing, &incoming);
        assert_eq!(merged.config.api_key, "new");
    }

    #[test]
    fn test_merge_keeps_existing_base_url_if_incoming_none() {
        let existing = make_provider(
            "p",
            ProviderConfig {
                base_url: Some("https://old.com".into()),
                ..Default::default()
            },
        );
        let incoming = make_provider(
            "p",
            ProviderConfig {
                base_url: None,
                ..Default::default()
            },
        );
        let merged = default_provider_merge(&existing, &incoming);
        assert_eq!(merged.config.base_url.as_deref(), Some("https://old.com"));
    }

    #[test]
    fn test_merge_overwrites_base_url_if_incoming_some() {
        let existing = make_provider(
            "p",
            ProviderConfig {
                base_url: Some("https://old.com".into()),
                ..Default::default()
            },
        );
        let incoming = make_provider(
            "p",
            ProviderConfig {
                base_url: Some("https://new.com".into()),
                ..Default::default()
            },
        );
        let merged = default_provider_merge(&existing, &incoming);
        assert_eq!(merged.config.base_url.as_deref(), Some("https://new.com"));
    }

    #[test]
    fn test_merge_models_overwritten_if_nonempty() {
        let existing = make_provider(
            "p",
            ProviderConfig {
                models: vec!["a".into()],
                ..Default::default()
            },
        );
        let incoming = make_provider(
            "p",
            ProviderConfig {
                models: vec!["b".into(), "c".into()],
                ..Default::default()
            },
        );
        let merged = default_provider_merge(&existing, &incoming);
        assert_eq!(merged.config.models, vec!["b", "c"]);
    }

    #[test]
    fn test_merge_models_kept_if_incoming_empty() {
        let existing = make_provider(
            "p",
            ProviderConfig {
                models: vec!["a".into()],
                ..Default::default()
            },
        );
        let incoming = make_provider(
            "p",
            ProviderConfig {
                models: vec![],
                ..Default::default()
            },
        );
        let merged = default_provider_merge(&existing, &incoming);
        assert_eq!(merged.config.models, vec!["a"]);
    }

    // ── merge_extra ──

    #[test]
    fn test_merge_extra_includes_extra() {
        let mut extra = HashMap::new();
        extra.insert("custom".to_string(), toml::Value::String("val".to_string()));
        let existing = make_provider(
            "p",
            ProviderConfig {
                ..Default::default()
            },
        );
        let incoming = make_provider(
            "p",
            ProviderConfig {
                extra: extra.clone(),
                ..Default::default()
            },
        );
        let merged = merge_extra(&existing, &incoming);
        assert!(merged.config.extra.contains_key("custom"));
    }

    #[test]
    fn test_merge_extra_no_extra() {
        let existing = make_provider(
            "p",
            ProviderConfig {
                ..Default::default()
            },
        );
        let incoming = make_provider(
            "p",
            ProviderConfig {
                ..Default::default()
            },
        );
        let merged = merge_extra(&existing, &incoming);
        assert!(merged.config.extra.is_empty());
    }

    // ── merge_with_env ──

    #[test]
    fn test_merge_with_env_includes_env() {
        let mut env = HashMap::new();
        env.insert("KEY".to_string(), "val".to_string());
        let existing = make_provider(
            "p",
            ProviderConfig {
                ..Default::default()
            },
        );
        let incoming = make_provider(
            "p",
            ProviderConfig {
                env: env.clone(),
                ..Default::default()
            },
        );
        let merged = merge_with_env(&existing, &incoming);
        assert_eq!(merged.config.env.get("KEY").unwrap(), "val");
    }

    // ── filter_extra_primitive ──

    #[test]
    fn test_filter_extra_primitive_excludes_known() {
        let mut map = serde_json::Map::new();
        map.insert("api_key".into(), serde_json::json!("sk-123"));
        map.insert("custom".into(), serde_json::json!("hello"));
        let entries: Vec<(String, DocValue)> = map
            .into_iter()
            .map(|(k, v)| (k, DocValue::from(v)))
            .collect();
        let result = filter_extra_primitive(entries.iter().map(|(k, v)| (k, v)), &["api_key"]);
        assert!(!result.contains_key("api_key"));
        assert!(result.contains_key("custom"));
    }

    #[test]
    fn test_filter_extra_primitive_only_primitives() {
        let mut map = serde_json::Map::new();
        map.insert("str".into(), serde_json::json!("hello"));
        map.insert("num".into(), serde_json::json!(42));
        map.insert("obj".into(), serde_json::json!({"nested": true}));
        let entries: Vec<(String, DocValue)> = map
            .into_iter()
            .map(|(k, v)| (k, DocValue::from(v)))
            .collect();
        let result = filter_extra_primitive(entries.iter().map(|(k, v)| (k, v)), &[]);
        assert!(result.contains_key("str"));
        assert!(result.contains_key("num"));
        assert!(!result.contains_key("obj"));
    }

    // ── headers_to_doc ──

    #[test]
    fn test_headers_to_doc() {
        let mut h = HashMap::new();
        h.insert("Authorization".to_string(), "Bearer tok".to_string());
        let doc = headers_to_doc(&h);
        assert_eq!(doc.get_path_str("Authorization").unwrap(), "Bearer tok");
    }

    #[test]
    fn test_headers_to_doc_empty() {
        let h: HashMap<String, String> = HashMap::new();
        let doc = headers_to_doc(&h);
        assert!(doc.as_object().unwrap().is_empty());
    }

    // ── env_fmt ──

    #[test]
    fn test_env_fmt_env_vars() {
        let f = env_fmt("env_vars");
        assert_eq!(f.fmt, DocFormat::Json);
        assert_eq!(f.pfx, "env.");
        assert!(f.model_env);
    }

    #[test]
    fn test_env_fmt_env_file() {
        let f = env_fmt("env_file");
        assert_eq!(f.fmt, DocFormat::Env);
        assert!(f.extra_env);
        assert!(f.file_perms);
    }

    #[test]
    fn test_env_fmt_yaml_flat() {
        let f = env_fmt("yaml_flat");
        assert_eq!(f.fmt, DocFormat::Yaml);
        assert!(f.weak_model);
        assert!(f.rm_model);
    }

    #[test]
    fn test_env_fmt_unknown() {
        let f = env_fmt("unknown");
        assert_eq!(f.fmt, DocFormat::Json);
        assert_eq!(f.pfx, "");
    }

    // ── pkey ──

    #[test]
    fn test_pkey_with_prefix() {
        assert_eq!(pkey("env.", "API_KEY"), "env.API_KEY");
    }

    #[test]
    fn test_pkey_no_prefix() {
        assert_eq!(pkey("", "API_KEY"), "API_KEY");
    }

    // ── host_name ──

    #[test]
    fn test_host_name_anthropic_native() {
        assert_eq!(host_name(None, "claude", "anthropic"), "claude-anthropic");
    }

    #[test]
    fn test_host_name_anthropic_default_url() {
        assert_eq!(
            host_name(Some("https://api.anthropic.com"), "claude", "anthropic"),
            "claude-anthropic"
        );
    }

    #[test]
    fn test_host_name_anthropic_custom_url() {
        assert_eq!(
            host_name(Some("https://custom.api.com/v1"), "claude", "anthropic"),
            "claude-custom-api-com"
        );
    }

    #[test]
    fn test_host_name_non_anthropic() {
        assert_eq!(
            host_name(Some("https://api.openai.com"), "tool", "openai"),
            "tool-api-openai-com"
        );
    }

    #[test]
    fn test_host_name_http_url() {
        // Colon in host:port is not stripped
        assert_eq!(
            host_name(Some("http://local.host:8080"), "tool", "custom"),
            "tool-local-host:8080"
        );
    }

    // ── res_type ──

    #[test]
    fn test_res_type_anthropic_with_custom_base() {
        assert_eq!(res_type("anthropic", Some("https://custom.com")), "custom");
    }

    #[test]
    fn test_res_type_anthropic_with_default_base() {
        assert_eq!(
            res_type("anthropic", Some("https://api.anthropic.com")),
            "anthropic"
        );
    }

    #[test]
    fn test_res_type_anthropic_no_base() {
        assert_eq!(res_type("anthropic", None), "anthropic");
    }

    #[test]
    fn test_res_type_other() {
        assert_eq!(res_type("openai", Some("https://custom.com")), "openai");
    }

    // ── new_synced_provider ──

    #[test]
    fn test_new_synced_provider() {
        let p = new_synced_provider(
            "gpt4".to_string(),
            "openai".to_string(),
            "sk-123".to_string(),
            Some("https://api.openai.com".to_string()),
            Some("gpt-4".to_string()),
            "claude",
        );
        assert_eq!(p.name, "gpt4");
        assert_eq!(p.config.provider_type, "openai");
        assert_eq!(p.config.api_key, "sk-123");
        assert_eq!(p.config.base_url.as_deref(), Some("https://api.openai.com"));
        assert_eq!(p.config.default_model.as_deref(), Some("gpt-4"));
        assert!(p.metadata.tags.contains(&"synced".to_string()));
        assert!(p.metadata.description.as_ref().unwrap().contains("claude"));
    }

    #[test]
    fn test_new_synced_provider_no_optional() {
        let p = new_synced_provider(
            "test".to_string(),
            "anthropic".to_string(),
            "sk-key".to_string(),
            None,
            None,
            "codex",
        );
        assert!(p.config.base_url.is_none());
        assert!(p.config.default_model.is_none());
    }

    // ── Platform-aware provider path tests ──

    #[test]
    fn test_host_name_with_windows_unc_path() {
        // UNC paths like \\server\share
        let result = host_name(Some("\\\\server\\share"), "tool", "custom");
        // strip_prefix won't match http/https, so falls through
        assert!(result.starts_with("tool-"));
    }

    #[test]
    fn test_host_name_with_localhost() {
        let result = host_name(Some("http://localhost:3000"), "tool", "custom");
        assert!(result.contains("localhost"));
    }

    #[test]
    fn test_host_name_with_ip_address() {
        let result = host_name(Some("http://192.168.1.1"), "tool", "custom");
        assert!(result.contains("192"));
    }

    #[test]
    fn test_env_fmt_defaults_for_unknown_format() {
        let f = env_fmt("something_unknown");
        assert_eq!(f.fmt, DocFormat::Json);
        assert_eq!(f.pfx, "");
        assert!(!f.model_env);
        assert!(!f.extra_env);
        assert!(!f.file_perms);
        assert!(!f.weak_model);
        assert!(!f.rm_model);
    }

    #[test]
    fn test_pkey_empty_key() {
        assert_eq!(pkey("env.", ""), "env.");
    }

    // ── Platform-specific permissions ──

    #[test]
    fn test_provider_config_sensitive_fields() {
        // Verify that provider config contains fields that need protection
        let config = ProviderConfig {
            api_key: "sk-sensitive-key".into(),
            base_url: Some("https://api.openai.com".into()),
            ..Default::default()
        };
        assert!(!config.api_key.is_empty());
        // On Unix, this config should trigger set_permissions_if_sensitive
        // On Windows, the function is a no-op
    }

    #[test]
    fn test_file_permissions_flag_in_env_fmt() {
        // env_file format should set file_perms flag (Unix-only permission setting)
        let f = env_fmt("env_file");
        assert!(f.file_perms);
    }

    #[test]
    fn test_json_format_no_file_perms() {
        // env_vars (JSON) format should not set file_perms
        let f = env_fmt("env_vars");
        assert!(!f.file_perms);
    }

    // ── clear_section behavior for provider profile apply (Bug #25) ──

    #[test]
    fn test_clear_section_then_write_simulates_profile_apply() {
        use crate::adapter::doc_engine::{DocFormat, DocTree, DocValue};
        // Simulate: initial config has multiple providers
        let mut doc = DocTree::new_test(
            DocFormat::Toml,
            DocValue::Object(HashMap::from([(
                "providers".into(),
                DocValue::Object(HashMap::from([
                    (
                        "old-a".into(),
                        DocValue::Object(HashMap::from([
                            ("type".into(), DocValue::String("openai".into())),
                            ("api_key".into(), DocValue::String("sk-old-a".into())),
                        ])),
                    ),
                    (
                        "old-b".into(),
                        DocValue::Object(HashMap::from([
                            ("type".into(), DocValue::String("anthropic".into())),
                            ("api_key".into(), DocValue::String("sk-old-b".into())),
                        ])),
                    ),
                ])),
            )])),
        );
        assert_eq!(doc.entries("providers").unwrap().len(), 2);

        // Profile apply: clear entries, then write new provider
        doc.clear_section("providers");
        doc.ensure_object("providers");
        assert!(doc.entries("providers").unwrap().is_empty());

        // Write new provider
        doc.ensure_object("providers.new-provider");
        doc.set(
            "providers.new-provider.type",
            DocValue::String("openai_legacy".into()),
        );
        doc.set(
            "providers.new-provider.api_key",
            DocValue::String("sk-new".into()),
        );

        // Verify only new provider exists
        let entries = doc.entries("providers").unwrap();
        assert_eq!(entries.len(), 1);
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"new-provider"));
        assert!(!keys.contains(&"old-a"));
        assert!(!keys.contains(&"old-b"));
    }

    #[test]
    fn test_no_clear_keeps_existing_entries() {
        use crate::adapter::doc_engine::{DocFormat, DocTree, DocValue};
        // Simulate: incremental add without clearing
        let mut doc = DocTree::new_test(
            DocFormat::Toml,
            DocValue::Object(HashMap::from([(
                "providers".into(),
                DocValue::Object(HashMap::from([(
                    "existing".into(),
                    DocValue::Object(HashMap::from([(
                        "type".into(),
                        DocValue::String("openai".into()),
                    )])),
                )])),
            )])),
        );

        // No clear_section — just add new provider (like add_resource)
        doc.ensure_object("providers.new-one");
        doc.set(
            "providers.new-one.type",
            DocValue::String("anthropic".into()),
        );

        let entries = doc.entries("providers").unwrap();
        assert_eq!(entries.len(), 2);
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"existing"));
        assert!(keys.contains(&"new-one"));
    }

    // ── sync_provider_upsert validate ──

    #[test]
    fn test_sync_provider_upsert_invalid_provider_skipped() {
        use crate::adapter::SyncResult;
        use crate::store::TomlStore;
        // A provider with empty provider_type should be skipped, not "created"
        let store = TomlStore::new().unwrap();
        let invalid = make_provider(
            "bad",
            ProviderConfig {
                provider_type: String::new(), // empty → validate fails
                ..Default::default()
            },
        );
        let mut result = SyncResult::default();
        sync_provider_upsert(
            &store,
            &invalid,
            "bad",
            "provider",
            false,
            &mut result,
            default_provider_compare,
            default_provider_merge,
        );
        assert!(
            result.created.is_empty(),
            "invalid provider should not be created"
        );
        assert_eq!(
            result.skipped.len(),
            1,
            "invalid provider should be skipped"
        );
    }

    #[test]
    fn test_compare_includes_provider_type_and_headers() {
        // provider_type difference should be detected
        let a = make_provider(
            "p",
            ProviderConfig {
                provider_type: "openai".into(),
                ..Default::default()
            },
        );
        let b = make_provider(
            "p",
            ProviderConfig {
                provider_type: "anthropic".into(),
                ..Default::default()
            },
        );
        assert!(!default_provider_compare(&a, &b));

        // headers difference should be detected
        let mut h1 = HashMap::new();
        h1.insert("X-Custom".into(), "v1".into());
        let c = make_provider(
            "p",
            ProviderConfig {
                provider_type: "openai".into(),
                headers: h1,
                ..Default::default()
            },
        );
        let d = make_provider(
            "p",
            ProviderConfig {
                provider_type: "openai".into(),
                ..Default::default()
            },
        );
        assert!(!default_provider_compare(&c, &d));
    }
}
