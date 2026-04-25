use anyhow::{bail, Result};
use std::collections::HashSet;

use super::output::{is_json_mode, output_json, output_success};
use crate::store::TomlStore;

/// 处理 config 子命令
pub(crate) fn handle_subcommand(matches: &clap::ArgMatches) -> Result<()> {
    let store = TomlStore::new()?;

    match matches.subcommand() {
        Some(("list", _)) => {
            let config = store.load_config()?;
            let path = store.root().join("vcc.toml");
            if is_json_mode() {
                let mut val = serde_json::to_value(&config)?;
                if let Some(obj) = val.as_object_mut() {
                    obj.insert(
                        "_path".to_string(),
                        serde_json::Value::String(path.display().to_string()),
                    );
                }
                output_json(&val);
            } else {
                println!("# {}", path.display());
                println!();
                println!("{}", toml::to_string_pretty(&config)?);
            }
        }
        Some(("path", _)) => {
            let root = store.root();
            if is_json_mode() {
                output_json(&serde_json::json!({
                    "root": root.display().to_string(),
                    "config": root.join("vcc.toml").display().to_string(),
                    "registry": root.join("registry").display().to_string(),
                    "profiles": root.join("profiles").display().to_string(),
                    "cache": root.join("cache").display().to_string(),
                    "cache_skills": root.join("cache/skills").display().to_string(),
                    "cache_plugins": root.join("cache/plugins").display().to_string(),
                    "session_cache": root.join("session-cache.json").display().to_string(),
                }));
            } else {
                println!("root:            {}", root.display());
                println!("config:          {}", root.join("vcc.toml").display());
                println!("registry:        {}", root.join("registry").display());
                println!("profiles:        {}", root.join("profiles").display());
                println!("cache:           {}", root.join("cache").display());
                println!("cache/skills:    {}", root.join("cache/skills").display());
                println!("cache/plugins:   {}", root.join("cache/plugins").display());
                println!(
                    "session-cache:   {}",
                    root.join("session-cache.json").display()
                );
            }
        }
        Some(("get", m)) => {
            let key = m.get_one::<String>("key").unwrap().clone();
            let config = store.load_config()?;
            let value = get_config_value(&config, &key)?;
            if is_json_mode() {
                output_json(&serde_json::json!({key: value}));
            } else {
                println!("{}", value);
            }
        }
        Some(("set", m)) => {
            let key = m.get_one::<String>("key").unwrap().clone();
            let value = m.get_one::<String>("value").unwrap().clone();
            let mut config = store.load_config()?;
            set_config_value(&mut config, &key, &value)?;
            store.save_config(&config)?;
            output_success(&format!("vcc: config '{}' set to '{}'", key, value));
        }
        _ => bail!("unknown config subcommand. Run 'vcc config --help' for usage."),
    }
    Ok(())
}

fn get_config_value(config: &crate::store::toml_store::VccConfig, key: &str) -> Result<String> {
    match key {
        "auto_sync" => Ok(config
            .auto_sync
            .map_or("true".to_string(), |v| v.to_string())),
        "version" => Ok(config.version.clone()),
        "installed_tools" => Ok(config
            .installed_tools
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(",")),
        _ => bail!(
            "unknown config key: '{}'. Available: auto_sync, version, installed_tools",
            key
        ),
    }
}

fn set_config_value(
    config: &mut crate::store::toml_store::VccConfig,
    key: &str,
    value: &str,
) -> Result<()> {
    match key {
        "auto_sync" => {
            config.auto_sync = Some(value.parse::<bool>().map_err(|_| {
                anyhow::anyhow!("invalid boolean value: '{}'. Use true or false", value)
            })?);
        }
        "installed_tools" => {
            let tools: HashSet<String> = value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            config.installed_tools = tools;
        }
        "version" => bail!("'version' is read-only and cannot be changed"),
        _ => bail!(
            "unknown config key: '{}'. Available: auto_sync, installed_tools",
            key
        ),
    }
    Ok(())
}
