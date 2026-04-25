use anyhow::{bail, Result};
use std::collections::HashSet;

use super::output::{is_json_mode, output_json};
use crate::adapter;

pub(crate) fn run(tool: Option<&str>, only: Option<&str>) -> Result<()> {
    let valid_types = crate::config::resource_registry().all_kinds();

    let only_types: HashSet<String> = only
        .map(|s| super::parse_csv(s).into_iter().collect())
        .unwrap_or_default();

    // 验证 --only 值
    for t in &only_types {
        if !valid_types.contains(&t.as_str()) {
            bail!(
                "unknown resource type '{}'. Valid types: {}",
                t,
                valid_types.join(", ")
            );
        }
    }

    let filter_sections = |result: &mut adapter::InspectResult| {
        if only_types.is_empty() {
            return;
        }
        result.sections.retain(|s| only_types.contains(&s.kind));
    };

    if let Some(tool_name) = tool {
        // 指定工具
        let adapter_instance = adapter::get_adapter(tool_name)?.ok_or_else(|| {
            anyhow::anyhow!(
                "unsupported tool: '{}'. Supported: {}",
                tool_name,
                adapter::supported_tool_names()
            )
        })?;

        if !adapter_instance.has_config_dir() {
            bail!("{} config directory not found", tool_name);
        }

        let mut result = adapter_instance.inspect()?;
        filter_sections(&mut result);
        print_result(&result);
    } else {
        // 所有已安装工具
        let mut all_results: Vec<adapter::InspectResult> = Vec::new();
        for adapter_instance in adapter::all_adapters() {
            if !adapter_instance.has_config_dir() {
                continue;
            }
            let mut result = adapter_instance.inspect()?;
            filter_sections(&mut result);
            if !result.sections.is_empty() {
                all_results.push(result);
            }
        }

        if is_json_mode() {
            output_json(&serde_json::to_value(&all_results)?);
            return Ok(());
        }

        if all_results.is_empty() {
            println!("No installed tools found.");
            return Ok(());
        }

        for (i, result) in all_results.iter().enumerate() {
            if i > 0 {
                println!("\n{}\n", "─".repeat(60));
            }
            print_result(result);
        }
    }

    Ok(())
}

fn print_result(result: &adapter::InspectResult) {
    if is_json_mode() {
        output_json(&serde_json::to_value(result).unwrap_or_default());
        return;
    }

    println!("{} configuration", result.tool);
    if let Some(ref dir) = result.config_dir {
        println!("  config_dir: {}", dir);
    }
    println!();

    if result.sections.is_empty() {
        println!("  (no resources found)");
        return;
    }

    for section in &result.sections {
        println!("[{}]", section.kind);
        if section.items.is_empty() {
            println!("  (none)");
        } else {
            for item in &section.items {
                let status = if item.enabled { "ON" } else { "OFF" };
                println!("  {} [{}] {}", item.name, status, item.detail);
            }
        }
        println!();
    }
}
