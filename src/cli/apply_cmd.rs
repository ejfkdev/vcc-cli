use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};

use super::output::{is_json_mode, output_json, print_dry_run_banner};
use crate::adapter;
use crate::store::TomlStore;

pub(crate) struct ApplyArgs {
    pub tool: Option<String>,
    pub profile: Option<String>,
    pub only: Option<String>,
    pub resource_ops: HashMap<String, AddRemove>,
    pub dry_run: bool,
}

pub(crate) struct AddRemove {
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

pub(crate) fn run(args: ApplyArgs) -> Result<()> {
    let has_add_remove = args
        .resource_ops
        .values()
        .any(|op| !op.add.is_empty() || !op.remove.is_empty());
    if args.profile.is_none() && !has_add_remove {
        bail!("--profile or at least one --add-*/--remove-* option is required");
    }

    let store = TomlStore::new()?;

    let only_types: HashSet<String> = args
        .only
        .as_deref()
        .map(|s| super::parse_csv(s).into_iter().collect())
        .unwrap_or_default();
    let valid_types = crate::config::resource_registry().all_kinds();
    for t in &only_types {
        if !valid_types.contains(&t.as_str()) {
            bail!(
                "unknown resource type '{}'. Valid types: {}",
                t,
                valid_types.join(", ")
            );
        }
    }

    let apply_all = only_types.is_empty();
    let should_apply = |t: &str| apply_all || only_types.contains(t);

    let tool_names: Vec<String> = if let Some(ref tool) = args.tool {
        adapter::validate_tool_name(tool)?;
        vec![tool.clone()]
    } else {
        let installed: Vec<String> = adapter::all_adapters()
            .iter()
            .filter(|a| a.config_dir().map(|d| d.exists()).unwrap_or(false))
            .map(|a| a.tool_name().to_string())
            .collect();
        if installed.is_empty() {
            bail!("no installed tools found. Specify a tool with the positional argument.");
        }
        if !is_json_mode() {
            println!(
                "Applying to all installed tools: {}\n",
                installed.join(", ")
            );
        }
        installed
    };

    print_dry_run_banner(args.dry_run);

    let mut total_applied = 0;
    let mut tool_results: Vec<serde_json::Value> = Vec::new();

    for tool_name in &tool_names {
        let adapter_instance = match adapter::get_adapter(tool_name) {
            Ok(Some(a)) => a,
            _ => continue,
        };
        if !adapter_instance.has_config_dir() {
            if args.tool.is_some() {
                bail!("{} config directory not found", tool_name);
            }
            continue;
        }

        let mut applied = 0;

        // Apply 前自动 sync
        if args.profile.is_some() {
            let auto_sync = store.load_config()?.auto_sync.unwrap_or(true);
            if auto_sync && !args.dry_run {
                if let Ok(result) = adapter_instance.sync(&store, false) {
                    if (!result.created.is_empty() || !result.updated.is_empty()) && !is_json_mode()
                    {
                        println!("Auto-syncing from {}...", tool_name);
                        if !result.created.is_empty() {
                            println!("  +{} created", result.created.len());
                        }
                        if !result.updated.is_empty() {
                            println!("  ~{} updated", result.updated.len());
                        }
                        println!();
                    }
                }
            }
        }

        // Profile 全量替换模式
        if let Some(ref pname) = args.profile {
            let profile = store.load_profile(pname)?;
            if !is_json_mode() {
                if tool_names.len() > 1 {
                    println!("[{}] Applying profile '{}'...", tool_name, pname);
                } else {
                    println!("Applying profile '{}' to {}...", pname, tool_name);
                }
            }
            applied += adapter_instance.apply_defaults(&store, &should_apply, args.dry_run)?;
            if should_apply("provider") {
                applied += adapter_instance.apply_provider(&store, &profile, args.dry_run)?;
            }
            applied += adapter_instance.apply_settings_batch(
                &store,
                &profile,
                args.dry_run,
                &should_apply,
            )?;
            if should_apply("skill") {
                applied += adapter_instance.apply_skill(&store, &profile, args.dry_run)?;
            }
            if should_apply("prompt") {
                applied += adapter_instance.apply_prompt(&store, &profile, args.dry_run)?;
            }
            if should_apply("agent") {
                applied += adapter_instance.apply_agent(&store, &profile, args.dry_run)?;
            }
            if should_apply("plugin") {
                applied += adapter_instance.apply_plugin(&store, &profile, args.dry_run)?;
            }
            tool_results.push(serde_json::json!({ "tool": tool_name, "mode": "profile", "profile": pname, "applied": applied }));
        }

        // 增量增删模式
        if !apply_all && !is_json_mode() {
            let skipped: Vec<&str> = args
                .resource_ops
                .iter()
                .filter(|(k, op)| (!op.add.is_empty() || !op.remove.is_empty()) && !should_apply(k))
                .map(|(k, _)| k.as_str())
                .collect();
            if !skipped.is_empty() {
                eprintln!(
                    "  warning: --only filter skipped {} operations: {}",
                    skipped.len(),
                    skipped.join(", ")
                );
            }
        }

        let mut incremental = 0;
        for (kind, op) in &args.resource_ops {
            if !should_apply(kind) {
                continue;
            }
            if !op.add.is_empty() {
                incremental +=
                    adapter_instance.add_resource(kind, &store, &op.add, args.dry_run)?;
            }
            if !op.remove.is_empty() {
                incremental +=
                    adapter_instance.remove_resource(kind, &store, &op.remove, args.dry_run)?;
            }
        }

        if incremental > 0 {
            if args.profile.is_none() && !is_json_mode() {
                if tool_names.len() > 1 {
                    println!("[{}] Applying incremental changes...", tool_name);
                } else {
                    println!("Applying incremental changes to {}...", tool_name);
                }
            }
            tool_results.push(serde_json::json!({ "tool": tool_name, "mode": "incremental", "applied": incremental }));
            applied += incremental;
        }

        if applied == 0 && !is_json_mode() {
            println!("  [{}] (nothing to apply)", tool_name);
        }
        total_applied += applied;
    }

    if is_json_mode() {
        output_json(
            &serde_json::json!({ "success": true, "profile": args.profile, "dry_run": args.dry_run, "total_applied": total_applied, "tools": tool_results }),
        );
    } else if args.dry_run {
        println!(
            "\n=== DRY RUN: {} resource(s) would be applied ===",
            total_applied
        );
    } else {
        println!("\nvcc: applied {} resource(s)", total_applied);
    }
    Ok(())
}
