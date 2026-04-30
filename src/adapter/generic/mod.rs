pub mod crud;
pub mod helpers;

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::adapter::agent_format::{parse_agent_markdown, parse_agent_yaml};
use crate::adapter::doc_engine::{DocFormat, DocTree, DocValue};
use crate::adapter::plugin_manifest::{adapt_plugin_manifest, get_manifest_dir, get_manifest_file};
use crate::adapter::provider;
use crate::adapter::{
    apply_profile_override, load_default, load_default_name, resolve_env, resolve_hook,
    resolve_mcp, resolve_prompt, resolve_provider, Adapter, InspectItem, InspectResult,
    InspectSection, SyncItem, SyncResult,
};
use crate::model::{
    agent::Agent, env::Env, hook::Hook, mcp::McpServer, plugin::Plugin, profile::Profile,
    prompt::Prompt, provider::Provider, skill::Skill, Resource,
};
use crate::store::TomlStore;

use helpers::write_agent_file;
use helpers::*;

pub(crate) struct GenericAdapter {
    pub(super) mapping: crate::adapter::mapping::ToolMapping,
}

impl GenericAdapter {
    pub fn new(tool_name: &str) -> Result<Self> {
        Ok(Self {
            mapping: crate::adapter::mapping::ToolMapping::load_for_tool(tool_name)
                .with_context(|| format!("failed to load mapping for tool '{}'", tool_name))?,
        })
    }
    fn require_config_dir(&self) -> Result<PathBuf> {
        self.config_dir().context("cannot find config directory")
    }
    fn settings_path_from(&self, dir: impl AsRef<std::path::Path>) -> PathBuf {
        dir.as_ref().join(self.mapping.settings_file())
    }
    fn prompt_path_from(&self, dir: impl AsRef<std::path::Path>) -> PathBuf {
        dir.as_ref().join(self.mapping.prompt.path())
    }
    fn agents_dir(&self) -> Result<PathBuf> {
        Ok(self.require_config_dir()?.join(self.mapping.agent.path()))
    }
    fn skills_dir(&self) -> Result<PathBuf> {
        Ok(self.require_config_dir()?.join(self.mapping.skill.path()))
    }
    fn plugins_key(&self) -> &str {
        self.mapping.plugin.plugins_key()
    }
    fn plugin_install_dir(&self, dir: &std::path::Path) -> PathBuf {
        dir.join(self.mapping.plugin.install_dir())
    }
    /// Insert `$schema` key if jsonc format and not already present.
    fn maybe_insert_schema(&self, doc: &mut DocTree) {
        if self.mapping.mcp.jsonc && !doc.exists("$schema") {
            if let Some(url) = &crate::config::adapter_defaults().opencode.schema_url {
                doc.set("$schema", DocValue::String(url.clone()));
            }
        }
    }
    /// Load MCP config doc, apply closure, save if not dry_run.
    fn with_mcp_doc<F, R>(&self, dry_run: bool, f: F) -> Result<R>
    where
        F: FnOnce(&mut DocTree) -> Result<R>,
    {
        let (path, format) = self.mcp_config_path()?;
        with_doc(&path, format, dry_run, f)
    }
    /// Load env config doc, apply closure, save if not dry_run.
    fn with_env_doc<F, R>(&self, dry_run: bool, f: F) -> Result<R>
    where
        F: FnOnce(&mut DocTree) -> Result<R>,
    {
        let (path, format) = crud::env_config_path(&self.mapping)
            .ok_or_else(|| anyhow::anyhow!("env not supported for {}", self.tool_name()))?;
        with_doc(&path, format, dry_run, f)
    }
    /// Returns (is_yaml, extension) for the agent format configured in this tool.
    fn agent_format_info(&self) -> (bool, &'static str) {
        let is_yaml = self.mapping.agent.format == "kimi_yaml";
        (is_yaml, if is_yaml { "yaml" } else { "md" })
    }
    /// Returns (config_path, format, is_toml) for the hook format configured in this tool.
    fn hook_config_info(&self, dir: &std::path::Path) -> (PathBuf, DocFormat, bool) {
        let is_toml = self.mapping.hook.format == "toml_hooks";
        let config_path = if is_toml {
            dir.join(self.mapping.hook.path())
        } else {
            self.settings_path_from(dir)
        };
        let format = if is_toml {
            DocFormat::Toml
        } else {
            DocFormat::from_filename(self.mapping.settings_file())
        };
        (config_path, format, is_toml)
    }
    /// Load hook config doc, apply closure (receives is_toml flag), save if not dry_run.
    fn with_hook_doc<F, R>(&self, dry_run: bool, f: F) -> Result<R>
    where
        F: FnOnce(&mut DocTree, bool) -> Result<R>,
    {
        let dir = self.require_config_dir()?;
        let (config_path, format, is_toml) = self.hook_config_info(&dir);
        with_doc(&config_path, format, dry_run, |doc| f(doc, is_toml))
    }
    fn mcp_config_path(&self) -> Result<(PathBuf, DocFormat)> {
        let dir = self.require_config_dir()?;
        let path = dir.join(self.mapping.mcp.path());
        Ok((path, DocFormat::from_format_str(&self.mapping.mcp.format)))
    }
    fn plugin_doc_config_path(&self) -> Result<(PathBuf, DocFormat)> {
        let plugin_cfg = &self.mapping.plugin;
        let format_str = if plugin_cfg.format == "toml_table" {
            "toml"
        } else {
            "json"
        };
        Ok((
            self.require_config_dir()?.join(plugin_cfg.map_path()),
            DocFormat::from_format_str(format_str),
        ))
    }
    fn plugin_array_path(&self) -> Result<PathBuf> {
        Ok(self
            .require_config_dir()?
            .join(self.mapping.plugin.array_path()))
    }
    fn is_plugin_dir_format(&self) -> bool {
        self.mapping.plugin.format == "directory"
    }
    /// Load a plugin doc for DocTree-based formats (not directory).
    /// Returns (doc, plugin_key, format_str) or None if not a DocTree format / file missing.
    fn load_plugin_doc_if_exists(&self) -> Option<(DocTree, &str, &str)> {
        let plugin_cfg = &self.mapping.plugin;
        match plugin_cfg.format.as_str() {
            "enabled_list" | "json_array" => {
                let doc = try_load_doc(
                    DocFormat::from_filename(self.mapping.plugin.array_path()),
                    &self.plugin_array_path().ok()?,
                )?;
                Some((doc, plugin_cfg.array_key(), &plugin_cfg.format))
            }
            "enabled_map" | "toml_table" => {
                let (config_path, fmt) = self.plugin_doc_config_path().ok()?;
                let doc = try_load_doc(fmt, &config_path)?;
                Some((doc, self.plugins_key(), &plugin_cfg.format))
            }
            _ => None,
        }
    }
    fn set_plugin_entry_in_doc(doc: &mut DocTree, plugins_key: &str, ref_name: &str) {
        doc.set_entry_bool(plugins_key, ref_name, true);
    }
    fn excluded_env_keys(&self) -> std::collections::HashSet<&str> {
        let mut keys = std::collections::HashSet::new();
        for em in &self.mapping.provider.env_mapping {
            keys.insert(em.api_key.as_str());
            if !em.base_url.is_empty() {
                keys.insert(em.base_url.as_str());
            }
            if !em.model.is_empty() {
                keys.insert(em.model.as_str());
            }
        }
        for key in &self.mapping.env.exclude_keys {
            keys.insert(key.as_str());
        }
        keys
    }
    fn read_tool_config_doc(&self, dir: &std::path::Path, primary_path: &str) -> Result<DocTree> {
        let primary = dir.join(primary_path);
        if self.mapping.mcp.jsonc || !self.mapping.mcp.fallback_paths.is_empty() {
            let mut candidates = vec![primary.clone()];
            for fb in &self.mapping.mcp.fallback_paths {
                candidates.push(dir.join(fb));
            }
            DocTree::load_with_options(
                DocFormat::from_filename(primary.to_string_lossy().as_ref()),
                &primary,
                self.mapping.mcp.jsonc,
                &candidates,
            )
        } else {
            DocTree::load(
                DocFormat::from_filename(primary.to_string_lossy().as_ref()),
                &primary,
            )
        }
    }
}

impl Adapter for GenericAdapter {
    fn tool_name(&self) -> &str {
        &self.mapping.tool.name
    }
    fn config_dir(&self) -> Option<PathBuf> {
        self.mapping.resolved_config_dir()
    }

    fn apply_defaults(
        &self,
        store: &TomlStore,
        should_apply: &dyn Fn(&str) -> bool,
        dry_run: bool,
    ) -> Result<usize> {
        let mut applied = 0;
        for kind in &self.mapping.capabilities.defaults {
            if !should_apply(kind) {
                continue;
            }
            match kind.as_str() {
                "provider" => {
                    if let Some(p) = load_default::<Provider>(
                        store,
                        "provider",
                        self.tool_name(),
                        resolve_provider,
                    )? {
                        applied += provider::write_provider(&self.mapping, &p, dry_run, false)?;
                    }
                }
                "env" | "mcp" => {
                    if let Some(name) = load_default_name(store, kind, self.tool_name()) {
                        applied += self.write_defaults(kind, store, &name, dry_run)?;
                    }
                }
                _ => {}
            }
        }
        Ok(applied)
    }

    fn apply_provider(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize> {
        let po = apply_profile_override(profile, self.tool_name());
        let provider_name = match &profile.providers.default {
            Some(p) => p,
            None => return Ok(0),
        };
        let raw: Provider = store.load_resource("provider", provider_name)?;
        let mut provider = resolve_provider(&raw, self.tool_name());
        if let Some(ref m) = po.default_model {
            provider.config.default_model = Some(m.clone());
        }
        provider::write_provider(&self.mapping, &provider, dry_run, true)
    }

    fn apply_mcp(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize> {
        if self.mapping.mcp.format.is_empty() {
            return Ok(0);
        }
        self.with_mcp_doc(dry_run, |doc| {
            let applied = self.apply_mcp_to_doc(store, profile, doc)?;
            if doc.format() != DocFormat::Toml {
                self.maybe_insert_schema(doc);
            }
            Ok(applied)
        })
    }

    fn apply_env(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize> {
        if !self.mapping.capabilities.env_enabled || profile.env.enabled.is_empty() {
            return Ok(0);
        }
        self.with_env_doc(dry_run, |doc| self.apply_env_to_doc(store, profile, doc))
    }

    fn apply_hook(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize> {
        if !self.mapping.capabilities.hook_enabled {
            return Ok(0);
        }
        let all_hooks = collect_all_hooks(store, &profile.hooks.enabled, self.tool_name());
        let hook_count = all_hooks.len();
        let dir = self.require_config_dir()?;
        let (config_path, _, is_toml) = self.hook_config_info(&dir);
        if is_toml {
            let section_key = self.mapping.hook.section_key.as_str();
            with_doc(&config_path, DocFormat::Toml, dry_run, |doc| {
                doc.set(section_key, DocValue::Array(vec![]));
                for hook in &all_hooks {
                    doc.push(section_key, hook_to_doc_entry(hook));
                }
                Ok(hook_count)
            })?;
            print_apply_status("hook", hook_count, &config_path, dry_run);
            Ok(hook_count)
        } else {
            let hook_fmt = DocFormat::from_filename(config_path.to_string_lossy().as_ref());
            let applied = with_doc(&config_path, hook_fmt, dry_run, |doc| {
                self.apply_hook_to_doc(store, profile, doc)
            })?;
            Ok(applied)
        }
    }

    fn apply_settings_batch(
        &self,
        store: &TomlStore,
        profile: &Profile,
        dry_run: bool,
        should_apply: &dyn Fn(&str) -> bool,
    ) -> Result<usize> {
        let dir = self.require_config_dir()?;
        let mut total = 0;
        if should_apply("mcp") && self.mapping.mcp.format == "toml" {
            total += self.apply_mcp(store, profile, dry_run)?;
        }
        if should_apply("hook")
            && self.mapping.capabilities.hook_enabled
            && self.mapping.hook.format == "toml_hooks"
        {
            total += self.apply_hook(store, profile, dry_run)?;
        }
        // MCP with separate JSON file (e.g. kimi's mcp.json) — write independently
        let mcp_in_separate_file = should_apply("mcp")
            && !self.mapping.mcp.format.is_empty()
            && self.mapping.mcp.format != "toml"
            && self.mapping.mcp.path() != self.mapping.settings_file();
        if mcp_in_separate_file {
            total +=
                self.with_mcp_doc(dry_run, |doc| self.apply_mcp_to_doc(store, profile, doc))?;
        }
        let need_mcp_json = should_apply("mcp")
            && !self.mapping.mcp.format.is_empty()
            && self.mapping.mcp.format != "toml"
            && self.mapping.mcp.path() == self.mapping.settings_file();
        let need_hook = should_apply("hook")
            && self.mapping.capabilities.hook_enabled
            && self.mapping.hook.format != "toml_hooks";
        let need_env = should_apply("env")
            && self.mapping.capabilities.env_enabled
            && !profile.env.enabled.is_empty();
        if !need_mcp_json && !need_hook && !need_env {
            return Ok(total);
        }
        let settings_path = self.settings_path_from(&dir);
        let settings_fmt = DocFormat::from_filename(self.mapping.settings_file());
        let mut doc = DocTree::load(settings_fmt, &settings_path)?;
        if need_mcp_json {
            total += self.apply_mcp_to_doc(store, profile, &mut doc)?;
        }
        if need_hook {
            total += self.apply_hook_to_doc(store, profile, &mut doc)?;
        }
        if need_env {
            total += self.apply_env_to_doc(store, profile, &mut doc)?;
        }
        if total > 0 && !dry_run {
            self.maybe_insert_schema(&mut doc);
            ensure_dir(&dir)?;
            doc.save_to(&settings_path)?;
            println!("  settings batch ({}) → {}", total, settings_path.display());
        }
        Ok(total)
    }

    fn apply_skill(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize> {
        if self.mapping.capabilities.skill_disabled() || profile.skills.enabled.is_empty() {
            return Ok(0);
        }
        let skills_dir = self.skills_dir()?;
        let tool_name = self.mapping.tool.name.as_str();
        if !dry_run {
            clean_stale_dir_entries(&skills_dir, &profile.skills.enabled.iter().collect());
        }
        let mut applied = 0;
        for name in &profile.skills.enabled {
            let skill: Skill = match load_or_skip(store, "skill", name) {
                Some(s) => s,
                None => continue,
            };
            if let Some(o) = skill.tool.get(tool_name) {
                if o.disabled {
                    if !dry_run {
                        let t = skills_dir.join(name);
                        let r = if t.is_symlink() {
                            if t.is_dir() {
                                std::fs::remove_dir(&t)
                            } else {
                                std::fs::remove_file(&t)
                            }
                        } else if t.is_dir() {
                            std::fs::remove_dir_all(&t)
                        } else {
                            continue;
                        };
                        if let Err(e) = r {
                            crate::cli::output::warn(&format!("failed to remove '{}': {}", t.display(), e));
                        }
                    }
                    continue;
                }
            }
            let target = skills_dir.join(name);
            let linked = if self.mapping.capabilities.skill_mode == "full" {
                link_skill(&skill, &target, store, dry_run)?
            } else {
                let cache_dir = store.root().join("cache").join("skills").join(name);
                if cache_dir.exists() {
                    if !dry_run {
                        ensure_dir(&skills_dir)?;
                        apply_skill_link(&target, &cache_dir, &skill.config.install_method)?;
                    }
                    true
                } else {
                    println!("  skill '{}' not installed yet.", name);
                    false
                }
            };
            if linked {
                applied += 1;
            }
        }
        print_apply_status("skill", applied, &skills_dir, dry_run);
        Ok(applied)
    }

    fn apply_prompt(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize> {
        let prompt_name = match &profile.prompts.system {
            Some(p) => p,
            None => return Ok(0),
        };
        let raw: Prompt = store.load_resource("prompt", prompt_name)?;
        let prompt = resolve_prompt(&raw, self.tool_name());
        let dir = self.require_config_dir()?;
        let prompt_path = self.prompt_path_from(&dir);
        if dry_run {
            println!("  [dry-run] prompt → {}", prompt_path.display());
        } else {
            ensure_dir(&dir)?;
            if prompt_path.exists() {
                std::fs::copy(&prompt_path, prompt_path.with_extension("md.bak"))?;
            }
            std::fs::write(
                &prompt_path,
                format!(
                    "<!-- managed by vcc (profile: {}) -->\n{}",
                    profile.name, prompt.config.content
                ),
            )?;
            println!("  prompt → {}", prompt_path.display());
        }
        Ok(1)
    }

    fn apply_plugin(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize> {
        self.plugin_doc_op(store, &profile.plugins.enabled, dry_run, true, false)
    }

    fn apply_agent(&self, store: &TomlStore, profile: &Profile, dry_run: bool) -> Result<usize> {
        let agent_path = match &self.mapping.agent.path {
            Some(p) => p,
            None => return Ok(0),
        };
        if profile.agents.enabled.is_empty() {
            return Ok(0);
        }
        let dir = self.require_config_dir()?;
        let agents_dir = dir.join(agent_path);
        let (is_yaml, ext) = self.agent_format_info();
        if !dry_run && agents_dir.is_dir() {
            let enabled_set: std::collections::HashSet<&String> =
                profile.agents.enabled.iter().collect();
            if let Ok(entries) = std::fs::read_dir(&agents_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let should_remove = if path.extension().map_or(true, |e| e != ext) {
                        if is_yaml && path.extension().is_some_and(|e| e == "md") {
                            path.file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .is_some_and(|s| {
                                    s.ends_with("-system")
                                        && enabled_set.contains(&s.replace("-system", ""))
                                })
                        } else {
                            false
                        }
                    } else {
                        path.file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .map_or(true, |n| !enabled_set.contains(&n))
                    };
                    if should_remove {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
        let applied = load_each::<Agent, _>(store, "agent", &profile.agents.enabled, |agent| {
            if let Err(e) = write_agent_file(&agent, &agents_dir, is_yaml, ext, dry_run) {
                crate::cli::output::warn(&format!("failed to write agent '{}': {}", agent.name, e));
            }
        });
        print_apply_status("agent", applied, &agents_dir, dry_run);
        Ok(applied)
    }

    fn sync(&self, store: &TomlStore, dry_run: bool) -> Result<SyncResult> {
        self.sync_impl(store, dry_run)
    }
    fn inspect(&self) -> Result<InspectResult> {
        self.inspect_impl()
    }

    fn add_resource(
        &self,
        kind: &str,
        store: &TomlStore,
        names: &[String],
        dry_run: bool,
    ) -> Result<usize> {
        match kind {
            "mcp" => self.upsert_mcp(store, names, dry_run, false),
            "hook" => self.add_hook_to_config(store, names, dry_run),
            "plugin" => self.plugin_doc_op(store, names, dry_run, false, false),
            "env" => {
                if !self.mapping.capabilities.env_enabled {
                    return Ok(0);
                }
                let section_key = self.mapping.env.section_key();
                self.with_env_doc(dry_run, |doc| {
                    Ok(crud::env_upsert(
                        doc,
                        section_key,
                        store,
                        names,
                        self.tool_name(),
                        resolve_env,
                        None,
                    ))
                })
            }
            "skill" => {
                if self.mapping.capabilities.skill_disabled() {
                    return Ok(0);
                }
                let skills_dir = self.skills_dir()?;
                let tool_name = self.mapping.tool.name.as_str();
                let mut applied = 0;
                load_each::<Skill, _>(store, "skill", names, |skill| {
                    if skill.tool.get(tool_name).is_some_and(|o| o.disabled) {
                        return;
                    }
                    if link_skill(&skill, &skills_dir.join(&skill.name), store, dry_run)
                        .unwrap_or(false)
                    {
                        applied += 1;
                    }
                });
                if !dry_run && applied > 0 {
                    println!("  skill (+{}) → {}", applied, skills_dir.display());
                }
                Ok(applied)
            }
            "agent" => {
                let agents_dir = self.agents_dir()?;
                let (is_yaml, ext) = self.agent_format_info();
                let applied = load_each::<Agent, _>(store, "agent", names, |agent| {
                    if let Err(e) = write_agent_file(&agent, &agents_dir, is_yaml, ext, dry_run) {
                        crate::cli::output::warn(&format!("failed to write agent '{}': {}", agent.name, e));
                    }
                });
                if !dry_run && applied > 0 {
                    println!("  agent (+{}) → {}", applied, agents_dir.display());
                }
                Ok(applied)
            }
            "prompt" => {
                let dir = self.require_config_dir()?;
                let prompt_path = self.prompt_path_from(&dir);
                let tool_name = self.tool_name();
                let applied = load_each::<Prompt, _>(store, "prompt", names, |raw| {
                    let prompt = resolve_prompt(&raw, tool_name);
                    if !dry_run {
                        if let Err(e) = ensure_dir(&dir) {
                            eprintln!(
                                "warning: failed to create directory {}: {}",
                                dir.display(),
                                e
                            );
                        }
                        if let Err(e) = std::fs::write(&prompt_path, &prompt.config.content) {
                            eprintln!(
                                "warning: failed to write prompt '{}': {}",
                                prompt_path.display(),
                                e
                            );
                        }
                    }
                });
                Ok(applied)
            }
            "provider" => {
                let tool_name = self.tool_name();
                let mut applied = 0;
                load_each::<Provider, _>(store, "provider", names, |raw| {
                    let provider = resolve_provider(&raw, tool_name);
                    match provider::write_provider(&self.mapping, &provider, dry_run, true) {
                        Ok(n) => applied += n,
                        Err(e) => eprintln!(
                            "warning: failed to write provider '{}': {}",
                            provider.name, e
                        ),
                    }
                });
                Ok(applied)
            }
            _ => Ok(0),
        }
    }
    fn remove_resource(
        &self,
        kind: &str,
        store: &TomlStore,
        names: &[String],
        dry_run: bool,
    ) -> Result<usize> {
        match kind {
            "mcp" => {
                if self.mapping.mcp.format.is_empty() {
                    return Ok(0);
                }
                self.with_mcp_doc(dry_run, |doc| {
                    Ok(crud::object_map_remove(
                        doc,
                        &self.mapping.mcp.servers_key,
                        names,
                    ))
                })
            }
            "hook" => self.remove_hook_from_config(names, dry_run),
            "plugin" => self.remove_plugin_from_config(names, dry_run),
            "env" => {
                if !self.mapping.capabilities.env_enabled {
                    return Ok(0);
                }
                let section_key = self.mapping.env.section_key();
                self.with_env_doc(dry_run, |doc| {
                    Ok(crud::env_remove(
                        doc,
                        section_key,
                        store,
                        names,
                        self.tool_name(),
                        resolve_env,
                    ))
                })
            }
            "skill" => {
                let skills_dir = self.skills_dir()?;
                let removed = remove_dir_entries(&skills_dir, names, dry_run, true)?;
                if !dry_run && removed > 0 {
                    println!("  skill (-{}) → {}", removed, skills_dir.display());
                }
                Ok(removed)
            }
            "agent" => {
                let agents_dir = self.agents_dir()?;
                let (is_yaml, ext) = self.agent_format_info();
                let targets: Vec<std::path::PathBuf> = names
                    .iter()
                    .flat_map(|n| {
                        let mut files = vec![agents_dir.join(format!("{}.{}", n, ext))];
                        if is_yaml {
                            files.push(agents_dir.join(format!("{}-system.md", n)));
                        }
                        files
                    })
                    .filter(|p| p.exists())
                    .collect();
                let removed = remove_files(&targets, dry_run)?;
                if !dry_run && removed > 0 {
                    println!("  agent (-{}) → {}", removed, agents_dir.display());
                }
                Ok(removed)
            }
            "prompt" => Ok(0),   // single file — no-op for now
            "provider" => Ok(0), // provider removal not supported via incremental apply
            _ => Ok(0),
        }
    }
    fn toggle_resource(
        &self,
        kind: &str,
        enable: bool,
        store: &TomlStore,
        names: &[String],
        dry_run: bool,
    ) -> Result<usize> {
        match kind {
            "mcp" | "plugin" => {
                if enable {
                    if kind == "mcp" {
                        self.upsert_mcp(store, names, dry_run, true)
                    } else {
                        self.plugin_doc_op(store, names, dry_run, false, true)
                    }
                } else {
                    self.disable_in_doc(kind, names, dry_run)
                }
            }
            _ => Ok(0),
        }
    }
}

impl GenericAdapter {
    fn apply_mcp_to_doc(
        &self,
        store: &TomlStore,
        profile: &Profile,
        doc: &mut DocTree,
    ) -> Result<usize> {
        let servers_key = &self.mapping.mcp.servers_key;
        doc.clear_section(servers_key);
        let mapping = &self.mapping.mcp;
        let tool_name = self.tool_name();
        Ok(load_each::<McpServer, _>(
            store,
            "mcp",
            &profile.mcp_servers.enabled,
            |raw| {
                insert_mcp_to_doc(
                    doc,
                    servers_key,
                    mapping,
                    &raw.name,
                    &resolve_mcp(&raw, tool_name),
                    tool_name,
                );
            },
        ))
    }

    fn apply_hook_to_doc(
        &self,
        store: &TomlStore,
        profile: &Profile,
        doc: &mut DocTree,
    ) -> Result<usize> {
        let all_hooks = collect_all_hooks(store, &profile.hooks.enabled, self.tool_name());
        with_settings_json_mut(doc, |json| {
            for event in &self.mapping.hook.events {
                if let Some(obj) = json.as_object_mut() {
                    obj.remove(event.as_str());
                }
            }
            let obj = json
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("settings is not an object"))?;
            let mut applied = 0;
            for hook in &all_hooks {
                self.add_hook_entry(obj, hook);
                applied += 1;
            }
            Ok(applied)
        })
    }

    fn apply_env_to_doc(
        &self,
        store: &TomlStore,
        profile: &Profile,
        doc: &mut DocTree,
    ) -> Result<usize> {
        let po = apply_profile_override(profile, self.tool_name());
        let extra = if po.extra_env.is_empty() {
            None
        } else {
            Some(&po.extra_env)
        };
        Ok(set_env_vars_in_doc(
            doc,
            self.mapping.env.section_key(),
            store,
            &profile.env.enabled,
            self.tool_name(),
            resolve_env,
            extra,
        ))
    }

    fn write_defaults(
        &self,
        kind: &str,
        store: &TomlStore,
        name: &str,
        dry_run: bool,
    ) -> Result<usize> {
        match kind {
            "env" => {
                if !self.mapping.capabilities.env_enabled {
                    return Ok(0);
                }
                let section_key = self.mapping.env.section_key();
                self.with_env_doc(dry_run, |doc| {
                    Ok(set_env_vars_in_doc(
                        doc,
                        section_key,
                        store,
                        &[name.to_string()],
                        self.tool_name(),
                        resolve_env,
                        None,
                    ))
                })
            }
            "mcp" => {
                let raw: McpServer = store.load_resource("mcp", name)?;
                let mcp = resolve_mcp(&raw, self.tool_name());
                self.with_mcp_doc(dry_run, |doc| {
                    insert_mcp_to_doc(
                        doc,
                        &self.mapping.mcp.servers_key,
                        &self.mapping.mcp,
                        &mcp.name,
                        &mcp,
                        self.tool_name(),
                    );
                    Ok(1)
                })
            }
            _ => Ok(0),
        }
    }

    fn add_hook_entry(
        &self,
        hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
        hook: &Hook,
    ) {
        let matcher = if hook.config.matcher.is_empty() {
            ".*".to_string()
        } else {
            hook.config.matcher.clone()
        };
        let event_entry = hooks_obj
            .entry(hook.config.event.clone())
            .or_insert_with(|| serde_json::json!([]));
        let event_arr = match event_entry.as_array_mut() {
            Some(arr) => arr,
            None => {
                *event_entry = serde_json::json!([]);
                // SAFETY: we just set event_entry to a JSON array
                event_entry.as_array_mut().expect("just set to array")
            }
        };
        let command_key = &self.mapping.hook.command_key;
        let mut hook_json =
            serde_json::json!({ "type": "command", "timeout": hook.config.timeout });
        if let Some(map) = hook_json.as_object_mut() {
            map.insert(
                command_key.clone(),
                serde_json::Value::String(hook.config.command.clone()),
            );
        }
        if let Some(o) = hook.tool.get(self.tool_name()) {
            if !o.extra.is_empty() {
                if let Some(map) = hook_json.as_object_mut() {
                    for (k, v) in &o.extra {
                        map.insert(k.clone(), crate::adapter::toml_to_json_value(v));
                    }
                }
            }
        }
        let matcher_key = &self.mapping.hook.matcher_key;
        let hooks_key = &self.mapping.hook.hooks_key;
        for entry in event_arr.iter_mut() {
            if entry.get(matcher_key).and_then(|v| v.as_str()) == Some(&matcher) {
                if let Some(hooks) = entry.get_mut(hooks_key).and_then(|v| v.as_array_mut()) {
                    hooks.push(hook_json);
                }
                return;
            }
        }
        let mut group = serde_json::Map::new();
        group.insert(matcher_key.clone(), serde_json::Value::String(matcher));
        group.insert(hooks_key.clone(), serde_json::Value::Array(vec![hook_json]));
        event_arr.push(serde_json::Value::Object(group));
    }

    fn upsert_mcp(
        &self,
        store: &TomlStore,
        names: &[String],
        dry_run: bool,
        enable_mode: bool,
    ) -> Result<usize> {
        if self.mapping.mcp.format.is_empty() {
            return Ok(0);
        }
        let servers_key = self.mapping.mcp.servers_key.clone();
        let toggle_key = self.mapping.mcp.toggle_key().to_string();
        let uses_enabled = self.mapping.mcp.uses_enabled_semantic();
        self.with_mcp_doc(dry_run, |doc| {
            if enable_mode {
                return Ok(crud::object_map_enable(
                    doc,
                    &servers_key,
                    &toggle_key,
                    uses_enabled,
                    names,
                ));
            }
            doc.ensure_object(&servers_key);
            let mapping = &self.mapping.mcp;
            let tool_name = self.tool_name();
            let applied = load_each::<McpServer, _>(store, "mcp", names, |raw| {
                insert_mcp_to_doc(
                    doc,
                    &servers_key,
                    mapping,
                    &raw.name,
                    &resolve_mcp(&raw, tool_name),
                    tool_name,
                );
                // Ensure proper toggle state for newly inserted entries:
                // - "disabled" semantic: remove disabled key only if tool_extra didn't explicitly set it
                // - "enabled" semantic: set enabled=true only if tool_extra didn't explicitly set it
                if let Some(DocValue::Object(map)) = doc.get_entry_mut(&servers_key, &raw.name) {
                    let tool_extra_has_toggle = raw
                        .tool
                        .get(tool_name)
                        .map(|o| o.extra.contains_key(&toggle_key))
                        .unwrap_or(false)
                        || raw.config.extra.contains_key(&toggle_key);
                    if uses_enabled {
                        if !tool_extra_has_toggle {
                            map.insert(toggle_key.clone(), DocValue::Bool(true));
                        }
                    } else if !tool_extra_has_toggle {
                        map.remove(&toggle_key);
                    }
                }
            });
            Ok(applied)
        })
    }

    fn add_hook_to_config(
        &self,
        store: &TomlStore,
        names: &[String],
        dry_run: bool,
    ) -> Result<usize> {
        if !self.mapping.capabilities.hook_enabled {
            return Ok(0);
        }
        self.with_hook_doc(dry_run, |doc, is_toml| {
            let tool_name = self.tool_name();
            if is_toml {
                let section_key = self.mapping.hook.section_key.as_str();
                Ok(load_each::<Hook, _>(store, "hook", names, |raw| {
                    doc.push(
                        section_key,
                        hook_to_doc_entry(&resolve_hook(&raw, tool_name)),
                    );
                }))
            } else {
                with_settings_json_mut(doc, |json| {
                    let obj = json
                        .as_object_mut()
                        .ok_or_else(|| anyhow::anyhow!("settings is not an object"))?;
                    Ok(load_each::<Hook, _>(store, "hook", names, |raw| {
                        self.add_hook_entry(obj, &resolve_hook(&raw, tool_name));
                    }))
                })
            }
        })
    }

    fn remove_hook_from_config(&self, names: &[String], dry_run: bool) -> Result<usize> {
        if !self.mapping.capabilities.hook_enabled {
            return Ok(0);
        }
        let dir = self.require_config_dir()?;
        let (config_path, _, is_toml) = self.hook_config_info(&dir);
        if !config_path.exists() {
            return Ok(0);
        }
        let names_set: std::collections::HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
        let cmd_matches = |cmd: &str| {
            let cmd_name = cmd.split_whitespace().next().unwrap_or(cmd);
            names_set.iter().any(|n| *n == cmd_name || cmd.contains(n))
        };
        if is_toml {
            return with_doc(&config_path, DocFormat::Toml, dry_run, |doc| {
                let section_key = self.mapping.hook.section_key.as_str();
                let before = doc
                    .get(section_key)
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                doc.retain_in_array(section_key, |entry| {
                    !cmd_matches(entry.get_path_str("command").unwrap_or(""))
                });
                Ok(before
                    - doc
                        .get(section_key)
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0))
            });
        }
        with_doc(
            &config_path,
            DocFormat::from_filename(config_path.to_string_lossy().as_ref()),
            dry_run,
            |doc| {
                with_settings_json_mut(doc, |json| {
                    let mut removed = 0;
                    let hooks_key = &self.mapping.hook.hooks_key;
                    let command_key = &self.mapping.hook.command_key;
                    for event_name in &self.mapping.hook.events {
                        let event_arr =
                            match json.get_mut(event_name).and_then(|v| v.as_array_mut()) {
                                Some(a) => a,
                                None => continue,
                            };
                        for entry in event_arr.iter_mut() {
                            if let Some(hooks_arr) =
                                entry.get_mut(hooks_key).and_then(|v| v.as_array_mut())
                            {
                                let orig_len = hooks_arr.len();
                                hooks_arr.retain(|h| {
                                    !cmd_matches(
                                        h.get(command_key).and_then(|v| v.as_str()).unwrap_or(""),
                                    )
                                });
                                removed += orig_len - hooks_arr.len();
                            }
                        }
                        if let Some(ea) = json.get_mut(event_name).and_then(|v| v.as_array_mut()) {
                            ea.retain(|e| {
                                e.get(hooks_key)
                                    .and_then(|v| v.as_array())
                                    .is_some_and(|a| !a.is_empty())
                            });
                        }
                    }
                    Ok(removed)
                })
            },
        )
    }
}

impl GenericAdapter {
    fn disable_in_doc(&self, kind: &str, names: &[String], dry_run: bool) -> Result<usize> {
        match kind {
            "mcp" => {
                if self.mapping.mcp.format.is_empty() {
                    return Ok(0);
                }
                self.with_mcp_doc(dry_run, |doc| {
                    Ok(crud::object_map_disable(
                        doc,
                        &self.mapping.mcp.servers_key,
                        self.mapping.mcp.toggle_key(),
                        self.mapping.mcp.uses_enabled_semantic(),
                        names,
                    ))
                })
            }
            "plugin" => {
                let plugin_cfg = &self.mapping.plugin;
                if plugin_cfg.format.is_empty() {
                    return Ok(0);
                }
                // For array formats (enabled_list, json_array), disable = remove from array
                if matches!(plugin_cfg.format.as_str(), "enabled_list" | "json_array") {
                    let config_path = self.plugin_array_path()?;
                    if !config_path.exists() {
                        return Ok(0);
                    }
                    let pk = plugin_cfg.array_key();
                    let fmt = DocFormat::from_filename(config_path.to_string_lossy().as_ref());
                    return with_doc(&config_path, fmt, dry_run, |doc| {
                        let mut removed = 0;
                        for name in names {
                            let rn = name.to_string();
                            let before = doc
                                .get(pk)
                                .and_then(|v| v.as_array())
                                .map_or(0, |a| a.len());
                            doc.retain_in_array(pk, |item| {
                                // Keep items that don't match the name
                                item.as_str().map_or(true, |s| {
                                    s != rn && !s.starts_with(&format!("{}@", rn))
                                })
                            });
                            let after = doc
                                .get(pk)
                                .and_then(|v| v.as_array())
                                .map_or(0, |a| a.len());
                            removed += before.saturating_sub(after);
                        }
                        Ok(removed)
                    });
                }
                let (config_path, format) = self.plugin_doc_config_path()?;
                if !config_path.exists() {
                    return Ok(0);
                }
                with_doc(&config_path, format, dry_run, |doc| {
                    let pk = self.plugins_key();
                    // For enabled_map format (e.g. claude's enabledPlugins),
                    // each entry is a bool: disable = set to false
                    if self.mapping.plugin.format == "enabled_map" {
                        Ok(names
                            .iter()
                            .filter(|n| {
                                let key = doc.find_matching_key(pk, n);
                                if let Some(k) = &key {
                                    doc.set(&format!("{}.{}", pk, k), DocValue::Bool(false));
                                }
                                key.is_some()
                            })
                            .count())
                    } else {
                        let dk = self.mapping.plugin.disabled_key();
                        Ok(names
                            .iter()
                            .filter(|n| {
                                if let Some(DocValue::Object(map)) = doc.get_entry_mut(pk, n) {
                                    map.insert(dk.to_string(), DocValue::Bool(true));
                                }
                                doc.find_matching_key(pk, n).is_some()
                            })
                            .count())
                    }
                })
            }
            _ => Ok(0),
        }
    }

    // ── sync methods ──

    fn sync_impl(&self, store: &TomlStore, dry_run: bool) -> Result<SyncResult> {
        let dir = match self.config_dir() {
            Some(d) if d.exists() => d,
            _ => return Ok(SyncResult::default()),
        };
        let mut result = SyncResult::default();
        result.merge(provider::sync_provider(
            store,
            &self.mapping,
            &dir,
            dry_run,
        )?);
        if !self.mapping.mcp.format.is_empty() {
            if let Ok((config_path, _)) = self.mcp_config_path() {
                if config_path.exists() {
                    result.merge(self.sync_mcp(store, dry_run)?);
                }
            }
        }
        if self.mapping.capabilities.hook_enabled {
            let hook_path = if self.mapping.hook.format == "toml_hooks" {
                dir.join(self.mapping.hook.path())
            } else {
                self.settings_path_from(&dir)
            };
            if hook_path.exists() {
                result.merge(self.sync_hooks(store, &dir, dry_run)?);
            }
        }
        if self.mapping.capabilities.env_enabled && self.settings_path_from(&dir).exists() {
            result.merge(self.sync_env(store, &dir, dry_run)?);
        }
        result.merge(self.sync_prompt(store, &dir, dry_run)?);
        if self.mapping.agent.path.is_some() {
            result.merge(self.sync_agent(store, &dir, dry_run)?);
        }
        if !self.mapping.plugin.format.is_empty() {
            result.merge(self.sync_plugin(store, &dir, dry_run)?);
        }
        Ok(result)
    }

    fn sync_mcp(&self, store: &TomlStore, dry_run: bool) -> Result<SyncResult> {
        let (config_path, _) = self.mcp_config_path()?;
        if !config_path.exists() {
            return Ok(SyncResult::default());
        }
        let format = DocFormat::from_format_str(&self.mapping.mcp.format);
        let doc = DocTree::load(format, &config_path)?;
        let servers_key = &self.mapping.mcp.servers_key;
        let entries = match doc.entries(servers_key) {
            Some(e) => e,
            None => return Ok(SyncResult::default()),
        };
        let mut result = SyncResult::default();
        for (name, _) in entries {
            let mut incoming = match mcp_from_doc(
                &doc,
                servers_key,
                &self.mapping.mcp,
                &name,
                self.tool_name(),
            ) {
                Some(m) => m,
                None => continue,
            };
            if !store.resource_exists("mcp", &name) {
                save_if_ok(&incoming, store, dry_run);
                result.created.push(SyncItem::new("mcp", &name));
                continue;
            }
            let existing: McpServer = match store.load_resource("mcp", &name) {
                Ok(e) => e,
                Err(_) => {
                    result.skipped.push(SyncItem::new("mcp", &name));
                    continue;
                }
            };
            // When a tool type (e.g. "remote") maps to multiple VCC types (sse/streamable-http),
            // the sync may resolve to a different server_type than what's in the registry.
            // Normalize: if both are url-based types, use the existing server_type so the
            // subset comparison isn't thrown off by this ambiguous mapping.
            let is_url = |t: &str| self.mapping.mcp.is_url_type(t);
            if is_url(&existing.config.server_type)
                && is_url(&incoming.config.server_type)
                && existing.config.server_type != incoming.config.server_type
            {
                incoming.config.server_type = existing.config.server_type.clone();
            }
            let ej = serde_json::to_value(&existing).unwrap_or_default();
            let nj = serde_json::to_value(&incoming).unwrap_or_default();
            // Compare only hash-relevant fields (type, config), ignoring id/name/metadata/tool
            // Use subset check on config: if incoming's config keys are all present in existing with equal values,
            // then the sync adds no new information and should be skipped.
            let incoming_config = nj.get("config");
            let existing_config = ej.get("config");
            let config_is_subset = incoming_config.map_or(true, |ic| {
                existing_config.is_some_and(|ec| json_is_subset(ic, ec))
            });
            if config_is_subset {
                // Incoming hash matches or is a subset of existing — no meaningful change, skip
                result.skipped.push(SyncItem::new("mcp", &name));
            } else {
                // Incoming has new/changed fields — merge into existing to preserve vcc-managed fields
                let merged = crate::cli::import::merge_mcp(&existing, &incoming);
                save_if_ok(&merged, store, dry_run);
                result.updated.push(SyncItem::new("mcp", &name));
            }
        }
        Ok(result)
    }

    fn sync_hooks(
        &self,
        store: &TomlStore,
        dir: &std::path::Path,
        dry_run: bool,
    ) -> Result<SyncResult> {
        let (config_path, format, is_toml) = self.hook_config_info(dir);
        Ok(with_doc_if_exists(
            format,
            &config_path,
            SyncResult::default(),
            |doc| {
                let mut result = SyncResult::default();
                let tool_name = &self.mapping.tool.name;
                let section_key = self.mapping.hook.section_key.as_str();
                for_each_hook_entry(
                    &doc,
                    is_toml,
                    section_key,
                    &self.mapping.hook.events,
                    &self.mapping.hook.command_key,
                    &self.mapping.hook.matcher_key,
                    &self.mapping.hook.hooks_key,
                    |he| {
                        let hook = build_synced_hook(
                            he.event,
                            he.matcher,
                            he.command.to_string(),
                            he.timeout,
                            he.index,
                            tool_name,
                            he.tool_extra,
                        );
                        sync_entry_skip(store, "hook", &hook, dry_run, &mut result);
                    },
                );
                result
            },
        ))
    }

    fn sync_env(
        &self,
        store: &TomlStore,
        dir: &std::path::Path,
        dry_run: bool,
    ) -> Result<SyncResult> {
        let settings_path = self.settings_path_from(dir);
        if !settings_path.exists() {
            return Ok(SyncResult::default());
        }
        let env_fmt = DocFormat::from_format_str(self.mapping.env.format_str());
        let doc = match DocTree::load(env_fmt, &settings_path) {
            Ok(d) => d,
            Err(_) => return Ok(SyncResult::default()),
        };
        let excluded_keys = self.excluded_env_keys();
        let section_key = self.mapping.env.section_key();
        let vars: HashMap<String, String> = doc
            .entries(section_key)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(k, _)| !excluded_keys.contains(k.as_str()))
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        if vars.is_empty() {
            return Ok(SyncResult::default());
        }
        let name = format!("{}-env", self.mapping.tool.name);
        let mut env = Env::new_with_name(&name);
        env.config.vars = vars;
        env.metadata = sync_metadata(&self.mapping.tool.name);
        let mut result = SyncResult::default();
        sync_entry_merge(
            store,
            "env",
            &env,
            dry_run,
            &mut result,
            |e: &Env, n: &Env| e.config.vars == n.config.vars,
        );
        Ok(result)
    }

    fn sync_prompt(
        &self,
        store: &TomlStore,
        dir: &std::path::Path,
        dry_run: bool,
    ) -> Result<SyncResult> {
        let prompt_path = self.prompt_path_from(dir);
        if !prompt_path.exists() {
            return Ok(SyncResult::default());
        }
        let raw = std::fs::read_to_string(&prompt_path)?;
        let content = raw
            .lines()
            .filter(|l| !l.starts_with("<!-- managed by vcc"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if content.is_empty() {
            return Ok(SyncResult::default());
        }
        let default_name = format!("{}-system", self.mapping.tool.name);
        let name = self
            .mapping
            .prompt
            .sync_name
            .as_deref()
            .unwrap_or(&default_name);
        let mut prompt = Prompt::new_with_name(name);
        prompt.config.content = content;
        prompt.metadata = sync_metadata(&self.mapping.tool.name);
        let mut result = SyncResult::default();
        sync_entry_merge(
            store,
            "prompt",
            &prompt,
            dry_run,
            &mut result,
            |e: &Prompt, n: &Prompt| e.config.content == n.config.content,
        );
        Ok(result)
    }

    fn sync_agent(
        &self,
        store: &TomlStore,
        dir: &std::path::Path,
        dry_run: bool,
    ) -> Result<SyncResult> {
        let agent_dir = dir.join(self.mapping.agent.path());
        if !agent_dir.is_dir() {
            return Ok(SyncResult::default());
        }
        let (is_yaml, ext) = self.agent_format_info();
        let mut result = SyncResult::default();
        for entry in std::fs::read_dir(&agent_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(true, |e| e != ext) {
                continue;
            }
            let raw = std::fs::read_to_string(&path)?;
            let fallback_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");
            let agent = if is_yaml {
                match parse_agent_yaml(&raw, fallback_name) {
                    Some(a) => a,
                    None => continue,
                }
            } else {
                match parse_agent_markdown(&raw, fallback_name, &self.mapping.tool.name) {
                    Some(a) => a,
                    None => continue,
                }
            };
            sync_entry_merge(
                store,
                "agent",
                &agent,
                dry_run,
                &mut result,
                |e: &Agent, n: &Agent| {
                    e.config.model == n.config.model
                        && e.config.tools.enabled == n.config.tools.enabled
                        && e.config.tools.disabled == n.config.tools.disabled
                        && e.config.content == n.config.content
                },
            );
        }
        Ok(result)
    }

    fn sync_plugin(
        &self,
        store: &TomlStore,
        dir: &std::path::Path,
        dry_run: bool,
    ) -> Result<SyncResult> {
        let plugin_cfg = &self.mapping.plugin;
        let tool_name = &self.mapping.tool.name;
        let mut result = SyncResult::default();
        if self.is_plugin_dir_format() {
            let pd = self.plugin_install_dir(dir);
            if pd.is_dir() {
                for entry in std::fs::read_dir(&pd)?.flatten() {
                    if entry.file_type()?.is_dir() {
                        self.sync_plugin_entry(
                            store,
                            &entry.file_name().to_string_lossy(),
                            None,
                            None,
                            tool_name,
                            dry_run,
                            &mut result,
                        );
                    }
                }
            }
            return Ok(result);
        }
        // For json_array format, use read_tool_config_doc which handles jsonc/fallbacks
        if plugin_cfg.format == "json_array" {
            let doc = match self.read_tool_config_doc(dir, plugin_cfg.array_path()) {
                Ok(d) if !d.is_empty() => d,
                _ => return Ok(result),
            };
            let pk = plugin_cfg.array_key();
            for (name, mp, opts) in crud::plugin_refs_from_doc(&doc, &plugin_cfg.format, pk) {
                self.sync_plugin_entry(
                    store,
                    &name,
                    mp,
                    opts.as_ref(),
                    tool_name,
                    dry_run,
                    &mut result,
                );
            }
            return Ok(result);
        }
        if let Some((doc, pk, fmt)) = self.load_plugin_doc_if_exists() {
            for (name, mp, _) in crud::plugin_refs_from_doc(&doc, fmt, pk) {
                self.sync_plugin_entry(store, &name, mp, None, tool_name, dry_run, &mut result);
            }
        }
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    fn sync_plugin_entry(
        &self,
        store: &TomlStore,
        name: &str,
        marketplace: Option<String>,
        plugin_options: Option<&serde_json::Map<String, serde_json::Value>>,
        tool_name: &str,
        dry_run: bool,
        result: &mut SyncResult,
    ) {
        if store.resource_exists("plugin", name) {
            result.skipped.push(SyncItem::new("plugin", name));
            return;
        }
        let source = if marketplace.is_some() {
            "marketplace"
        } else if plugin_options.is_some() {
            "local"
        } else {
            "unknown"
        };
        let repo = plugin_options.and_then(|o| {
            o.get("repo")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });
        let mut plugin = Plugin::new_with_name(name);
        plugin.config.source = source.to_string();
        plugin.config.repo = repo;
        plugin.config.marketplace = marketplace;
        plugin.config.install_method = self.mapping.plugin.install_method.clone();
        plugin.config.format = Some(tool_name.to_string());
        plugin.metadata = sync_metadata(tool_name);
        sync_entry_skip(store, "plugin", &plugin, dry_run, result);
    }

    // ── inspect methods ──

    fn inspect_impl(&self) -> Result<InspectResult> {
        let dir = match self.config_dir() {
            Some(d) if d.exists() => d,
            _ => {
                return Ok(InspectResult {
                    tool: self.tool_name().to_string(),
                    config_dir: self.config_dir().map(|p| p.to_string_lossy().to_string()),
                    sections: Vec::new(),
                })
            }
        };
        let mut sections: Vec<InspectSection> = Vec::new();
        let mut add = |kind: &str, items: Vec<InspectItem>| {
            if !items.is_empty() {
                sections.push(InspectSection {
                    kind: kind.to_string(),
                    items,
                });
            }
        };
        add("mcp", self.inspect_mcp(&dir)?);
        if !self.mapping.plugin.format.is_empty() {
            add("plugin", self.inspect_plugin(&dir)?);
        }
        if self.mapping.capabilities.hook_enabled {
            add("hook", self.inspect_hook(&dir)?);
        }
        if self.mapping.capabilities.env_enabled {
            add("env", self.inspect_env(&dir));
        }
        if !self.mapping.capabilities.skill_disabled() {
            add("skill", self.inspect_skill(&dir)?);
        }
        if self.mapping.agent.path.is_some() {
            add("agent", self.inspect_agent(&dir)?);
        }
        add("prompt", self.inspect_prompt(&dir));
        if !self.mapping.provider.format.is_empty() {
            add("provider", self.inspect_provider(&dir)?);
        }
        Ok(InspectResult {
            tool: self.tool_name().to_string(),
            config_dir: Some(dir.to_string_lossy().to_string()),
            sections,
        })
    }

    fn inspect_mcp(&self, _dir: &std::path::Path) -> Result<Vec<InspectItem>> {
        if self.mapping.mcp.format.is_empty() {
            return Ok(Vec::new());
        }
        let (config_path, format) = self.mcp_config_path()?;
        Ok(with_doc_if_exists(
            format,
            &config_path,
            Vec::new(),
            |doc| {
                let entries = match doc.entries(&self.mapping.mcp.servers_key) {
                    Some(e) => e,
                    None => return Vec::new(),
                };
                entries
                    .into_iter()
                    .map(|(name, val)| {
                        let enabled = if self.mapping.mcp.uses_enabled_semantic() {
                            // "enabled" semantic: enabled=true means ON, absent or false means OFF
                            val.get_path(self.mapping.mcp.toggle_key())
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false)
                        } else {
                            // "disabled" semantic: disabled=true means OFF, absent means ON
                            !val.get_path(self.mapping.mcp.toggle_key())
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false)
                        };
                        let detail = mcp_detail_str(val);
                        InspectItem {
                            name,
                            enabled,
                            detail,
                        }
                    })
                    .collect()
            },
        ))
    }

    fn inspect_plugin(&self, dir: &std::path::Path) -> Result<Vec<InspectItem>> {
        let mut items: Vec<InspectItem> = Vec::new();
        if self.is_plugin_dir_format() {
            let plugins_dir = self.plugin_install_dir(dir);
            if !plugins_dir.is_dir() {
                return Ok(items);
            }
            let disabled_set: std::collections::HashSet<String> = try_load_doc(
                DocFormat::from_filename(self.mapping.plugin.enablement_path()),
                &dir.join(self.mapping.plugin.enablement_path()),
            )
            .and_then(|doc| {
                doc.root().as_object().map(|obj| {
                    obj.iter()
                        .filter(|(_, v)| v.as_bool() == Some(false))
                        .map(|(k, _)| k.clone())
                        .collect()
                })
            })
            .unwrap_or_default();
            for entry in std::fs::read_dir(&plugins_dir)?.flatten() {
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                items.push(InspectItem {
                    name: name.clone(),
                    enabled: !disabled_set.contains(&name),
                    detail: "directory".into(),
                });
            }
            return Ok(items);
        }
        if let Some((doc, pk, fmt)) = self.load_plugin_doc_if_exists() {
            for info in
                crud::plugin_inspect_from_doc(&doc, fmt, pk, self.mapping.plugin.disabled_key())
            {
                items.push(InspectItem {
                    name: info.name,
                    enabled: info.enabled,
                    detail: info.detail,
                });
            }
        }
        Ok(items)
    }

    fn inspect_hook(&self, dir: &std::path::Path) -> Result<Vec<InspectItem>> {
        let (config_path, format, is_toml) = self.hook_config_info(dir);
        Ok(with_doc_if_exists(
            format,
            &config_path,
            Vec::new(),
            |doc| {
                let mut items = Vec::new();
                let section_key = self.mapping.hook.section_key.as_str();
                for_each_hook_entry(
                    &doc,
                    is_toml,
                    section_key,
                    &self.mapping.hook.events,
                    &self.mapping.hook.command_key,
                    &self.mapping.hook.matcher_key,
                    &self.mapping.hook.hooks_key,
                    |he| {
                        items.push(hook_inspect_item(he.event, he.command, he.matcher));
                    },
                );
                items
            },
        ))
    }

    fn inspect_env(&self, dir: &std::path::Path) -> Vec<InspectItem> {
        let env_fmt = DocFormat::from_format_str(self.mapping.env.format_str());
        with_doc_if_exists(env_fmt, &self.settings_path_from(dir), Vec::new(), |doc| {
            let excluded = self.excluded_env_keys();
            let section_key = self.mapping.env.section_key();
            let count = doc
                .entries(section_key)
                .map(|e| {
                    e.iter()
                        .filter(|(k, _)| !excluded.contains(k.as_str()))
                        .count()
                })
                .unwrap_or(0);
            if count > 0 {
                vec![InspectItem {
                    name: "env".into(),
                    enabled: true,
                    detail: format!("{} vars", count),
                }]
            } else {
                Vec::new()
            }
        })
    }

    fn inspect_skill(&self, dir: &std::path::Path) -> Result<Vec<InspectItem>> {
        inspect_dir(
            dir.join(self.mapping.skill.path()),
            |p| {
                p.is_dir()
                    && p.file_name()
                        .map_or(true, |n| !n.to_string_lossy().starts_with('.'))
            },
            |name, path| {
                let detail = if path.is_symlink() {
                    std::fs::read_link(path)
                        .map(|t| format!("→ {}", t.display()))
                        .unwrap_or_else(|_| "symlink".into())
                } else {
                    "dir".into()
                };
                InspectItem {
                    name,
                    enabled: true,
                    detail,
                }
            },
        )
    }

    fn inspect_prompt(&self, dir: &std::path::Path) -> Vec<InspectItem> {
        let pp = self.prompt_path_from(dir);
        if pp.exists() {
            vec![InspectItem {
                name: pp
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| self.mapping.prompt.path().into()),
                enabled: true,
                detail: pp.to_string_lossy().into_owned(),
            }]
        } else {
            Vec::new()
        }
    }

    fn inspect_agent(&self, dir: &std::path::Path) -> Result<Vec<InspectItem>> {
        let agent_path = match &self.mapping.agent.path {
            Some(p) => p,
            None => return Ok(Vec::new()),
        };
        let (is_yaml, ext) = self.agent_format_info();
        let tool_name = &self.mapping.tool.name;
        inspect_dir(
            dir.join(agent_path),
            |path| path.extension().is_some_and(|e| e == ext),
            |name, path| {
                let detail = std::fs::read_to_string(path)
                    .ok()
                    .map(|raw| {
                        let agent = if is_yaml {
                            parse_agent_yaml(&raw, &name)
                        } else {
                            parse_agent_markdown(&raw, &name, tool_name)
                        };
                        agent
                            .and_then(|a| a.config.model.map(|m| format!("model: {}", m)))
                            .unwrap_or_else(|| "mode: primary".into())
                    })
                    .unwrap_or_else(|| "mode: primary".into());
                InspectItem {
                    name,
                    enabled: true,
                    detail,
                }
            },
        )
    }

    fn inspect_provider(&self, dir: &std::path::Path) -> Result<Vec<InspectItem>> {
        if let Some(field_map) = &self.mapping.provider.field_map {
            let (format, config_path) = provider::provider_config_info(&self.mapping, dir);
            let doc = provider::load_provider_doc(format, &config_path, &self.mapping, dir)?;
            if doc.is_empty() {
                return Ok(vec![]);
            }
            let mut entries = crate::adapter::doc_engine::inspect_entries(&doc, field_map);
            for item in &mut entries {
                if item.detail.contains("type: unknown") {
                    if let Some(npm_name) = doc
                        .get(&format!("{}.{}.npm", field_map.entries_path, item.name))
                        .and_then(|v| v.as_str())
                    {
                        let inferred = self
                            .mapping
                            .provider
                            .npm_type_map
                            .get(npm_name)
                            .map(|t| format!("type: {} (npm)", t))
                            .unwrap_or_else(|| format!("type: {} (npm)", npm_name));
                        item.detail = item.detail.replace("type: unknown", &inferred);
                    } else if let Some(vcc_type) = self
                        .mapping
                        .provider
                        .type_map
                        .iter()
                        .find(|tm| tm.tool == item.name)
                        .map(|tm| tm.vcc.as_str())
                    {
                        item.detail = item
                            .detail
                            .replace("type: unknown", &format!("type: {}", vcc_type));
                    }
                }
            }
            return Ok(entries);
        }
        let fmt = &self.mapping.provider.format;
        Ok(match fmt.as_str() {
            "env_vars" | "env_file" | "yaml_flat" => {
                provider::env_inspect_items(&self.mapping, dir)
            }
            "codex_split" => provider::codex_inspect_items(&self.mapping, dir),
            "json_custom_models" => provider::json_models_inspect_items(&self.mapping, dir),
            _ => Vec::new(),
        })
    }

    // ── plugin ops methods ──

    /// Unified plugin doc operation: apply (clear_first=true) or upsert (clear_first=false, enable_mode)
    fn plugin_doc_op(
        &self,
        store: &TomlStore,
        names: &[String],
        dry_run: bool,
        clear_first: bool,
        enable_mode: bool,
    ) -> Result<usize> {
        let plugin_cfg = &self.mapping.plugin;
        if plugin_cfg.format.is_empty() {
            return Ok(0);
        }
        // Directory format — filesystem only, no doc operations
        if self.is_plugin_dir_format() {
            let dir = self.require_config_dir()?;
            let pd = self.plugin_install_dir(&dir);
            if clear_first && !dry_run {
                clean_stale_dir_entries(&pd, &names.iter().collect());
            }
            let enabled_plugins = self.collect_enabled_plugins(store, names);
            let applied = self.install_plugins_to_dir(&enabled_plugins, &dir, store, dry_run)?;
            if clear_first {
                print_apply_status("plugin", applied, &pd, dry_run);
            } else if !dry_run && applied > 0 {
                println!("  plugin (+{})", applied);
            }
            return Ok(applied);
        }
        let dir = self.require_config_dir()?;
        let enabled_plugins = self.collect_enabled_plugins(store, names);
        if enabled_plugins.is_empty() {
            return Ok(0);
        }
        let applied = match plugin_cfg.format.as_str() {
            "enabled_list" | "json_array" => {
                let config_path = self.plugin_array_path()?;
                let pk = plugin_cfg.array_key();
                let is_json_array = plugin_cfg.format == "json_array";
                with_doc(
                    &config_path,
                    DocFormat::from_filename(config_path.to_string_lossy().as_ref()),
                    dry_run,
                    |doc| {
                        let existing = if is_json_array && clear_first {
                            doc.extract_array_name_map(pk)
                        } else {
                            HashMap::new()
                        };
                        if clear_first {
                            doc.clear_section(pk);
                        }
                        let mut new_plugins = Vec::new();
                        let mut applied = 0;
                        for plugin in &enabled_plugins {
                            let rn = plugin_ref_name(plugin);
                            let found = doc.find_matching_key(pk, &rn);
                            // entries() only works on objects (returns None for arrays).
                            // Use get().as_array() to check string entries directly.
                            let already_in_array = is_json_array
                                && doc.get(pk).and_then(|v| v.as_array()).is_some_and(|arr| {
                                    arr.iter().any(|item| {
                                        item.as_str().is_some_and(|s| {
                                            s == rn || s.starts_with(&format!("{}@", rn))
                                        })
                                    })
                                });
                            if enable_mode && already_in_array {
                                // Already in JSON array — nothing to do (presence = enabled)
                                continue;
                            } else if enable_mode && found.is_some() {
                                // Remove disabled key (enable the plugin)
                                if let Some(DocValue::Object(map)) = doc.get_entry_mut(pk, &rn) {
                                    map.remove(self.mapping.plugin.disabled_key());
                                }
                            } else if clear_first {
                                doc.push(
                                    pk,
                                    existing
                                        .get(&rn)
                                        .cloned()
                                        .unwrap_or_else(|| DocValue::String(rn.clone())),
                                );
                                new_plugins.push(plugin.clone());
                            } else if found.is_none() {
                                doc.push(pk, DocValue::String(rn.clone()));
                                new_plugins.push(plugin.clone());
                            }
                            applied += 1;
                        }
                        if !is_json_array {
                            for p in &new_plugins {
                                self.install_plugin_files(p, &dir, store, dry_run)?;
                            }
                            if let Some(mk) = &plugin_cfg.marketplace_key {
                                Self::sync_marketplaces_in_doc(doc, mk, &new_plugins);
                            }
                        } else {
                            self.maybe_insert_schema(doc);
                        }
                        Ok(applied)
                    },
                )?
            }
            "enabled_map" | "toml_table" => {
                let (config_path, fmt) = self.plugin_doc_config_path()?;
                let pk = self.plugins_key();
                with_doc(&config_path, fmt, dry_run, |doc| {
                    if clear_first {
                        doc.clear_section(pk);
                    } else {
                        doc.ensure_object(pk);
                    }
                    let mut new_plugins = Vec::new();
                    let mut applied = 0;
                    for plugin in &enabled_plugins {
                        let rn = plugin_ref_name(plugin);
                        if enable_mode {
                            if let Some(key) = doc.find_matching_key(pk, &rn) {
                                // Enable the plugin
                                if self.mapping.plugin.format == "enabled_map" {
                                    // For enabled_map (e.g. claude), set bool value to true
                                    doc.set(&format!("{}.{}", pk, key), DocValue::Bool(true));
                                } else if let Some(DocValue::Object(map)) =
                                    doc.get_entry_mut(pk, &rn)
                                {
                                    // For toml_table, remove disabled key
                                    map.remove(self.mapping.plugin.disabled_key());
                                }
                            }
                        } else {
                            Self::set_plugin_entry_in_doc(doc, pk, &rn);
                            self.install_plugin_files(plugin, &dir, store, dry_run)?;
                            new_plugins.push(plugin.clone());
                        }
                        applied += 1;
                    }
                    if doc.format() == DocFormat::Json {
                        if let Some(mk) = &plugin_cfg.marketplace_key {
                            Self::sync_marketplaces_in_doc(doc, mk, &new_plugins);
                        }
                    }
                    Ok(applied)
                })?
            }
            _ => 0,
        };
        if !clear_first && !dry_run && applied > 0 {
            println!("  plugin (+{})", applied);
        }
        Ok(applied)
    }

    fn sync_marketplaces_in_doc(doc: &mut DocTree, marketplace_key: &str, plugins: &[Plugin]) {
        let needed: HashMap<String, String> = plugins
            .iter()
            .filter_map(|p| {
                let mp = p.config.marketplace.as_ref()?;
                let repo = p.config.repo.as_ref()?;
                Some((mp.clone(), repo.clone()))
            })
            .collect();
        if needed.is_empty() {
            return;
        }
        doc.ensure_object(marketplace_key);
        for (name, repo) in &needed {
            let entry_path = format!("{}.{}", marketplace_key, name);
            if doc.get(&entry_path).is_none() {
                doc.set(
                    &format!("{}.source.source", entry_path),
                    DocValue::String("github".to_string()),
                );
                doc.set(
                    &format!("{}.source.repo", entry_path),
                    DocValue::String(repo.clone()),
                );
            }
        }
    }

    fn remove_plugin_from_config(&self, names: &[String], dry_run: bool) -> Result<usize> {
        let plugin_cfg = &self.mapping.plugin;
        if plugin_cfg.format.is_empty() {
            return Ok(0);
        }
        if self.is_plugin_dir_format() {
            let removed = remove_dir_entries(
                &self.plugin_install_dir(&self.require_config_dir()?),
                names,
                dry_run,
                false,
            )?;
            if !dry_run && removed > 0 {
                println!("  plugin (-{})", removed);
            }
            return Ok(removed);
        }
        // Array formats (enabled_list, json_array)
        let removed = if matches!(plugin_cfg.format.as_str(), "enabled_list" | "json_array") {
            let arr_path = self.plugin_array_path()?;
            with_doc(
                &arr_path,
                DocFormat::from_filename(arr_path.to_string_lossy().as_ref()),
                dry_run,
                |doc| {
                    let pk = plugin_cfg.array_key();
                    let before = doc
                        .get(pk)
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    let ns: std::collections::HashSet<&str> =
                        names.iter().map(|s| s.as_str()).collect();
                    doc.retain_in_array(pk, |v| {
                        v.as_str()
                            .or_else(|| {
                                v.as_array()
                                    .and_then(|a| a.first().and_then(|f| f.as_str()))
                            })
                            .map_or(true, |s| !ns.contains(parse_plugin_ref(s).0.as_str()))
                    });
                    Ok(before
                        - doc
                            .get(pk)
                            .and_then(|v| v.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0))
                },
            )?
        } else {
            // Map formats (enabled_map, toml_table)
            let (config_path, fmt) = self.plugin_doc_config_path()?;
            if !config_path.exists() {
                return Ok(0);
            }
            with_doc(&config_path, fmt, dry_run, |doc| {
                Ok(names
                    .iter()
                    .map(|n| doc.remove_matching_from_object(self.plugins_key(), n))
                    .sum::<usize>())
            })?
        };
        if !dry_run && removed > 0 {
            println!("  plugin (-{})", removed);
        }
        Ok(removed)
    }

    fn collect_enabled_plugins(&self, store: &TomlStore, names: &[String]) -> Vec<Plugin> {
        let tool_name = self.mapping.tool.name.as_str();
        names
            .iter()
            .filter_map(|name| {
                let plugin: Plugin = load_or_skip(store, "plugin", name)?;
                if plugin.tool.get(tool_name).is_some_and(|o| o.disabled) {
                    None
                } else {
                    Some(plugin)
                }
            })
            .collect()
    }

    fn install_plugin_to_target(
        &self,
        plugin: &Plugin,
        target: &std::path::Path,
        ensure_parent: bool,
        dry_run: bool,
        store: &TomlStore,
    ) -> Result<bool> {
        let src_path = match self.get_plugin_source_path(plugin, store) {
            Some(p) if p.exists() => p,
            Some(_) => {
                println!("  plugin '{}' not installed yet", plugin.name);
                return Ok(false);
            }
            None => {
                println!("  plugin '{}' source not available", plugin.name);
                return Ok(false);
            }
        };
        if !dry_run {
            if ensure_parent {
                ensure_dir(target.parent().unwrap_or(target))?;
            }
            apply_skill_link(target, &src_path, &plugin.config.install_method)?;
            self.adapt_plugin_for_tool(plugin, target, dry_run)?;
        }
        Ok(true)
    }

    fn install_plugins_to_dir(
        &self,
        plugins: &[Plugin],
        dir: &std::path::Path,
        store: &TomlStore,
        dry_run: bool,
    ) -> Result<usize> {
        let plugins_dir = self.plugin_install_dir(dir);
        let mut applied = 0;
        for plugin in plugins {
            let target = plugins_dir.join(&plugin.name);
            if self.install_plugin_to_target(plugin, &target, true, dry_run, store)? {
                applied += 1;
            }
        }
        Ok(applied)
    }

    fn get_plugin_source_path(
        &self,
        plugin: &Plugin,
        store: &TomlStore,
    ) -> Option<std::path::PathBuf> {
        match plugin.config.source.as_str() {
            "local" => plugin.config.path.as_ref().map(std::path::PathBuf::from),
            "github" | "url" | "marketplace" => {
                let d = store
                    .root()
                    .join("cache")
                    .join("plugins")
                    .join(&plugin.name);
                d.exists().then_some(d)
            }
            _ => None,
        }
    }

    fn install_plugin_files(
        &self,
        plugin: &Plugin,
        tool_dir: &std::path::Path,
        store: &TomlStore,
        dry_run: bool,
    ) -> Result<()> {
        let install_dir = match &self.mapping.plugin.install_dir {
            Some(d) => d,
            None => return Ok(()),
        };
        let target = tool_dir
            .join(install_dir)
            .join(plugin.config.marketplace.as_deref().unwrap_or("local"))
            .join(&plugin.name);
        self.install_plugin_to_target(plugin, &target, false, dry_run, store)?;
        Ok(())
    }

    fn adapt_plugin_for_tool(
        &self,
        plugin: &Plugin,
        target_dir: &std::path::Path,
        dry_run: bool,
    ) -> Result<()> {
        let target_tool = self.mapping.tool.name.as_str();
        let source_format = plugin.config.format.as_deref().unwrap_or(target_tool);
        if source_format == target_tool || source_format == "universal" || dry_run {
            return Ok(());
        }
        let source_manifest_path = target_dir
            .join(get_manifest_dir(source_format))
            .join(get_manifest_file(source_format));
        if !source_manifest_path.exists() {
            return Ok(());
        }
        let source_manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&source_manifest_path)?)
                .unwrap_or_default();
        if source_manifest.is_null() {
            return Ok(());
        }
        let plugin_cfg = &self.mapping.plugin;
        let dir = match plugin_cfg.manifest_dir() {
            "" => target_dir.to_path_buf(),
            d => {
                let p = target_dir.join(d);
                ensure_dir(&p)?;
                p
            }
        };
        std::fs::write(
            dir.join(plugin_cfg.manifest_file()),
            serde_json::to_string_pretty(&adapt_plugin_manifest(
                source_format,
                target_tool,
                &source_manifest,
            )?)?,
        )?;
        Ok(())
    }
}

/// Build a detail string for an MCP server entry from its DocValue.
fn mcp_detail_str(val: &DocValue) -> String {
    // Array-format command (e.g. ["npx", "args..."])
    if let Some(arr) = val.get_path("command").and_then(|v| v.as_array()) {
        let parts: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
        if !parts.is_empty() {
            return format!("command: {}", parts.join(" "));
        }
    }
    // String-format command
    if let Some(cmd) = val.get_path_str("command") {
        let args: Vec<String> = val
            .get_path("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        return if args.is_empty() {
            format!("command: {}", cmd)
        } else {
            format!("command: {} {}", cmd, args.join(" "))
        };
    }
    // URL-based server
    if let Some(url) = val
        .get_path_str("url")
        .or_else(|| val.get_path_str("httpUrl"))
    {
        return format!("url: {}", url);
    }
    "configured".to_string()
}

fn hook_inspect_item(event: &str, command: &str, matcher: &str) -> InspectItem {
    let cmd = command.split_whitespace().next().unwrap_or(command);
    let name = if matcher.is_empty() {
        format!("{}:{}", event, cmd)
    } else {
        format!("{}:{}:{}", event, matcher, cmd)
    };
    InspectItem {
        name,
        enabled: true,
        detail: format!("command: {}", command),
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;
    use crate::adapter::doc_engine::{DocFormat, DocTree};

    // ── mcp_detail_str ──

    #[test]
    fn test_mcp_detail_str_array_command() {
        let doc = DocTree::new_test(
            DocFormat::Json,
            serde_json::json!({
                "command": ["npx", "-y", "@anthropic/server"],
            })
            .into(),
        );
        let val = doc.root();
        assert_eq!(mcp_detail_str(val), "command: npx -y @anthropic/server");
    }

    #[test]
    fn test_mcp_detail_str_string_command_with_args() {
        let doc = DocTree::new_test(
            DocFormat::Json,
            serde_json::json!({
                "command": "npx",
                "args": ["-y", "server"],
            })
            .into(),
        );
        let val = doc.root();
        assert_eq!(mcp_detail_str(val), "command: npx -y server");
    }

    #[test]
    fn test_mcp_detail_str_string_command_no_args() {
        let doc = DocTree::new_test(
            DocFormat::Json,
            serde_json::json!({
                "command": "/usr/bin/mcp",
            })
            .into(),
        );
        let val = doc.root();
        assert_eq!(mcp_detail_str(val), "command: /usr/bin/mcp");
    }

    #[test]
    fn test_mcp_detail_str_url() {
        let doc = DocTree::new_test(
            DocFormat::Json,
            serde_json::json!({
                "url": "https://mcp.example.com/sse",
            })
            .into(),
        );
        let val = doc.root();
        assert_eq!(mcp_detail_str(val), "url: https://mcp.example.com/sse");
    }

    #[test]
    fn test_mcp_detail_str_http_url() {
        let doc = DocTree::new_test(
            DocFormat::Json,
            serde_json::json!({
                "httpUrl": "http://localhost:8080/mcp",
            })
            .into(),
        );
        let val = doc.root();
        assert_eq!(mcp_detail_str(val), "url: http://localhost:8080/mcp");
    }

    #[test]
    fn test_mcp_detail_str_fallback() {
        let doc = DocTree::new_test(
            DocFormat::Json,
            serde_json::json!({
                "type": "streamable",
            })
            .into(),
        );
        let val = doc.root();
        assert_eq!(mcp_detail_str(val), "configured");
    }

    #[test]
    fn test_mcp_detail_str_empty_array_command() {
        let doc = DocTree::new_test(
            DocFormat::Json,
            serde_json::json!({
                "command": [],
                "url": "https://fallback.com",
            })
            .into(),
        );
        let val = doc.root();
        // Empty array → falls through to url
        assert_eq!(mcp_detail_str(val), "url: https://fallback.com");
    }

    // ── hook_inspect_item ──

    #[test]
    fn test_hook_inspect_item_no_matcher() {
        let item = hook_inspect_item("PreToolUse", "echo hello", "");
        assert_eq!(item.name, "PreToolUse:echo");
        assert_eq!(item.detail, "command: echo hello");
        assert!(item.enabled);
    }

    #[test]
    fn test_hook_inspect_item_with_matcher() {
        let item = hook_inspect_item("PostToolUse", "my-script.sh", "Bash");
        assert_eq!(item.name, "PostToolUse:Bash:my-script.sh");
    }

    #[test]
    fn test_hook_inspect_item_extracts_first_word() {
        let item = hook_inspect_item("PreToolUse", "/usr/bin/python3 script.py --flag", "");
        assert_eq!(item.name, "PreToolUse:/usr/bin/python3");
        assert_eq!(item.detail, "command: /usr/bin/python3 script.py --flag");
    }

    #[test]
    fn test_hook_inspect_item_command_only() {
        let item = hook_inspect_item("Notification", "notify-send", "");
        assert_eq!(item.name, "Notification:notify-send");
    }
}
