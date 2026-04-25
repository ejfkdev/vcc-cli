use anyhow::{bail, Result};

use super::output::{is_json_mode, output_item, output_list, output_success};
use crate::model::profile::{
    Profile, ProfileModel, ProfilePrompts, ProfileProviders, ProfileResources,
};
use crate::store::TomlStore;

/// 从 ArgMatches 中提取可选字符串参数
fn opt_str(m: &clap::ArgMatches, name: &str) -> Option<String> {
    m.get_one::<String>(name).cloned()
}
/// 从 ArgMatches 中提取必需字符串参数
fn req_str(m: &clap::ArgMatches, name: &str) -> String {
    m.get_one::<String>(name).unwrap().clone()
}

/// 验证逗号分隔的资源名称列表是否存在
fn validate_refs(store: &TomlStore, kind: &str, csv: &str) -> Result<()> {
    for name in super::parse_csv(csv) {
        if !store.resource_exists(kind, &name) {
            bail!("{} '{}' not found", kind, name);
        }
    }
    Ok(())
}

/// 验证单个资源引用是否存在
fn validate_ref(store: &TomlStore, kind: &str, name: &str) -> Result<()> {
    if !store.resource_exists(kind, name) {
        bail!("{} '{}' not found", kind, name);
    }
    Ok(())
}

/// 验证可选的逗号分隔资源引用
fn validate_opt_refs(store: &TomlStore, kind: &str, csv: Option<&String>) -> Result<()> {
    if let Some(s) = csv {
        validate_refs(store, kind, s)?;
    }
    Ok(())
}

/// 验证可选的单个资源引用
fn validate_opt_ref(store: &TomlStore, kind: &str, name: Option<&String>) -> Result<()> {
    if let Some(s) = name {
        validate_ref(store, kind, s)?;
    }
    Ok(())
}

/// 处理 profile 子命令
pub(crate) fn handle_subcommand(matches: &clap::ArgMatches) -> Result<()> {
    let store = TomlStore::new()?;
    match matches.subcommand() {
        Some(("add", m)) => handle_add(&store, m),
        Some(("list", _)) => handle_list(&store),
        Some(("show", m)) => handle_show(&store, m),
        Some(("edit", m)) => handle_edit(&store, m),
        Some(("remove", m)) => {
            let name = req_str(m, "name");
            store.remove_profile(&name)?;
            output_success(&format!("vcc: profile '{}' removed", name));
            Ok(())
        }
        _ => bail!("unknown profile subcommand. Run 'vcc profile --help' for usage."),
    }
}

fn handle_add(store: &TomlStore, m: &clap::ArgMatches) -> Result<()> {
    let name = req_str(m, "name");
    if store.load_profile(&name).is_ok() {
        bail!("profile '{}' already exists", name);
    }
    let provider = opt_str(m, "provider");
    let model = opt_str(m, "model");
    let prompt = opt_str(m, "prompt");
    let description = opt_str(m, "description");

    // (kind, arg_name) → 提取并验证
    let res_csvs: [(&str, &str, Vec<String>); 6] = [
        (
            "mcp",
            "mcp",
            opt_str(m, "mcp")
                .map(|s| super::parse_csv(&s))
                .unwrap_or_default(),
        ),
        (
            "hook",
            "hook",
            opt_str(m, "hook")
                .map(|s| super::parse_csv(&s))
                .unwrap_or_default(),
        ),
        (
            "agent",
            "agent",
            opt_str(m, "agent")
                .map(|s| super::parse_csv(&s))
                .unwrap_or_default(),
        ),
        (
            "skill",
            "skill",
            opt_str(m, "skill")
                .map(|s| super::parse_csv(&s))
                .unwrap_or_default(),
        ),
        (
            "plugin",
            "plugin",
            opt_str(m, "plugin")
                .map(|s| super::parse_csv(&s))
                .unwrap_or_default(),
        ),
        (
            "env",
            "env",
            opt_str(m, "env")
                .map(|s| super::parse_csv(&s))
                .unwrap_or_default(),
        ),
    ];
    validate_opt_ref(store, "provider", provider.as_ref())?;
    validate_opt_ref(store, "prompt", prompt.as_ref())?;
    for &(kind, _, ref names) in &res_csvs {
        for n in names {
            validate_ref(store, kind, n)?;
        }
    }

    let profile = Profile {
        name: name.clone(),
        description,
        model: ProfileModel {
            default: model,
            weak_model: None,
            editor_model: None,
        },
        providers: ProfileProviders { default: provider },
        mcp_servers: ProfileResources {
            enabled: res_csvs[0].2.clone(),
        },
        hooks: ProfileResources {
            enabled: res_csvs[1].2.clone(),
        },
        agents: ProfileResources {
            enabled: res_csvs[2].2.clone(),
        },
        skills: ProfileResources {
            enabled: res_csvs[3].2.clone(),
        },
        plugins: ProfileResources {
            enabled: res_csvs[4].2.clone(),
        },
        env: ProfileResources {
            enabled: res_csvs[5].2.clone(),
        },
        prompts: ProfilePrompts { system: prompt },
        overrides: std::collections::HashMap::new(),
    };
    profile.validate()?;
    store.save_profile(&profile)?;
    output_success(&format!("vcc: profile '{}' added", name));
    Ok(())
}

fn handle_list(store: &TomlStore) -> Result<()> {
    let names = store.list_profiles()?;
    if names.is_empty() {
        if is_json_mode() {
            output_list(&Vec::<Profile>::new());
        } else {
            println!("No profiles configured.");
        }
        return Ok(());
    }
    let items: Vec<Profile> = names
        .iter()
        .filter_map(|n| store.load_profile(n).ok())
        .collect();
    if is_json_mode() {
        let result: Vec<serde_json::Value> = items
            .iter()
            .map(|p| serde_json::to_value(p).unwrap_or_default())
            .collect();
        output_list(&result);
    } else {
        println!("{:<25} DESCRIPTION", "NAME");
        println!("{}", "-".repeat(55));
        for p in &items {
            println!("{:<25} {}", p.name, p.description.as_deref().unwrap_or("-"));
        }
    }
    Ok(())
}

fn handle_show(store: &TomlStore, m: &clap::ArgMatches) -> Result<()> {
    let name = req_str(m, "name");
    let p = store.load_profile(&name)?;
    let path = store.root().join("profiles").join(format!("{}.toml", name));
    if is_json_mode() {
        let mut val = serde_json::to_value(&p)?;
        if let Some(obj) = val.as_object_mut() {
            obj.insert(
                "_path".to_string(),
                serde_json::Value::String(path.display().to_string()),
            );
        }
        output_item(&val);
    } else {
        println!("# {}", path.display());
        println!();
        println!("{}", toml::to_string_pretty(&p)?);
    }
    Ok(())
}

fn handle_edit(store: &TomlStore, m: &clap::ArgMatches) -> Result<()> {
    let name = req_str(m, "name");
    let provider = opt_str(m, "provider");
    let model = opt_str(m, "model");
    let description = opt_str(m, "description");
    let prompt = opt_str(m, "prompt");

    // 资源类型: (kind, set_arg, add_arg, remove_arg)
    #[allow(clippy::type_complexity)]
    let res_args: [(&str, Option<String>, Option<String>, Option<String>); 6] = [
        (
            "mcp",
            opt_str(m, "mcp"),
            opt_str(m, "add_mcp"),
            opt_str(m, "remove_mcp"),
        ),
        (
            "hook",
            opt_str(m, "hook"),
            opt_str(m, "add_hook"),
            opt_str(m, "remove_hook"),
        ),
        (
            "agent",
            opt_str(m, "agent"),
            opt_str(m, "add_agent"),
            opt_str(m, "remove_agent"),
        ),
        (
            "skill",
            opt_str(m, "skill"),
            opt_str(m, "add_skill"),
            opt_str(m, "remove_skill"),
        ),
        (
            "plugin",
            opt_str(m, "plugin"),
            opt_str(m, "add_plugin"),
            opt_str(m, "remove_plugin"),
        ),
        (
            "env",
            opt_str(m, "env"),
            opt_str(m, "add_env"),
            opt_str(m, "remove_env"),
        ),
    ];

    let has_args = provider.is_some()
        || model.is_some()
        || description.is_some()
        || prompt.is_some()
        || res_args
            .iter()
            .any(|(_, s, a, r)| s.is_some() || a.is_some() || r.is_some());

    if has_args {
        let mut profile = store.load_profile(&name)?;

        // 验证引用
        validate_opt_ref(store, "provider", provider.as_ref())?;
        validate_opt_ref(store, "prompt", prompt.as_ref())?;
        for &(kind, ref set, ref add, _) in &res_args {
            validate_opt_refs(store, kind, set.as_ref().or(add.as_ref()))?;
        }

        if let Some(p) = provider {
            profile.providers.default = Some(p);
        }
        if let Some(m) = model {
            profile.model.default = Some(m);
        }
        if let Some(p) = prompt {
            profile.prompts.system = Some(p);
        }
        if let Some(d) = description {
            profile.description = Some(d);
        }

        // 统一处理资源类型的 set/add/remove
        let fields: [(&str, &mut Vec<String>); 6] = [
            ("mcp", &mut profile.mcp_servers.enabled),
            ("hook", &mut profile.hooks.enabled),
            ("agent", &mut profile.agents.enabled),
            ("skill", &mut profile.skills.enabled),
            ("plugin", &mut profile.plugins.enabled),
            ("env", &mut profile.env.enabled),
        ];
        for (i, (_, field)) in fields.into_iter().enumerate() {
            let (_, set, add, remove) = &res_args[i];
            if let Some(s) = set {
                *field = super::parse_csv(s);
            }
            super::add_remove_vec(field, add.as_deref(), remove.as_deref());
        }

        profile.validate()?;
        store.save_profile(&profile)?;
        output_success(&format!("vcc: profile '{}' updated", name));
    } else {
        let path = store.root().join("profiles").join(format!("{}.toml", name));
        if !path.exists() {
            bail!("profile '{}' not found", name);
        }
        println!("{}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_refs_empty_csv() {
        let store = TomlStore::new().unwrap();
        assert!(validate_refs(&store, "mcp", "").is_ok());
    }

    #[test]
    fn test_validate_refs_nonexistent() {
        let store = TomlStore::new().unwrap();
        assert!(validate_refs(&store, "mcp", "nonexistent-mcp-xyz").is_err());
    }

    #[test]
    fn test_validate_opt_refs_none() {
        let store = TomlStore::new().unwrap();
        assert!(validate_opt_refs(&store, "mcp", None).is_ok());
    }

    #[test]
    fn test_opt_str_and_req_str() {
        let app = clap::Command::new("test")
            .arg(clap::Arg::new("name").long("name"))
            .arg(clap::Arg::new("count").long("count").required(true));
        let m = app.try_get_matches_from(["test", "--count", "5"]).unwrap();
        assert_eq!(opt_str(&m, "name"), None);
        assert_eq!(req_str(&m, "count"), "5");
    }
}
