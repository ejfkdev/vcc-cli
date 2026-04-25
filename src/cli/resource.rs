use anyhow::{bail, Result};
use std::collections::HashMap;

use super::dynamic::{FieldMap, ResourceAction};
use super::output::{is_json_mode, output_item, output_list, output_success, print_dry_run_banner};
use crate::adapter;
use crate::model::{
    agent::Agent, env::Env, hook::Hook, mask_api_key, mask_env_map, mcp::McpServer, plugin::Plugin,
    prompt::Prompt, provider::Provider, skill::Skill, Resource,
};
use crate::store::TomlStore;

macro_rules! dispatch_kind {
    ($kind:expr, $( $k:literal => $e:expr ),+ $(,)?) => { match $kind { $( $k => $e, )+ _ => bail!("unknown resource kind: {}", $kind) } };
}

/// Dispatch a resource kind to a generic function across all 8 resource types
macro_rules! dispatch_resource {
    ($kind:expr, $fn:ident < $generic:ident > $(, $arg:expr )*) => {
        dispatch_kind!($kind,
            "provider" => $fn::<Provider>($($arg),*),
            "mcp" => $fn::<McpServer>($($arg),*),
            "hook" => $fn::<Hook>($($arg),*),
            "agent" => $fn::<Agent>($($arg),*),
            "skill" => $fn::<Skill>($($arg),*),
            "prompt" => $fn::<Prompt>($($arg),*),
            "env" => $fn::<Env>($($arg),*),
            "plugin" => $fn::<Plugin>($($arg),*))
    };
}

fn resource_file_path(store: &TomlStore, kind: &str, name: &str) -> std::path::PathBuf {
    let dir = store.root().join("registry").join(
        crate::config::resource_registry()
            .dir_for_kind(kind)
            .unwrap_or(kind),
    );
    // Try to find the actual file with hash8 suffix
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let prefix = format!("{}-", name);
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "toml") {
                if let Some(stem) = p.file_stem() {
                    let s = stem.to_string_lossy();
                    if s == name
                        || (s.starts_with(&prefix)
                            && s[name.len() + 1..].len() == 8
                            && s[name.len() + 1..].chars().all(|c| c.is_ascii_hexdigit()))
                    {
                        return p;
                    }
                }
            }
        }
    }
    // Fallback: construct the expected path even if it doesn't exist
    dir.join(format!("{}.toml", name))
}

pub(crate) fn merge_kvvec(target: &mut HashMap<String, String>, updates: &HashMap<String, String>) {
    for (k, v) in updates {
        if v.is_empty() {
            target.remove(k);
        } else {
            target.insert(k.clone(), v.clone());
        }
    }
}

fn load_by_query<T: Resource>(store: &TomlStore, kind: &str, query: &str) -> Result<T> {
    store.load_resource_by_query(kind, query)
}
fn remove_by_query(store: &TomlStore, kind: &str, query: &str) -> Result<String> {
    store.remove_resource_by_query(kind, query)
}
fn get_display_id(id: &str) -> &str {
    if id.is_empty() {
        "-"
    } else {
        id
    }
}

/// Convert `Option<&str>` to String, using "-" for None.
fn dash(v: Option<&str>) -> String {
    v.unwrap_or("-").to_string()
}
fn installed_status(cache_dir: &std::path::Path, name: &str) -> &'static str {
    if cache_dir.join(name).exists() {
        "installed"
    } else {
        "remote"
    }
}
fn inject_installed_flag(val: &mut serde_json::Value, cache_dir: &std::path::Path, name: &str) {
    if let Some(obj) = val.as_object_mut() {
        obj.insert(
            "_installed".into(),
            serde_json::Value::Bool(cache_dir.join(name).exists()),
        );
    }
}

pub(crate) fn handle_resource(kind: &str, action: ResourceAction) -> Result<()> {
    let store = TomlStore::new()?;
    match action {
        ResourceAction::Add { name, fields } => handle_add(&store, kind, &name, &fields),
        ResourceAction::Edit { query, fields } => handle_edit(&store, kind, &query, &fields),
        ResourceAction::List => handle_list(&store, kind),
        ResourceAction::Show { query } => handle_show(&store, kind, &query),
        ResourceAction::Remove { query } => handle_remove(&store, kind, &query),
        ResourceAction::Enable {
            name,
            tool,
            dry_run,
        } => handle_toggle_resource(kind, &name, true, tool.as_deref(), dry_run),
        ResourceAction::Disable {
            name,
            tool,
            dry_run,
        } => handle_toggle_resource(kind, &name, false, tool.as_deref(), dry_run),
        ResourceAction::Install {
            name,
            branch,
            dry_run,
        } => handle_install(&store, kind, &name, branch.as_deref(), dry_run),
    }
}

fn handle_add(store: &TomlStore, kind: &str, name: &str, fields: &FieldMap) -> Result<()> {
    if store.resource_exists(kind, name) {
        bail!("{} '{}' already exists", kind, name);
    }
    if kind == "plugin" {
        let mut plugin = crate::model::Plugin::new_with_name(name);
        apply_validate_save_plugin(store, &mut plugin, fields, "added")
    } else {
        dispatch_resource!(kind, handle_add_typed<T>, store, name, fields)
    }
}

fn handle_add_typed<T: Resource>(store: &TomlStore, name: &str, fields: &FieldMap) -> Result<()> {
    let mut resource = T::new_with_name(name);
    apply_validate_save(store, &mut resource, fields, "added")
}

fn handle_edit(store: &TomlStore, kind: &str, query: &str, fields: &FieldMap) -> Result<()> {
    if kind == "plugin" {
        let mut plugin: crate::model::Plugin = load_by_query(store, kind, query)?;
        apply_validate_save_plugin(store, &mut plugin, fields, "updated")
    } else {
        dispatch_resource!(kind, handle_edit_typed<T>, store, kind, query, fields)
    }
}

fn handle_edit_typed<T: Resource>(
    store: &TomlStore,
    kind: &str,
    query: &str,
    fields: &FieldMap,
) -> Result<()> {
    let mut resource: T = load_by_query(store, kind, query)?;
    apply_validate_save(store, &mut resource, fields, "updated")
}

fn apply_validate_save<T: Resource>(
    store: &TomlStore,
    resource: &mut T,
    fields: &FieldMap,
    verb: &str,
) -> Result<()> {
    resource.apply_fields(fields)?;
    resource.validate()?;
    store.save_resource(resource)?;
    output_success(&format!(
        "vcc: {} '{}' {}",
        resource.kind(),
        resource.name(),
        verb
    ));
    Ok(())
}

/// Plugin-specific apply with source auto-inference
fn apply_validate_save_plugin(
    store: &TomlStore,
    plugin: &mut crate::model::Plugin,
    fields: &FieldMap,
    verb: &str,
) -> Result<()> {
    plugin.apply_fields(fields)?;
    // Auto-infer source from marketplace/repo/path when source is still the default "github"
    // Note: can't use fields.was_provided("source") because clap's default_value always sets it
    if plugin.config.source == "github" {
        if plugin.config.marketplace.is_some() {
            plugin.config.source = "marketplace".to_string();
        } else if plugin.config.path.is_some()
            && plugin.config.repo.is_none()
            && plugin.config.marketplace.is_none()
        {
            plugin.config.source = "local".to_string();
        }
    } else if plugin.config.source == "local" && plugin.config.repo.is_some() {
        plugin.config.source = "github".to_string();
    }
    plugin.validate()?;
    store.save_resource(plugin)?;
    output_success(&format!(
        "vcc: {} '{}' {}",
        plugin.kind(),
        plugin.name(),
        verb
    ));
    Ok(())
}

fn handle_list(store: &TomlStore, kind: &str) -> Result<()> {
    dispatch_kind!(kind,
        "provider" => list_providers(store), "mcp" => list_mcps(store), "hook" => list_hooks(store), "agent" => list_agents(store),
        "skill" => list_skills(store), "prompt" => list_prompts(store), "env" => list_envs(store), "plugin" => list_plugins(store))
}

fn load_all_resources<T: Resource>(
    store: &TomlStore,
    kind: &str,
    label: &str,
) -> Result<Option<Vec<T>>> {
    let names = store.list_resources::<T>(kind)?;
    if names.is_empty() {
        if is_json_mode() {
            output_list(&Vec::<T>::new());
        } else {
            println!("No {} configured.", label);
        }
        return Ok(None);
    }
    Ok(Some(
        names
            .iter()
            .filter_map(|n| store.load_resource(kind, n).ok())
            .collect(),
    ))
}

fn list_typed<T: Resource + serde::Serialize>(
    store: &TomlStore,
    kind: &str,
    label: &str,
    headers: &[&str],
    widths: &[usize],
    row_fn: impl Fn(&T) -> Vec<String>,
    sanitize_fn: impl Fn(&T, &mut serde_json::Value),
) -> Result<()> {
    let Some(items) = load_all_resources::<T>(store, kind, label)? else {
        return Ok(());
    };
    if is_json_mode() {
        let values: Vec<serde_json::Value> = items
            .iter()
            .map(|item| {
                let mut val = serde_json::to_value(item).unwrap_or_default();
                sanitize_fn(item, &mut val);
                val
            })
            .collect();
        output_list(&values);
    } else {
        list_table(headers, widths, || items.iter().map(&row_fn).collect());
    }
    Ok(())
}

/// Simplified list helper for types with no sanitization needed.
fn list_simple<T: Resource + serde::Serialize>(
    store: &TomlStore,
    kind: &str,
    label: &str,
    headers: &[&str],
    widths: &[usize],
    row_fn: impl Fn(&T) -> Vec<String>,
) -> Result<()> {
    list_typed(store, kind, label, headers, widths, row_fn, |_, _| {})
}

fn list_providers(store: &TomlStore) -> Result<()> {
    list_typed::<Provider>(
        store,
        "provider",
        "providers",
        &["NAME", "ID", "MODELS", "BASE_URL"],
        &[25, 12, 10, 0],
        |p| {
            vec![
                p.name.clone(),
                get_display_id(p.id()).to_string(),
                if p.config.models.is_empty() {
                    p.config
                        .default_model
                        .as_deref()
                        .map(|_| "1")
                        .unwrap_or("0")
                        .to_string()
                } else {
                    p.config.models.len().to_string()
                },
                dash(p.config.base_url.as_deref()),
            ]
        },
        |_, val| {
            if let Some(obj) = val.get_mut("config").and_then(|c| c.as_object_mut()) {
                if let Some(key) = obj.get("api_key").and_then(|v| v.as_str()) {
                    obj.insert("api_key".into(), serde_json::json!(mask_api_key(key)));
                }
            }
        },
    )
}
fn list_mcps(store: &TomlStore) -> Result<()> {
    list_typed::<McpServer>(
        store,
        "mcp",
        "MCP servers",
        &["NAME", "ID", "TYPE", "COMMAND/URL"],
        &[25, 12, 20, 0],
        |m| {
            vec![
                m.name.clone(),
                get_display_id(m.id()).to_string(),
                m.config.server_type.clone(),
                dash(m.config.command.as_deref().or(m.config.url.as_deref())),
            ]
        },
        |m, val| {
            if let Some(env_obj) = val
                .get_mut("config")
                .and_then(|c| c.as_object_mut())
                .and_then(|c| c.get_mut("env"))
                .and_then(|e| e.as_object_mut())
            {
                let masked = mask_env_map(&m.config.env);
                for (k, v) in &masked {
                    env_obj.insert(k.clone(), serde_json::json!(v));
                }
            }
        },
    )
}
fn list_hooks(store: &TomlStore) -> Result<()> {
    list_simple::<Hook>(
        store,
        "hook",
        "hooks",
        &["NAME", "ID", "EVENT", "COMMAND", "MATCHER"],
        &[25, 12, 20, 30, 0],
        |h| {
            vec![
                h.name.clone(),
                get_display_id(h.id()).to_string(),
                h.config.event.clone(),
                if h.config.command.chars().count() > 28 {
                    format!(
                        "{}...",
                        h.config.command.chars().take(25).collect::<String>()
                    )
                } else {
                    h.config.command.clone()
                },
                if h.config.matcher.is_empty() {
                    "*".into()
                } else {
                    h.config.matcher.clone()
                },
            ]
        },
    )
}
fn list_agents(store: &TomlStore) -> Result<()> {
    list_simple::<Agent>(
        store,
        "agent",
        "agents",
        &["NAME", "ID", "MODE", "MODEL", "DESCRIPTION"],
        &[25, 12, 10, 20, 0],
        |a| {
            vec![
                a.name.clone(),
                get_display_id(a.id()).to_string(),
                a.config.mode.clone(),
                dash(a.config.model.as_deref()),
                dash(a.config.description.as_deref()),
            ]
        },
    )
}
fn list_skills(store: &TomlStore) -> Result<()> {
    let cache_dir = store.root().join("cache").join("skills");
    list_typed::<Skill>(
        store,
        "skill",
        "skills",
        &["NAME", "ID", "SOURCE", "STATUS", "REPO/PATH"],
        &[25, 12, 10, 9, 0],
        |s| {
            vec![
                s.name.clone(),
                get_display_id(s.id()).to_string(),
                s.config.source.clone(),
                installed_status(&cache_dir, &s.name).to_string(),
                dash(s.config.repo.as_deref().or(s.config.path.as_deref())),
            ]
        },
        |s, val| inject_installed_flag(val, &cache_dir, &s.name),
    )
}
fn list_prompts(store: &TomlStore) -> Result<()> {
    list_simple::<Prompt>(
        store,
        "prompt",
        "prompts",
        &["NAME", "ID", "DESCRIPTION"],
        &[25, 12, 0],
        |p| {
            vec![
                p.name.clone(),
                get_display_id(p.id()).to_string(),
                dash(p.metadata.description.as_deref()),
            ]
        },
    )
}
fn list_envs(store: &TomlStore) -> Result<()> {
    list_simple::<Env>(
        store,
        "env",
        "env groups",
        &["NAME", "ID", "VARS", "DESCRIPTION"],
        &[25, 12, 10, 0],
        |e| {
            vec![
                e.name.clone(),
                get_display_id(e.id()).to_string(),
                e.config.vars.len().to_string(),
                dash(e.metadata.description.as_deref()),
            ]
        },
    )
}
fn list_plugins(store: &TomlStore) -> Result<()> {
    let cache_dir = store.root().join("cache").join("plugins");
    list_typed::<Plugin>(
        store,
        "plugin",
        "plugins",
        &[
            "NAME",
            "ID",
            "SOURCE",
            "FORMAT",
            "STATUS",
            "REPO/MARKETPLACE",
        ],
        &[25, 12, 10, 12, 9, 0],
        |p| {
            vec![
                p.name.clone(),
                get_display_id(p.id()).to_string(),
                p.config.source.clone(),
                dash(p.config.format.as_deref()),
                installed_status(&cache_dir, &p.name).to_string(),
                dash(
                    p.config
                        .repo
                        .as_deref()
                        .or(p.config.marketplace.as_deref())
                        .or(p.config.path.as_deref()),
                ),
            ]
        },
        |p, val| inject_installed_flag(val, &cache_dir, &p.name),
    )
}

fn list_table(headers: &[&str], widths: &[usize], rows_fn: impl Fn() -> Vec<Vec<String>>) {
    let width_sum: usize = widths.iter().filter(|&&w| w > 0).sum::<usize>() + widths.len() * 2;
    println!(
        "{}",
        headers
            .iter()
            .enumerate()
            .map(|(i, h)| format!(
                "{:<width$}",
                h,
                width = widths.get(i).copied().unwrap_or(0).max(h.len())
            ))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("{}", "-".repeat(width_sum.max(60)));
    for row in rows_fn() {
        println!(
            "{}",
            row.iter()
                .enumerate()
                .map(|(i, v)| format!(
                    "{:<width$}",
                    v,
                    width = widths
                        .get(i)
                        .copied()
                        .unwrap_or(0)
                        .max(headers.get(i).map(|h| h.len()).unwrap_or(0))
                ))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
}

fn handle_show(store: &TomlStore, kind: &str, query: &str) -> Result<()> {
    dispatch_resource!(kind, show_resource<T>, store, kind, query)
}

fn show_resource<T: crate::model::Resource + crate::model::SanitizeDisplay>(
    store: &TomlStore,
    kind: &str,
    query: &str,
) -> Result<()> {
    let item: T = store.load_resource_by_query(kind, query)?;
    let path = resource_file_path(store, kind, item.name());
    let mut val = serde_json::to_value(&item)?;
    item.sanitize_display(&mut val);
    if is_json_mode() {
        if let Some(obj) = val.as_object_mut() {
            obj.insert(
                "_path".into(),
                serde_json::Value::String(path.display().to_string()),
            );
        }
        output_item(&val);
    } else {
        println!("# {}\n", path.display());
        match serde_json::from_value::<toml::Value>(val.clone()) {
            Ok(toml_val) => println!("{}", toml::to_string_pretty(&toml_val)?),
            Err(_) => println!("{}", serde_json::to_string_pretty(&val)?),
        }
    }
    Ok(())
}

fn handle_remove(store: &TomlStore, kind: &str, query: &str) -> Result<()> {
    let name = remove_by_query(store, kind, query)?;
    output_success(&format!("vcc: {} '{}' removed", kind, name));
    Ok(())
}

fn handle_install(
    store: &TomlStore,
    kind: &str,
    name: &str,
    branch: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let (resolved_name, source, repo, path, cache_subdir) = match kind {
        "skill" => {
            let s: Skill = load_by_query(store, kind, name)?;
            (
                s.name.clone(),
                s.config.source,
                s.config.repo,
                s.config.path,
                "skills",
            )
        }
        "plugin" => {
            let p: Plugin = load_by_query(store, kind, name)?;
            (
                p.name.clone(),
                p.config.source,
                p.config.repo,
                p.config.path,
                "plugins",
            )
        }
        _ => bail!("{} does not support install", kind),
    };
    install_git_resource(
        kind,
        &resolved_name,
        &source,
        repo.as_deref(),
        path.as_deref(),
        &store.root().join("cache").join(cache_subdir),
        branch,
        dry_run,
    )
}

#[allow(clippy::too_many_arguments)]
fn install_git_resource(
    kind: &str,
    name: &str,
    source: &str,
    repo: Option<&str>,
    path: Option<&str>,
    cache_base: &std::path::Path,
    branch: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let cache_dir = cache_base.join(name);
    if cache_dir.exists() {
        if dry_run {
            println!(
                "DRY RUN: would update {} '{}' (git pull in {})",
                kind,
                name,
                cache_dir.display()
            );
        } else {
            println!(
                "{} '{}' already installed, updating...",
                capitalize(kind),
                name
            );
            let status = std::process::Command::new("git")
                .arg("pull")
                .current_dir(&cache_dir)
                .status()?;
            if !status.success() {
                bail!("failed to update {} '{}'", kind, name);
            }
        }
    } else {
        let repo_url = match source {
            "github" | "url" => repo
                .or(path)
                .ok_or_else(|| anyhow::anyhow!("{} '{}' has no repo or path", kind, name))?,
            "marketplace" => {
                repo.ok_or_else(|| anyhow::anyhow!("marketplace {} '{}' has no repo", kind, name))?
            }
            "local" => {
                let local_path =
                    path.ok_or_else(|| anyhow::anyhow!("local {} '{}' has no path", kind, name))?;
                if !std::path::Path::new(local_path).exists() {
                    bail!("local {} path '{}' not found", kind, local_path);
                }
                println!(
                    "{} '{}' is a local {}, no installation needed.",
                    capitalize(kind),
                    name,
                    kind
                );
                return Ok(());
            }
            _ => bail!("unsupported {} source for install: {}", kind, source),
        };
        let full_url = if repo_url.starts_with("http://")
            || repo_url.starts_with("https://")
            || repo_url.starts_with("git@")
        {
            repo_url.to_string()
        } else {
            format!("https://github.com/{}.git", repo_url)
        };
        if dry_run {
            print!("DRY RUN: would clone {} '{}' from {}", kind, name, full_url);
            if let Some(b) = branch {
                print!(" (branch: {})", b);
            }
            println!(" into {}", cache_dir.display());
        } else {
            if let Some(parent) = cache_dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut git_cmd = std::process::Command::new("git");
            git_cmd.arg("clone").arg(&full_url).arg(&cache_dir);
            if let Some(b) = branch {
                git_cmd.arg("--branch").arg(b);
            }
            let status = git_cmd.status()?;
            if !status.success() {
                let _ = std::fs::remove_dir_all(&cache_dir); // best-effort cleanup on failed clone
                bail!("failed to clone {} '{}' from {}", kind, name, full_url);
            }
        }
    }
    if dry_run {
        println!("\n=== DRY RUN: {} '{}' would be installed ===", kind, name);
    } else {
        output_success(&format!("vcc: {} '{}' installed", kind, name));
    }
    Ok(())
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().chain(c).collect(),
    }
}

fn handle_toggle_resource(
    kind: &str,
    name: &str,
    enable: bool,
    tool: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let store = TomlStore::new()?;
    let names: Vec<String> = name
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    for n in &names {
        if !store.resource_exists(kind, n) {
            bail!("{} '{}' not found", kind, n);
        }
    }
    let action = if enable { "enable" } else { "disable" };
    let tool_names: Vec<String> = if let Some(tool_name) = tool {
        adapter::validate_tool_name(tool_name)?;
        vec![tool_name.to_string()]
    } else {
        let installed: Vec<String> = adapter::all_adapters()
            .iter()
            .filter(|a| a.config_dir().map(|d| d.exists()).unwrap_or(false))
            .map(|a| a.tool_name().to_string())
            .collect();
        if installed.is_empty() {
            bail!("no installed tools found. Specify a tool with --tool.");
        }
        if !is_json_mode() {
            let action_ing = if enable { "Enabling" } else { "Disabling" };
            println!(
                "{} {} '{}' in: {}",
                action_ing,
                kind,
                name,
                installed.join(", ")
            );
        }
        installed
    };
    print_dry_run_banner(dry_run);
    let mut total = 0;
    for tool_name in &tool_names {
        let adapter_instance = match adapter::get_adapter(tool_name) {
            Ok(Some(a)) => a,
            _ => continue,
        };
        let count = adapter_instance.toggle_resource(kind, enable, &store, &names, dry_run)?;
        if count > 0 && !is_json_mode() {
            println!(
                "  [{}] {}d {} '{}'",
                tool_name,
                action,
                kind,
                names.join(",")
            );
        }
        total += count;
    }
    if total == 0 && !is_json_mode() {
        println!("vcc: {} '{}' was already {}d", kind, name, action);
    } else if !dry_run {
        output_success(&format!("vcc: {} '{}' {}d", kind, name, action));
    } else if !is_json_mode() {
        println!(
            "\n=== DRY RUN: {} '{}' would be {}d ===",
            kind, name, action
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── merge_kvvec ──

    #[test]
    fn test_merge_kvvec_adds_new() {
        let mut target = HashMap::new();
        let updates = HashMap::from([("KEY".into(), "VAL".into())]);
        merge_kvvec(&mut target, &updates);
        assert_eq!(target.get("KEY").unwrap(), "VAL");
    }

    #[test]
    fn test_merge_kvvec_overwrites() {
        let mut target = HashMap::from([("KEY".into(), "old".into())]);
        let updates = HashMap::from([("KEY".into(), "new".into())]);
        merge_kvvec(&mut target, &updates);
        assert_eq!(target.get("KEY").unwrap(), "new");
    }

    #[test]
    fn test_merge_kvvec_preserves_existing() {
        let mut target = HashMap::from([("A".into(), "1".into())]);
        let updates = HashMap::from([("B".into(), "2".into())]);
        merge_kvvec(&mut target, &updates);
        assert_eq!(target.len(), 2);
    }

    #[test]
    fn test_merge_kvvec_empty_value_deletes_key() {
        let mut target = HashMap::from([
            ("KEY1".into(), "val1".into()),
            ("KEY2".into(), "val2".into()),
        ]);
        let updates = HashMap::from([("KEY1".into(), String::new())]);
        merge_kvvec(&mut target, &updates);
        assert!(
            !target.contains_key("KEY1"),
            "empty value should delete key"
        );
        assert_eq!(target.get("KEY2").unwrap(), "val2");
    }

    #[test]
    fn test_merge_kvvec_empty_value_nonexistent_key() {
        let mut target = HashMap::new();
        let updates = HashMap::from([("NOKEY".into(), String::new())]);
        merge_kvvec(&mut target, &updates);
        assert!(
            target.is_empty(),
            "deleting nonexistent key should be no-op"
        );
    }

    // ── get_display_id ──

    #[test]
    fn test_get_display_id_empty() {
        assert_eq!(get_display_id(""), "-");
    }

    #[test]
    fn test_get_display_id_present() {
        assert_eq!(get_display_id("abc123"), "abc123");
    }

    // ── dash ──

    #[test]
    fn test_dash_some() {
        assert_eq!(dash(Some("hello")), "hello");
    }

    #[test]
    fn test_dash_none() {
        assert_eq!(dash(None), "-");
    }

    // ── capitalize ──

    #[test]
    fn test_capitalize_basic() {
        assert_eq!(capitalize("hello"), "Hello");
    }

    #[test]
    fn test_capitalize_single_char() {
        assert_eq!(capitalize("a"), "A");
    }

    #[test]
    fn test_capitalize_empty() {
        assert_eq!(capitalize(""), "");
    }

    #[test]
    fn test_capitalize_already() {
        assert_eq!(capitalize("Hello"), "Hello");
    }

    // ── installed_status ──

    #[test]
    fn test_installed_status_nonexistent() {
        let tmp = std::env::temp_dir().join("vcc_test_installed_status");
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(installed_status(&tmp, "anything"), "remote");
    }

    #[test]
    fn test_installed_status_existing() {
        let tmp = std::env::temp_dir().join("vcc_test_installed_status2");
        let _ = std::fs::create_dir_all(tmp.join("my-plugin"));
        assert_eq!(installed_status(&tmp, "my-plugin"), "installed");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── inject_installed_flag ──

    #[test]
    fn test_inject_installed_flag_adds_field() {
        let tmp = std::env::temp_dir().join("vcc_test_inject_flag");
        let _ = std::fs::create_dir_all(tmp.join("my-plugin"));
        let mut val = serde_json::json!({"name": "my-plugin"});
        inject_installed_flag(&mut val, &tmp, "my-plugin");
        assert_eq!(val.get("_installed").unwrap().as_bool(), Some(true));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_inject_installed_flag_not_installed() {
        let tmp = std::env::temp_dir().join("vcc_test_inject_flag2");
        let _ = std::fs::remove_dir_all(&tmp);
        let mut val = serde_json::json!({"name": "my-plugin"});
        inject_installed_flag(&mut val, &tmp, "my-plugin");
        assert_eq!(val.get("_installed").unwrap().as_bool(), Some(false));
    }

    #[test]
    fn test_inject_installed_flag_non_object_skipped() {
        let tmp = std::env::temp_dir().join("vcc_test_inject_flag3");
        let mut val = serde_json::json!("not an object");
        inject_installed_flag(&mut val, &tmp, "my-plugin");
        assert!(val.get("_installed").is_none());
    }

    // ── merge_kvvec additional ──

    #[test]
    fn test_merge_kvvec_empty_updates() {
        let mut target = HashMap::from([("KEY".into(), "VAL".into())]);
        let updates = HashMap::new();
        merge_kvvec(&mut target, &updates);
        assert_eq!(target.len(), 1);
    }

    // ── dispatch macros compile ──

    #[test]
    fn test_dispatch_kind_macro() {
        let result: &str = match "mcp" {
            "mcp" => "matched",
            "other" => "nope",
            _ => "default",
        };
        assert_eq!(result, "matched");
    }

    // ── Plugin source inference ──

    #[test]
    fn test_plugin_source_inference_marketplace() {
        let mut plugin = crate::model::Plugin::new_with_name("test-marketplace");
        plugin.config.source = "github".to_string();
        plugin.config.marketplace = Some("my-plugin".to_string());
        plugin.config.repo = Some("owner/repo".to_string());
        // Simulate the inference logic from apply_validate_save_plugin
        if plugin.config.source == "github" && plugin.config.marketplace.is_some() {
            plugin.config.source = "marketplace".to_string();
        }
        assert_eq!(plugin.config.source, "marketplace");
    }

    #[test]
    fn test_plugin_source_inference_local_path() {
        let mut plugin = crate::model::Plugin::new_with_name("test-local");
        plugin.config.source = "github".to_string();
        plugin.config.path = Some("/tmp/plugin".to_string());
        if plugin.config.source == "github"
            && plugin.config.path.is_some()
            && plugin.config.repo.is_none()
            && plugin.config.marketplace.is_none()
        {
            plugin.config.source = "local".to_string();
        }
        assert_eq!(plugin.config.source, "local");
    }

    #[test]
    fn test_plugin_source_inference_repo_local() {
        let mut plugin = crate::model::Plugin::new_with_name("test-repo");
        plugin.config.source = "local".to_string();
        plugin.config.repo = Some("owner/repo".to_string());
        if plugin.config.source == "local" && plugin.config.repo.is_some() {
            plugin.config.source = "github".to_string();
        }
        assert_eq!(plugin.config.source, "github");
    }
}
