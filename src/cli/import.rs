use anyhow::{bail, Result};
use std::collections::HashMap;

use super::output::{is_json_mode, output_json, print_dry_run_banner};
use crate::adapter;
use crate::datasource::ccswitch::CcSwitchSource;
use crate::datasource::cherry_studio::CherryStudioSource;
use crate::model::{hook::Hook, mcp::McpServer, provider::Provider, Resource};
use crate::store::TomlStore;

fn fmt_counts(created: usize, updated: usize, skipped: usize) -> String {
    let mut p = Vec::new();
    if created > 0 {
        p.push(format!("+{} created", created));
    }
    if updated > 0 {
        p.push(format!("~{} updated", updated));
    }
    if skipped > 0 {
        p.push(format!("{} skipped", skipped));
    }
    p.join(", ")
}
#[derive(Default)]
struct ImportResult {
    created: usize,
    updated: usize,
    skipped: usize,
    by_category: HashMap<String, [usize; 3]>,
    items: Vec<(String, String, usize)>,
}

impl ImportResult {
    fn record(&mut self, category: &str, name: &str, status: usize) {
        match status {
            0 => self.created += 1,
            1 => self.updated += 1,
            _ => self.skipped += 1,
        }
        self.by_category.entry(category.to_string()).or_default()[status] += 1;
        self.items
            .push((category.to_string(), name.to_string(), status));
    }
    fn has_changes(&self) -> bool {
        self.created > 0 || self.updated > 0
    }
    fn merge(&mut self, other: &ImportResult) {
        self.created += other.created;
        self.updated += other.updated;
        self.skipped += other.skipped;
        for (cat, counts) in &other.by_category {
            let entry = self.by_category.entry(cat.clone()).or_default();
            entry[0] += counts[0];
            entry[1] += counts[1];
            entry[2] += counts[2];
        }
        self.items.extend(other.items.iter().cloned());
    }
    fn to_json_by_category(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut cats: Vec<&String> = self.by_category.keys().collect();
        cats.sort();
        let mut map = serde_json::Map::new();
        for cat in &cats {
            let counts = self.by_category[*cat];
            let ci: Vec<&(String, String, usize)> =
                self.items.iter().filter(|(c, _, _)| c == *cat).collect();
            map.insert(cat.to_string(), serde_json::json!({ "created": counts[0], "updated": counts[1], "skipped": counts[2],
                "created_items": ci.iter().filter(|(_,_,s)|*s==0).map(|(_,n,_)|n.as_str()).collect::<Vec<&str>>(),
                "updated_items": ci.iter().filter(|(_,_,s)|*s==1).map(|(_,n,_)|n.as_str()).collect::<Vec<&str>>(),
                "skipped_items": ci.iter().filter(|(_,_,s)|*s==2).map(|(_,n,_)|n.as_str()).collect::<Vec<&str>>() }));
        }
        map
    }
    fn to_json(&self, tool: &str) -> serde_json::Value {
        serde_json::json!({
            "tool": tool,
            "created": self.created,
            "updated": self.updated,
            "skipped": self.skipped,
            "by_category": self.to_json_by_category()
        })
    }
    fn from_sync_result(sr: &adapter::SyncResult) -> Self {
        let mut r = ImportResult::default();
        for i in &sr.created {
            r.record(&i.category, &i.name, 0);
        }
        for i in &sr.updated {
            r.record(&i.category, &i.name, 1);
        }
        for i in &sr.skipped {
            r.record(&i.category, &i.name, 2);
        }
        r
    }
}

fn fmt_cat_total(by_category: &HashMap<String, [usize; 3]>) -> String {
    let mut cats: Vec<&String> = by_category.keys().collect();
    cats.sort();
    cats.iter()
        .filter_map(|cat| {
            let c = by_category[*cat];
            let t = c[0] + c[1] + c[2];
            if t > 0 {
                Some(format!("{}: {}", cat, t))
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Import resources matched by name, using `resource_unchanged` for content equality.
fn import_by_name<T: Resource + serde::Serialize>(
    store: &TomlStore,
    resources: &[T],
    kind: &str,
    dry_run: bool,
) -> Result<ImportResult> {
    import_resources(
        store,
        resources,
        kind,
        |s, r: &T| s.find_by_content::<T, _>(kind, |e| e.name() == r.name()),
        |existing, incoming| {
            if resource_unchanged(existing, incoming) {
                None
            } else {
                Some(incoming.clone())
            }
        },
        dry_run,
    )
}

fn import_resources<T, F, R>(
    store: &TomlStore,
    resources: &[T],
    kind: &str,
    find_fn: F,
    resolve_fn: R,
    dry_run: bool,
) -> Result<ImportResult>
where
    T: Resource,
    F: Fn(&TomlStore, &T) -> Result<Option<T>>,
    R: Fn(&T, &T) -> Option<T>,
{
    let mut result = ImportResult::default();
    for res in resources {
        let existing = find_fn(store, res)?;
        if let Some(existing) = existing {
            if let Some(resolved) = resolve_fn(&existing, res) {
                resolved.validate()?;
                if !dry_run {
                    store.save_resource(&resolved)?;
                }
                result.record(kind, res.name(), 1);
            } else {
                result.record(kind, res.name(), 2);
            }
        } else {
            res.validate()?;
            if !dry_run {
                store.save_resource(res)?;
            }
            result.record(kind, res.name(), 0);
        }
    }
    Ok(result)
}

fn print_import_summary(label: &str, result: &ImportResult) {
    if is_json_mode() {
        return;
    }
    if result.has_changes() {
        let p = fmt_counts(result.created, result.updated, result.skipped);
        println!("{}: {} ({})", label, p, fmt_cat_total(&result.by_category));
    } else if result.skipped > 0 {
        println!(
            "{}: up to date ({})",
            label,
            fmt_cat_total(&result.by_category)
        );
    } else {
        println!("{}: no resources found", label);
    }
}

fn import_providers_to_store(
    store: &TomlStore,
    providers: &[Provider],
    dry_run: bool,
) -> Result<ImportResult> {
    import_resources(
        store,
        providers,
        "provider",
        |s, p: &Provider| s.find_by_content::<Provider, _>("provider", |e| e.name == p.name),
        |existing, incoming| {
            let merged = merge_provider_full(existing, incoming);
            if resource_unchanged(existing, &merged) {
                None
            } else {
                Some(merged)
            }
        },
        dry_run,
    )
}

fn import_mcp_servers_to_store(
    store: &TomlStore,
    servers: &[McpServer],
    dry_run: bool,
) -> Result<ImportResult> {
    import_resources(
        store,
        servers,
        "mcp",
        |s, srv: &McpServer| s.find_by_content::<McpServer, _>("mcp", |e| e.name == srv.name),
        |existing, incoming| {
            let merged = merge_mcp(existing, incoming);
            if resource_unchanged(existing, &merged) {
                None
            } else {
                Some(merged)
            }
        },
        dry_run,
    )
}

fn output_import_json(tool: &str, dry_run: bool, result: &ImportResult, registry_total: usize) {
    let mut val = result.to_json(tool);
    if let Some(obj) = val.as_object_mut() {
        obj.insert("dry_run".into(), serde_json::json!(dry_run));
        obj.insert("registry_total".into(), serde_json::json!(registry_total));
    }
    output_json(&val);
}

pub(crate) fn run_adapter(tool: &str, dry_run: bool) -> Result<()> {
    let store = TomlStore::new()?;
    let adapter_instance = adapter::get_adapter(tool)?.ok_or_else(|| {
        let mut all = adapter::supported_tool_names();
        all.push_str(", cc-switch, cherry-studio");
        anyhow::anyhow!("unsupported tool: '{}'. Supported: {}", tool, all)
    })?;
    if !adapter_instance.has_config_dir() {
        bail!("{} config directory not found", tool);
    }
    print_dry_run_banner(dry_run);
    if !is_json_mode() {
        println!("Importing from {}...", tool);
    }
    let result = adapter_instance.sync(&store, dry_run)?;
    let ir = ImportResult::from_sync_result(&result);
    if is_json_mode() {
        output_import_json(tool, dry_run, &ir, store.count_all_resources());
        return Ok(());
    }
    if result.is_empty() {
        println!("vcc: no configurations found to import from {}", tool);
    } else if ir.has_changes() {
        if !result.created.is_empty() {
            println!(
                "  created: {}",
                result
                    .created
                    .iter()
                    .map(|i| format!("{}:{}", i.category, i.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if !result.updated.is_empty() {
            println!(
                "  updated: {}",
                result
                    .updated
                    .iter()
                    .map(|i| format!("{}:{}", i.category, i.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let rt = store.count_all_resources();
        if dry_run {
            println!(
                "\n=== DRY RUN: +{} created, ~{} updated, {} skipped ({}) ===",
                ir.created,
                ir.updated,
                ir.skipped,
                fmt_cat_total(&ir.by_category)
            );
        } else {
            println!(
                "vcc: imported {} from {}, registry total: {}",
                ir.created + ir.updated,
                tool,
                rt
            );
        }
    } else if ir.skipped > 0 {
        println!("{}: up to date ({})", tool, fmt_cat_total(&ir.by_category));
    }
    Ok(())
}

pub(crate) fn run_all(dry_run: bool) -> Result<()> {
    let store = TomlStore::new()?;
    let adapters = adapter::all_adapters();
    print_dry_run_banner(dry_run);
    let mut grand_total = ImportResult::default();
    let mut tool_results: Vec<serde_json::Value> = Vec::new();
    for adapter_instance in &adapters {
        let tool_name = adapter_instance.tool_name();
        if !adapter_instance.has_config_dir() {
            continue;
        }
        let result = adapter_instance.sync(&store, dry_run)?;
        if result.is_empty() {
            continue;
        }
        let ir = ImportResult::from_sync_result(&result);
        grand_total.merge(&ir);
        print_import_summary(tool_name, &ir);
        tool_results.push(ir.to_json(tool_name));
    }
    if CcSwitchSource::new().is_some() {
        let ir = run_ccswitch_inner(&store, dry_run, false)?;
        grand_total.merge(&ir);
        tool_results.push(ir.to_json("cc-switch"));
    }
    if CherryStudioSource::new().is_some() {
        let ir = run_cherry_studio_inner(&store, dry_run, false)?;
        grand_total.merge(&ir);
        tool_results.push(ir.to_json("cherry-studio"));
    }
    if is_json_mode() {
        let rt = store.count_all_resources();
        output_json(
            &serde_json::json!({"dry_run":dry_run,"total_created":grand_total.created,"total_updated":grand_total.updated,"total_skipped":grand_total.skipped,"registry_total":rt,"by_category":grand_total.to_json_by_category(),"tools":tool_results}),
        );
    } else if grand_total.created > 0 || grand_total.updated > 0 {
        let rt = store.count_all_resources();
        println!(
            "\n---\nTotal: +{} created, ~{} updated, {} skipped (registry: {})",
            grand_total.created, grand_total.updated, grand_total.skipped, rt
        );
        let mut cats: Vec<&String> = grand_total.by_category.keys().collect();
        cats.sort();
        for cat in &cats {
            let c = grand_total.by_category[*cat];
            println!("  {}: {}", cat, fmt_counts(c[0], c[1], c[2]));
        }
    } else if grand_total.skipped > 0 {
        let rt = store.count_all_resources();
        println!(
            "vcc: everything up to date ({} skipped, registry: {})",
            grand_total.skipped, rt
        );
    } else {
        println!("vcc: no configurations found to import");
    }
    Ok(())
}

pub(crate) fn run_ccswitch(dry_run: bool) -> Result<()> {
    let store = TomlStore::new()?;
    let ir = run_ccswitch_inner(&store, dry_run, true)?;
    if is_json_mode() {
        output_import_json("cc-switch", dry_run, &ir, store.count_all_resources());
    }
    Ok(())
}

fn run_ccswitch_inner(store: &TomlStore, dry_run: bool, verbose: bool) -> Result<ImportResult> {
    let source = CcSwitchSource::new().ok_or_else(|| {
        anyhow::anyhow!("cc-switch database not found at ~/.cc-switch/cc-switch.db")
    })?;
    if verbose && !is_json_mode() {
        println!(
            "Importing from cc-switch ({})...",
            source.db_path().display()
        );
    }
    let mut result = ImportResult::default();
    let providers = source.import_providers()?;
    result.merge(&import_providers_to_store(store, &providers, dry_run)?);
    let servers = source.import_mcp_servers()?;
    result.merge(&import_mcp_servers_to_store(store, &servers, dry_run)?);
    let hooks = source.import_hooks()?;
    result.merge(&import_resources(
        store,
        &hooks,
        "hook",
        |s, h: &Hook| {
            s.find_by_content::<Hook, _>("hook", |e| {
                e.config.event == h.config.event && e.config.matcher == h.config.matcher
            })
        },
        |existing, incoming| {
            if hook_content_eq(existing, incoming) {
                None
            } else {
                Some(incoming.clone())
            }
        },
        dry_run,
    )?);
    let envs = source.import_envs()?;
    result.merge(&import_by_name(store, &envs, "env", dry_run)?);
    let prompts = source.import_prompts()?;
    result.merge(&import_by_name(store, &prompts, "prompt", dry_run)?);
    let skills = source.import_skills()?;
    result.merge(&import_by_name(store, &skills, "skill", dry_run)?);
    print_import_summary("cc-switch", &result);
    Ok(result)
}

fn resource_unchanged<T: Resource + serde::Serialize>(existing: &T, merged: &T) -> bool {
    // Compare hash-relevant fields only, ignoring id/name/metadata/tool
    let existing_json = serde_json::to_value(existing).unwrap_or_default();
    let merged_json = serde_json::to_value(merged).unwrap_or_default();
    crate::model::hash_fields::resource_hash_content(existing.kind(), &existing_json)
        == crate::model::hash_fields::resource_hash_content(merged.kind(), &merged_json)
}
fn hook_content_eq(a: &Hook, b: &Hook) -> bool {
    a.config.event == b.config.event
        && a.config.matcher == b.config.matcher
        && a.config.command == b.config.command
        && a.config.timeout == b.config.timeout
        && a.tool.len() == b.tool.len()
        && a.tool.iter().all(|(k, v)| {
            b.tool.get(k).is_some_and(|bv| {
                bv.matcher == v.matcher && bv.command == v.command && bv.timeout == v.timeout
            })
        })
}
macro_rules! define_merge {
    ($fn_name:ident, $type:ty) => {
        pub fn $fn_name(existing: &$type, incoming: &$type) -> $type {
            let mut merged = existing.clone();
            for (k, v) in &incoming.tool {
                merged.tool.insert(k.clone(), v.clone());
            }
            for (k, v) in &incoming.config.env {
                merged.config.env.insert(k.clone(), v.clone());
            }
            for (k, v) in &incoming.config.extra {
                merged.config.extra.insert(k.clone(), v.clone());
            }
            for tag in &incoming.metadata.tags {
                if !merged.metadata.tags.contains(tag) {
                    merged.metadata.tags.push(tag.clone());
                }
            }
            merged
        }
    };
}

define_merge!(merge_provider, Provider);
define_merge!(merge_mcp, McpServer);

fn merge_provider_full(existing: &Provider, incoming: &Provider) -> Provider {
    let mut merged = merge_provider(existing, incoming);
    for (k, v) in &incoming.config.headers {
        merged.config.headers.insert(k.clone(), v.clone());
    }
    merged
}

pub(crate) fn run_cherry_studio(dry_run: bool) -> Result<()> {
    let store = TomlStore::new()?;
    let ir = run_cherry_studio_inner(&store, dry_run, true)?;
    if is_json_mode() {
        output_import_json("cherry-studio", dry_run, &ir, store.count_all_resources());
    }
    Ok(())
}

fn run_cherry_studio_inner(
    store: &TomlStore,
    dry_run: bool,
    verbose: bool,
) -> Result<ImportResult> {
    let source = CherryStudioSource::new().ok_or_else(|| {
        anyhow::anyhow!(
            "Cherry Studio data not found (looked in ~/Library/Application Support/CherryStudio/)"
        )
    })?;
    if verbose && !is_json_mode() {
        println!(
            "Importing from Cherry Studio ({})...",
            source.data_dir().display()
        );
    }
    let mut result = ImportResult::default();
    let (providers, mcp_servers) = source.import_all()?;
    result.merge(&import_providers_to_store(store, &providers, dry_run)?);
    result.merge(&import_mcp_servers_to_store(store, &mcp_servers, dry_run)?);
    print_import_summary("cherry-studio", &result);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── fmt_counts ──

    #[test]
    fn test_fmt_counts_all() {
        assert_eq!(fmt_counts(2, 1, 3), "+2 created, ~1 updated, 3 skipped");
    }

    #[test]
    fn test_fmt_counts_only_created() {
        assert_eq!(fmt_counts(3, 0, 0), "+3 created");
    }

    #[test]
    fn test_fmt_counts_only_updated() {
        assert_eq!(fmt_counts(0, 2, 0), "~2 updated");
    }

    #[test]
    fn test_fmt_counts_only_skipped() {
        assert_eq!(fmt_counts(0, 0, 5), "5 skipped");
    }

    #[test]
    fn test_fmt_counts_empty() {
        assert_eq!(fmt_counts(0, 0, 0), "");
    }

    // ── fmt_cat_total ──

    #[test]
    fn test_fmt_cat_total_basic() {
        let mut by_cat = HashMap::new();
        by_cat.insert("mcp".into(), [2, 1, 0]);
        by_cat.insert("hook".into(), [0, 0, 3]);
        let result = fmt_cat_total(&by_cat);
        assert!(result.contains("hook: 3"));
        assert!(result.contains("mcp: 3"));
    }

    // ── ImportResult ──

    #[test]
    fn test_import_result_record_created() {
        let mut r = ImportResult::default();
        r.record("mcp", "fs", 0);
        assert_eq!(r.created, 1);
        assert_eq!(r.by_category["mcp"][0], 1);
    }

    #[test]
    fn test_import_result_record_updated() {
        let mut r = ImportResult::default();
        r.record("mcp", "fs", 1);
        assert_eq!(r.updated, 1);
        assert_eq!(r.by_category["mcp"][1], 1);
    }

    #[test]
    fn test_import_result_record_skipped() {
        let mut r = ImportResult::default();
        r.record("mcp", "fs", 2);
        assert_eq!(r.skipped, 1);
        assert_eq!(r.by_category["mcp"][2], 1);
    }

    #[test]
    fn test_import_result_has_changes() {
        let mut r = ImportResult::default();
        assert!(!r.has_changes());
        r.record("mcp", "fs", 0);
        assert!(r.has_changes());
    }

    #[test]
    fn test_import_result_has_changes_updated_only() {
        let mut r = ImportResult::default();
        r.record("mcp", "fs", 1);
        assert!(r.has_changes());
    }

    #[test]
    fn test_import_result_merge() {
        let mut a = ImportResult::default();
        a.record("mcp", "fs", 0);
        let mut b = ImportResult::default();
        b.record("hook", "test", 1);
        a.merge(&b);
        assert_eq!(a.created, 1);
        assert_eq!(a.updated, 1);
    }

    #[test]
    fn test_import_result_from_sync_result() {
        let mut sr = adapter::SyncResult::default();
        sr.created.push(adapter::SyncItem::new("mcp", "fs"));
        sr.skipped.push(adapter::SyncItem::new("hook", "test"));
        let ir = ImportResult::from_sync_result(&sr);
        assert_eq!(ir.created, 1);
        assert_eq!(ir.skipped, 1);
    }

    #[test]
    fn test_import_result_to_json() {
        let mut r = ImportResult::default();
        r.record("mcp", "fs", 0);
        let json = r.to_json("claude");
        assert_eq!(json["created"], 1);
        assert_eq!(json["tool"], "claude");
    }

    // ── hook_content_eq ──

    #[test]
    fn test_hook_content_eq_same() {
        let a = Hook {
            name: "test".into(),
            id: String::new(),
            r#type: "hook".into(),
            config: crate::model::HookConfig {
                event: "PreToolUse".into(),
                matcher: "Read".into(),
                command: "echo".into(),
                timeout: 30,
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let b = a.clone();
        assert!(hook_content_eq(&a, &b));
    }

    #[test]
    fn test_hook_content_eq_different_command() {
        let make_hook = |cmd: &str| Hook {
            name: "test".into(),
            id: String::new(),
            r#type: "hook".into(),
            config: crate::model::HookConfig {
                event: "PreToolUse".into(),
                matcher: "Read".into(),
                command: cmd.into(),
                timeout: 30,
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(!hook_content_eq(&make_hook("echo"), &make_hook("ls")));
    }

    // ── merge_provider / merge_mcp ──

    #[test]
    fn test_merge_provider() {
        let existing = Provider {
            name: "test".into(),
            id: "id1".into(),
            r#type: "provider".into(),
            config: crate::model::ProviderConfig {
                provider_type: "openai".into(),
                api_key: "sk-old".into(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let incoming = Provider {
            name: "test".into(),
            id: "id2".into(),
            r#type: "provider".into(),
            config: crate::model::ProviderConfig {
                provider_type: "openai".into(),
                api_key: "sk-new".into(),
                env: HashMap::from([("KEY".into(), "VAL".into())]),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let merged = merge_provider(&existing, &incoming);
        // env from incoming is merged
        assert!(merged.config.env.contains_key("KEY"));
    }

    // ── merge_mcp ──

    #[test]
    fn test_merge_mcp_tool_overrides() {
        use crate::model::mcp::McpToolOverride;
        let existing = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: crate::model::mcp::McpConfig::default(),
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let mut tool = HashMap::new();
        tool.insert(
            "claude".into(),
            McpToolOverride {
                disabled_tools: vec!["*".into()],
                ..Default::default()
            },
        );
        let incoming = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: crate::model::mcp::McpConfig::default(),
            metadata: Default::default(),
            tool,
        };
        let merged = merge_mcp(&existing, &incoming);
        assert!(merged.tool.contains_key("claude"));
    }

    #[test]
    fn test_merge_mcp_env_and_extra() {
        let mut env = HashMap::new();
        env.insert("KEY".into(), "val".into());
        let mut extra = HashMap::new();
        extra.insert("custom".into(), toml::Value::String("hello".into()));
        let existing = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: crate::model::mcp::McpConfig::default(),
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let incoming = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: crate::model::mcp::McpConfig {
                env,
                extra,
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let merged = merge_mcp(&existing, &incoming);
        assert!(merged.config.env.contains_key("KEY"));
        assert!(merged.config.extra.contains_key("custom"));
    }

    #[test]
    fn test_merge_mcp_tags_merged() {
        let existing = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: crate::model::mcp::McpConfig::default(),
            metadata: crate::model::Metadata {
                tags: vec!["synced".into()],
                ..Default::default()
            },
            tool: HashMap::new(),
        };
        let incoming = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: crate::model::mcp::McpConfig::default(),
            metadata: crate::model::Metadata {
                tags: vec!["cc-switch".into()],
                ..Default::default()
            },
            tool: HashMap::new(),
        };
        let merged = merge_mcp(&existing, &incoming);
        assert!(merged.metadata.tags.contains(&"synced".to_string()));
        assert!(merged.metadata.tags.contains(&"cc-switch".to_string()));
    }

    // ── merge_provider_full ──

    #[test]
    fn test_merge_provider_full_includes_headers() {
        let existing = Provider {
            name: "test".into(),
            id: String::new(),
            r#type: "provider".into(),
            config: crate::model::ProviderConfig::default(),
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let mut headers = HashMap::new();
        headers.insert("Authorization".into(), "Bearer tok".into());
        let incoming = Provider {
            name: "test".into(),
            id: String::new(),
            r#type: "provider".into(),
            config: crate::model::ProviderConfig {
                headers,
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let merged = merge_provider_full(&existing, &incoming);
        assert!(merged.config.headers.contains_key("Authorization"));
    }

    // ── resource_unchanged ──

    #[test]
    fn test_resource_unchanged_same_config() {
        let a = Provider {
            name: "test".into(),
            id: "id1".into(),
            r#type: "provider".into(),
            config: crate::model::ProviderConfig {
                provider_type: "openai".into(),
                api_key: "sk-123".into(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let mut b = a.clone();
        b.id = "id2".into(); // different id
        b.name = "different".into(); // different name
        assert!(resource_unchanged(&a, &b));
    }

    #[test]
    fn test_resource_unchanged_different_config() {
        let a = Provider {
            name: "test".into(),
            id: String::new(),
            r#type: "provider".into(),
            config: crate::model::ProviderConfig {
                api_key: "sk-1".into(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let b = Provider {
            name: "test".into(),
            id: String::new(),
            r#type: "provider".into(),
            config: crate::model::ProviderConfig {
                api_key: "sk-2".into(),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(!resource_unchanged(&a, &b));
    }

    #[test]
    fn test_resource_unchanged_mcp_different_command() {
        let a = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: crate::model::mcp::McpConfig {
                command: Some("npx".into()),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let b = McpServer {
            name: "fs".into(),
            id: String::new(),
            r#type: "mcp".into(),
            config: crate::model::mcp::McpConfig {
                command: Some("python".into()),
                ..Default::default()
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert!(!resource_unchanged(&a, &b));
    }

    #[test]
    fn test_resource_unchanged_ignores_metadata() {
        let a = Hook {
            name: "test".into(),
            id: String::new(),
            r#type: "hook".into(),
            config: crate::model::hook::HookConfig {
                event: "PreToolUse".into(),
                matcher: String::new(),
                command: "echo".into(),
                timeout: 30,
            },
            metadata: crate::model::Metadata {
                description: Some("a".into()),
                ..Default::default()
            },
            tool: HashMap::new(),
        };
        let mut b = a.clone();
        b.metadata.description = Some("different description".into());
        assert!(resource_unchanged(&a, &b)); // metadata changes don't affect hash
    }
}
