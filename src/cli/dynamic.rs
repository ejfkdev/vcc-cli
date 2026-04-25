use anyhow::Result;
use std::collections::HashMap;

use crate::config::{resource_registry, CliFieldDef};

#[derive(Debug, Clone)]
pub(crate) enum FieldValue {
    String(Option<String>),
    U64(Option<u64>),
    F64(Option<f64>),
    Bool(Option<bool>),
    Csv(Option<Vec<String>>),
    KvVec(HashMap<String, String>),
    ToolSet(HashMap<String, HashMap<String, String>>),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FieldMap {
    inner: HashMap<String, FieldValue>,
}

impl FieldMap {
    pub fn get_string(&self, name: &str) -> Option<String> {
        if let Some(FieldValue::String(v)) = self.inner.get(name) {
            v.clone()
        } else {
            None
        }
    }
    pub fn get_u64(&self, name: &str) -> Option<u64> {
        if let Some(FieldValue::U64(v)) = self.inner.get(name) {
            *v
        } else {
            None
        }
    }
    pub fn get_f64(&self, name: &str) -> Option<f64> {
        if let Some(FieldValue::F64(v)) = self.inner.get(name) {
            *v
        } else {
            None
        }
    }
    pub fn get_csv(&self, name: &str) -> Option<Vec<String>> {
        if let Some(FieldValue::Csv(v)) = self.inner.get(name) {
            v.clone()
        } else {
            None
        }
    }
    pub fn get_kvvec(&self, name: &str) -> HashMap<String, String> {
        if let Some(FieldValue::KvVec(v)) = self.inner.get(name) {
            v.clone()
        } else {
            HashMap::new()
        }
    }
    pub fn get_content_or_file(
        &self,
        content_name: &str,
        file_name: &str,
    ) -> Result<Option<String>> {
        if let Some(content) = self.get_string(content_name) {
            return Ok(Some(content));
        }
        if let Some(file_path) = self.get_string(file_name) {
            return Ok(Some(std::fs::read_to_string(&file_path)?));
        }
        Ok(None)
    }
    pub fn was_provided(&self, name: &str) -> bool {
        match self.inner.get(name) {
            Some(FieldValue::String(v)) => v.is_some(),
            Some(FieldValue::U64(v)) => v.is_some(),
            Some(FieldValue::F64(v)) => v.is_some(),
            Some(FieldValue::Bool(v)) => v.is_some(),
            Some(FieldValue::Csv(v)) => v.is_some(),
            Some(FieldValue::KvVec(v)) => !v.is_empty(),
            Some(FieldValue::ToolSet(v)) => !v.is_empty(),
            None => false,
        }
    }
    pub fn get_toolset(&self, name: &str) -> HashMap<String, HashMap<String, String>> {
        if let Some(FieldValue::ToolSet(v)) = self.inner.get(name) {
            v.clone()
        } else {
            HashMap::new()
        }
    }
    pub fn insert(&mut self, name: String, value: FieldValue) {
        self.inner.insert(name, value);
    }
}

#[derive(Debug)]
pub(crate) enum ResourceAction {
    Add {
        name: String,
        fields: FieldMap,
    },
    List,
    Show {
        query: String,
    },
    Edit {
        query: String,
        fields: FieldMap,
    },
    Remove {
        query: String,
    },
    Enable {
        name: String,
        tool: Option<String>,
        dry_run: bool,
    },
    Disable {
        name: String,
        tool: Option<String>,
        dry_run: bool,
    },
    Install {
        name: String,
        branch: Option<String>,
        dry_run: bool,
    },
}

pub(crate) fn augment_fixed_subcommands(app: clap::Command) -> clap::Command {
    app.subcommand(build_profile_command())
        .subcommand(build_config_command())
        .subcommand(build_session_command())
}

fn build_profile_command() -> clap::Command {
    let name_help = "Profile 名称（可通过 `vcc profile list` 查看）";
    let after_help = "Examples:\n  vcc profile add work -p anthropic                              # 指定 provider\n  vcc profile add dev -p openai --model gpt-4o -m fs             # 指定模型和 MCP\n  vcc profile add full -p anthropic -m fs,icm -H check           # 带 MCP 和 Hook\n  vcc profile list                                             # 列出所有 profile\n  vcc profile show work                                        # 查看 profile 详情\n  vcc profile edit work --add-mcp fs --add-mcp icm            # 增量添加 MCP\n  vcc profile edit work --model gpt-4o                          # 修改默认模型\n  vcc profile remove work                                      # 删除 profile";
    let mut cmd = clap::Command::new("profile")
        .about("管理 Profile")
        .after_help(after_help)
        .subcommand_required(true);
    let mut add_cmd = clap::Command::new("add").about("添加 Profile").arg(
        clap::Arg::new("name")
            .required(true)
            .help("Profile 名称（只能包含字母、数字、- 和 _）"),
    );
    for (name, short, help) in [
        ("provider", Some('p'), "默认 Provider"),
        ("model", None, "默认 Model"),
        ("mcp", Some('m'), "启用的 MCP Server (逗号分隔)"),
        ("hook", Some('H'), "启用的 Hook (逗号分隔)"),
        ("agent", Some('a'), "启用的 Agent (逗号分隔)"),
        ("skill", Some('s'), "启用的 Skill (逗号分隔)"),
        ("prompt", None, "系统 Prompt"),
        ("env", Some('e'), "包含的环境变量组 (逗号分隔)"),
        ("plugin", None, "启用的 Plugin (逗号分隔)"),
        ("description", Some('d'), "描述"),
    ] {
        let mut arg = clap::Arg::new(name).long(name).help(help);
        if let Some(s) = short {
            arg = arg.short(s);
        }
        add_cmd = add_cmd.arg(arg);
    }
    cmd = cmd.subcommand(add_cmd);
    let mut edit_cmd = clap::Command::new("edit")
        .about("编辑 Profile（不带参数则显示配置文件路径）")
        .arg(clap::Arg::new("name").required(true).help(name_help));
    for (name, short, help) in [
        ("provider", Some('p'), "默认 Provider"),
        ("model", None, "默认 Model"),
        ("mcp", Some('m'), "启用的 MCP Server (逗号分隔，覆盖原有)"),
        ("add_mcp", None, "增加的 MCP Server (逗号分隔)"),
        ("remove_mcp", None, "移除的 MCP Server (逗号分隔)"),
        ("hook", Some('H'), "启用的 Hook (逗号分隔，覆盖原有)"),
        ("add_hook", None, "增加的 Hook (逗号分隔)"),
        ("remove_hook", None, "移除的 Hook (逗号分隔)"),
        ("agent", Some('a'), "启用的 Agent (逗号分隔，覆盖原有)"),
        ("add_agent", None, "增加的 Agent (逗号分隔)"),
        ("remove_agent", None, "移除的 Agent (逗号分隔)"),
        ("skill", Some('s'), "启用的 Skill (逗号分隔，覆盖原有)"),
        ("add_skill", None, "增加的 Skill (逗号分隔)"),
        ("remove_skill", None, "移除的 Skill (逗号分隔)"),
        ("plugin", None, "启用的 Plugin (逗号分隔，覆盖原有)"),
        ("add_plugin", None, "增加的 Plugin (逗号分隔)"),
        ("remove_plugin", None, "移除的 Plugin (逗号分隔)"),
        ("prompt", None, "系统 Prompt"),
        ("env", Some('e'), "包含的环境变量组 (逗号分隔，覆盖原有)"),
        ("add_env", None, "增加的环境变量组 (逗号分隔)"),
        ("remove_env", None, "移除的环境变量组 (逗号分隔)"),
        ("description", Some('d'), "描述"),
    ] {
        let mut arg = clap::Arg::new(name).long(name.replace('_', "-")).help(help);
        if let Some(s) = short {
            arg = arg.short(s);
        }
        edit_cmd = edit_cmd.arg(arg);
    }
    cmd.subcommand(edit_cmd)
        .subcommand(clap::Command::new("list").about("列出所有 Profile"))
        .subcommand(
            clap::Command::new("show")
                .about("显示 Profile 详情")
                .arg(clap::Arg::new("name").required(true).help(name_help)),
        )
        .subcommand(
            clap::Command::new("remove")
                .about("删除 Profile")
                .arg(clap::Arg::new("name").required(true).help(name_help)),
        )
}

fn build_config_command() -> clap::Command {
    clap::Command::new("config").about("管理 vcc 自身配置").after_help("Examples:\n  vcc config list                              # 显示所有配置\n  vcc config path                              # 显示所有路径\n  vcc config get auto_sync                     # 获取配置项\n  vcc config set auto_sync true                # 设置配置项").subcommand_required(true)
        .subcommand(clap::Command::new("list").about("显示所有配置")).subcommand(clap::Command::new("path").about("显示配置文件路径"))
        .subcommand(clap::Command::new("get").about("获取配置项").arg(clap::Arg::new("key").required(true).help("配置项名称 (auto_sync, installed_tools)")))
        .subcommand(clap::Command::new("set").about("设置配置项").arg(clap::Arg::new("key").required(true).help("配置项名称")).arg(clap::Arg::new("value").required(true).help("配置项值")))
}

fn build_session_command() -> clap::Command {
    clap::Command::new("session").about("管理会话").after_help("Examples:\n  vcc session list                             # 列出所有会话\n  vcc session list -t claude                   # 只列出 claude 会话\n  vcc session show abc123                      # 查看会话详情\n  vcc session remove abc123                    # 删除会话").subcommand_required(true)
        .subcommand(clap::Command::new("list").about("列出会话").arg(clap::Arg::new("tool").short('t').long("tool").help("目标工具 (claude/codex/gemini/opencode/kimi)")))
        .subcommand(clap::Command::new("show").about("显示会话详情").arg(clap::Arg::new("id").required(true).help("会话 ID")).arg(clap::Arg::new("tool").short('t').long("tool").help("工具名称")))
        .subcommand(clap::Command::new("remove").about("删除会话").arg(clap::Arg::new("id").required(true).help("会话 ID")).arg(clap::Arg::new("tool").short('t').long("tool").help("工具名称")))
}

pub(crate) fn augment_resource_commands(app: clap::Command) -> clap::Command {
    let registry = resource_registry();
    let mut cmd = app;
    for res_cfg in &registry.resources {
        cmd = cmd.subcommand(build_resource_command(res_cfg));
    }
    cmd
}

fn build_resource_command(res_cfg: &crate::config::ResourceConfig) -> clap::Command {
    let kind = &res_cfg.kind;
    let query_help = format!(
        "{} 名称、文件名(name-hash8)或唯一 ID（可通过 `vcc {} list` 查看）",
        kind, kind
    );
    let name_help = format!("{} 名称（可通过 `vcc {} list` 查看）", kind, kind);
    let mut cmd = clap::Command::new(kind.clone())
        .about(format!("管理 {}", kind))
        .after_help(build_examples(kind, res_cfg))
        .subcommand_required(true);
    let mut add_cmd = clap::Command::new("add")
        .about(format!("添加 {}", kind))
        .arg(
            clap::Arg::new("name")
                .required(true)
                .help("名称（只能包含字母、数字、- 和 _）"),
        );
    for field in &res_cfg.cli_fields {
        if !field.edit_only {
            add_cmd = add_cmd.arg(build_arg(field, true));
        }
    }
    cmd = cmd.subcommand(add_cmd);
    let mut edit_cmd = clap::Command::new("edit")
        .about(format!("编辑 {}", kind))
        .arg(clap::Arg::new("query").required(true).help(&query_help));
    for field in &res_cfg.cli_fields {
        if !field.add_only {
            edit_cmd = edit_cmd.arg(build_arg(field, false));
        }
    }
    cmd = cmd.subcommand(edit_cmd);
    cmd = cmd
        .subcommand(clap::Command::new("list").about(format!("列出所有 {}", kind)))
        .subcommand(
            clap::Command::new("show")
                .about(format!("显示 {} 详情", kind))
                .arg(clap::Arg::new("query").required(true).help(&query_help)),
        )
        .subcommand(
            clap::Command::new("remove")
                .about(format!("删除 {}", kind))
                .arg(clap::Arg::new("query").required(true).help(&query_help)),
        );
    for (sub_name, sub_about, extra_arg) in [
        (
            "enable",
            format!("启用 {}", kind),
            Some(
                clap::Arg::new("tool")
                    .short('t')
                    .long("tool")
                    .help("目标工具"),
            ),
        ),
        (
            "disable",
            format!("禁用 {}", kind),
            Some(
                clap::Arg::new("tool")
                    .short('t')
                    .long("tool")
                    .help("目标工具"),
            ),
        ),
        (
            "install",
            format!("安装 {}", kind),
            Some(
                clap::Arg::new("branch")
                    .short('b')
                    .long("branch")
                    .help("Git 分支或标签"),
            ),
        ),
    ] {
        if !res_cfg.extra_subcommands.contains(&sub_name.to_string()) {
            continue;
        }
        let mut sub = clap::Command::new(sub_name)
            .about(sub_about)
            .arg(clap::Arg::new("name").required(true).help(&name_help));
        if let Some(arg) = extra_arg {
            sub = sub.arg(arg);
        }
        cmd = cmd.subcommand(
            sub.arg(
                clap::Arg::new("dry_run")
                    .long("dry-run")
                    .action(clap::ArgAction::SetTrue)
                    .help("模拟运行"),
            ),
        );
    }
    cmd
}

fn build_examples(kind: &str, res_cfg: &crate::config::ResourceConfig) -> String {
    let mut l = vec!["Examples:".to_string()];
    if !res_cfg.examples.is_empty() {
        l.extend(res_cfg.examples.iter().cloned());
    } else {
        l.extend([
            format!(
                "  vcc {} add my-{}                                          # 添加",
                kind, kind
            ),
            format!(
                "  vcc {} list                                               # 列出所有",
                kind
            ),
            format!(
                "  vcc {} show my-{}                                         # 查看详情",
                kind, kind
            ),
            format!(
                "  vcc {} edit my-{}                                         # 编辑",
                kind, kind
            ),
            format!(
                "  vcc {} remove my-{}                                       # 删除",
                kind, kind
            ),
        ]);
    }
    let extras = &res_cfg.extra_subcommands;
    if extras.contains(&"enable".to_string()) || extras.contains(&"disable".to_string()) {
        let en = match kind {
            "mcp" => "fs",
            "plugin" => "my-plugin",
            _ => "my-resource",
        };
        l.push(format!(
            "  vcc {} enable {} -t claude                             # 在指定工具中启用",
            kind, en
        ));
        l.push(format!(
            "  vcc {} disable {}                                      # 在所有工具中禁用",
            kind, en
        ));
    }
    if extras.contains(&"install".to_string()) {
        let en = match kind {
            "skill" => "my-skill",
            "plugin" => "my-plugin",
            _ => "my-resource",
        };
        l.push(format!(
            "  vcc {} install {}                                      # 安装到本地",
            kind, en
        ));
        l.push(format!(
            "  vcc {} install {} -b v1.0 --dry-run                    # 指定分支，模拟安装",
            kind, en
        ));
    }
    l.join("\n")
}

fn build_arg(field: &CliFieldDef, is_add: bool) -> clap::Arg {
    let long_name = field.name.replace('_', "-");
    let mut arg = clap::Arg::new(field.name.clone())
        .long(long_name)
        .help(field.help.clone());
    if let Some(short) = field.short {
        arg = arg.short(short);
    }
    if is_add && field.required_for_add && field.default_value.is_none() {
        arg = arg.required(true);
    }
    if is_add {
        if let Some(ref default) = field.default_value {
            arg = arg.default_value(default.clone());
        }
    }
    match field.field_type.as_str() {
        "u64" => {
            arg = arg.value_parser(clap::value_parser!(u64));
        }
        "f64" => {
            arg = arg.value_parser(clap::value_parser!(f64));
        }
        "bool" => {
            arg = arg.value_parser(clap::value_parser!(bool));
        }
        "kvvec" | "toolset" => {
            arg = arg.action(clap::ArgAction::Append);
        }
        "csv" => {
            // CSV values like "-y,mcp-server" must allow leading hyphens
            arg = arg.allow_hyphen_values(true);
        }
        _ => {
            if !field.possible_values.is_empty() {
                arg = arg.value_parser(clap::builder::PossibleValuesParser::new(
                    field.possible_values.clone(),
                ));
            }
        }
    }
    arg
}

pub(crate) fn extract_field_map(
    matches: &clap::ArgMatches,
    fields: &[CliFieldDef],
    is_add: bool,
) -> FieldMap {
    let mut map = FieldMap::default();
    for field in fields {
        if !is_add && field.add_only {
            continue;
        }
        if is_add && field.edit_only {
            continue;
        }
        match field.field_type.as_str() {
            "string" | "content" | "file" => {
                map.insert(
                    field.name.clone(),
                    FieldValue::String(matches.get_one::<String>(&field.name).cloned()),
                );
            }
            "u64" => {
                map.insert(
                    field.name.clone(),
                    FieldValue::U64(matches.get_one::<u64>(&field.name).copied()),
                );
            }
            "f64" => {
                map.insert(
                    field.name.clone(),
                    FieldValue::F64(matches.get_one::<f64>(&field.name).copied()),
                );
            }
            "bool" => {
                map.insert(
                    field.name.clone(),
                    FieldValue::Bool(matches.get_one::<bool>(&field.name).copied()),
                );
            }
            "csv" => {
                map.insert(
                    field.name.clone(),
                    FieldValue::Csv(
                        matches
                            .get_one::<String>(&field.name)
                            .map(|v| super::parse_csv(v)),
                    ),
                );
            }
            "kvvec" => {
                let vals: Vec<String> = matches
                    .get_many::<String>(&field.name)
                    .map(|v| v.cloned().collect())
                    .unwrap_or_default();
                map.insert(field.name.clone(), FieldValue::KvVec(parse_var_args(&vals)));
            }
            "toolset" => {
                let vals: Vec<String> = matches
                    .get_many::<String>(&field.name)
                    .map(|v| v.cloned().collect())
                    .unwrap_or_default();
                map.insert(
                    field.name.clone(),
                    FieldValue::ToolSet(parse_toolset_args(&vals)),
                );
            }
            _ => {}
        }
    }
    map
}

fn parse_var_args(vars: &[String]) -> HashMap<String, String> {
    vars.iter()
        .map(|kv| {
            let mut p = kv.trim().splitn(2, '=');
            (
                p.next().unwrap().trim().to_string(),
                p.next().map(|v| v.trim().to_string()).unwrap_or_default(),
            )
        })
        .collect()
}

fn parse_toolset_args(vars: &[String]) -> HashMap<String, HashMap<String, String>> {
    let mut result: HashMap<String, HashMap<String, String>> = HashMap::new();
    for arg in vars {
        let arg = arg.trim();
        if let Some((tool_part, kv_part)) = arg.split_once(':') {
            let tool = tool_part.trim().to_string();
            let mut kv_iter = kv_part.trim().splitn(2, '=');
            let key = kv_iter.next().unwrap_or("").trim().to_string();
            let value = kv_iter
                .next()
                .map(|v| v.trim().to_string())
                .unwrap_or_default();
            if !tool.is_empty() && !key.is_empty() {
                result.entry(tool).or_default().insert(key, value);
            }
        }
    }
    result
}

pub(crate) fn parse_resource_command(
    kind: &str,
    matches: &clap::ArgMatches,
) -> Result<ResourceAction> {
    let registry = resource_registry();
    let res_cfg = registry
        .resource_for(kind)
        .ok_or_else(|| anyhow::anyhow!("unknown resource kind: {}", kind))?;
    match matches.subcommand() {
        Some(("add", m)) => Ok(ResourceAction::Add {
            name: m.get_one::<String>("name").unwrap().clone(),
            fields: extract_field_map(m, &res_cfg.cli_fields, true),
        }),
        Some(("edit", m)) => Ok(ResourceAction::Edit {
            query: m.get_one::<String>("query").unwrap().clone(),
            fields: extract_field_map(m, &res_cfg.cli_fields, false),
        }),
        Some(("list", _)) => Ok(ResourceAction::List),
        Some(("show", m)) => Ok(ResourceAction::Show {
            query: m.get_one::<String>("query").unwrap().clone(),
        }),
        Some(("remove", m)) => Ok(ResourceAction::Remove {
            query: m.get_one::<String>("query").unwrap().clone(),
        }),
        Some(("enable", m)) => Ok(ResourceAction::Enable {
            name: m.get_one::<String>("name").unwrap().clone(),
            tool: m.get_one::<String>("tool").cloned(),
            dry_run: m.get_flag("dry_run"),
        }),
        Some(("disable", m)) => Ok(ResourceAction::Disable {
            name: m.get_one::<String>("name").unwrap().clone(),
            tool: m.get_one::<String>("tool").cloned(),
            dry_run: m.get_flag("dry_run"),
        }),
        Some(("install", m)) => Ok(ResourceAction::Install {
            name: m.get_one::<String>("name").unwrap().clone(),
            branch: m.get_one::<String>("branch").cloned(),
            dry_run: m.get_flag("dry_run"),
        }),
        _ => anyhow::bail!(
            "unknown subcommand for '{}'. Run 'vcc {} --help' for usage.",
            kind,
            kind
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_var_args ──

    #[test]
    fn test_parse_var_args_basic() {
        let args = vec!["KEY1=VAL1".into(), "KEY2=VAL2".into()];
        let map = parse_var_args(&args);
        assert_eq!(map.get("KEY1").unwrap(), "VAL1");
        assert_eq!(map.get("KEY2").unwrap(), "VAL2");
    }

    #[test]
    fn test_parse_var_args_no_value() {
        let args = vec!["KEY=".into()];
        let map = parse_var_args(&args);
        assert_eq!(map.get("KEY").unwrap(), "");
    }

    #[test]
    fn test_parse_var_args_value_with_equals() {
        let args = vec!["KEY=val=ue".into()];
        let map = parse_var_args(&args);
        assert_eq!(map.get("KEY").unwrap(), "val=ue");
    }

    #[test]
    fn test_parse_var_args_empty() {
        let args: Vec<String> = vec![];
        let map = parse_var_args(&args);
        assert!(map.is_empty());
    }

    // ── parse_toolset_args ──

    #[test]
    fn test_parse_toolset_args_basic() {
        let args = vec!["claude:api_key=sk-123".into()];
        let map = parse_toolset_args(&args);
        assert_eq!(map.get("claude").unwrap().get("api_key").unwrap(), "sk-123");
    }

    #[test]
    fn test_parse_toolset_args_multiple_tools() {
        let args = vec!["claude:key1=val1".into(), "gemini:key2=val2".into()];
        let map = parse_toolset_args(&args);
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("claude").unwrap().get("key1").unwrap(), "val1");
        assert_eq!(map.get("gemini").unwrap().get("key2").unwrap(), "val2");
    }

    #[test]
    fn test_parse_toolset_args_empty() {
        let args: Vec<String> = vec![];
        let map = parse_toolset_args(&args);
        assert!(map.is_empty());
    }

    #[test]
    fn test_parse_toolset_args_no_colon() {
        let args = vec!["no_colon_key=value".into()];
        let map = parse_toolset_args(&args);
        assert!(map.is_empty());
    }

    // ── FieldMap ──

    #[test]
    fn test_field_map_get_string() {
        let mut fm = FieldMap::default();
        fm.insert("name".into(), FieldValue::String(Some("test".into())));
        assert_eq!(fm.get_string("name"), Some("test".into()));
        assert_eq!(fm.get_string("missing"), None);
    }

    #[test]
    fn test_field_map_get_u64() {
        let mut fm = FieldMap::default();
        fm.insert("count".into(), FieldValue::U64(Some(42)));
        assert_eq!(fm.get_u64("count"), Some(42));
    }

    #[test]
    fn test_field_map_get_f64() {
        let mut fm = FieldMap::default();
        fm.insert("temp".into(), FieldValue::F64(Some(0.7)));
        assert_eq!(fm.get_f64("temp"), Some(0.7));
    }

    #[test]
    fn test_field_map_get_csv() {
        let mut fm = FieldMap::default();
        fm.insert(
            "items".into(),
            FieldValue::Csv(Some(vec!["a".into(), "b".into()])),
        );
        assert_eq!(fm.get_csv("items"), Some(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn test_field_map_get_kvvec() {
        let mut fm = FieldMap::default();
        fm.insert(
            "var".into(),
            FieldValue::KvVec(HashMap::from([("KEY".into(), "VAL".into())])),
        );
        let kv = fm.get_kvvec("var");
        assert_eq!(kv.get("KEY").unwrap(), "VAL");
    }

    #[test]
    fn test_field_map_was_provided_string() {
        let mut fm = FieldMap::default();
        fm.insert("name".into(), FieldValue::String(None));
        assert!(!fm.was_provided("name"));
        fm.insert("name".into(), FieldValue::String(Some("val".into())));
        assert!(fm.was_provided("name"));
    }

    #[test]
    fn test_field_map_was_provided_missing() {
        let fm = FieldMap::default();
        assert!(!fm.was_provided("anything"));
    }

    #[test]
    fn test_field_map_was_provided_kvvec() {
        let mut fm = FieldMap::default();
        fm.insert("var".into(), FieldValue::KvVec(HashMap::new()));
        assert!(!fm.was_provided("var"));
        fm.insert(
            "var".into(),
            FieldValue::KvVec(HashMap::from([("K".into(), "V".into())])),
        );
        assert!(fm.was_provided("var"));
    }

    #[test]
    fn test_field_map_get_toolset() {
        let mut fm = FieldMap::default();
        fm.insert(
            "tool_set".into(),
            FieldValue::ToolSet(HashMap::from([(
                "claude".into(),
                HashMap::from([("key".into(), "val".into())]),
            )])),
        );
        let ts = fm.get_toolset("tool_set");
        assert!(ts.contains_key("claude"));
    }

    #[test]
    fn test_field_map_wrong_type_returns_none() {
        let mut fm = FieldMap::default();
        fm.insert("name".into(), FieldValue::String(Some("test".into())));
        assert_eq!(fm.get_u64("name"), None);
        assert_eq!(fm.get_f64("name"), None);
        assert_eq!(fm.get_csv("name"), None);
    }

    // ── FieldMap: U64 wrong type ──

    #[test]
    fn test_field_map_u64_wrong_type() {
        let mut fm = FieldMap::default();
        fm.insert("count".into(), FieldValue::U64(Some(42)));
        assert_eq!(fm.get_string("count"), None);
    }

    #[test]
    fn test_field_map_csv_wrong_type() {
        let mut fm = FieldMap::default();
        fm.insert("items".into(), FieldValue::Csv(Some(vec!["a".into()])));
        assert_eq!(fm.get_u64("items"), None);
    }

    #[test]
    fn test_field_map_string_none() {
        let mut fm = FieldMap::default();
        fm.insert("name".into(), FieldValue::String(None));
        assert_eq!(fm.get_string("name"), None);
    }

    #[test]
    fn test_field_map_u64_none() {
        let mut fm = FieldMap::default();
        fm.insert("count".into(), FieldValue::U64(None));
        assert!(!fm.was_provided("count"));
        assert_eq!(fm.get_u64("count"), None);
    }

    #[test]
    fn test_field_map_f64_none() {
        let mut fm = FieldMap::default();
        fm.insert("temp".into(), FieldValue::F64(None));
        assert_eq!(fm.get_f64("temp"), None);
    }

    // ── parse_var_args additional ──

    #[test]
    fn test_parse_var_args_no_equals() {
        let args = vec!["NOEQUALS".into()];
        let map = parse_var_args(&args);
        // No '=' in arg → empty key with full value? Or skipped?
        // Actually splitn(2, '=') on "NOEQUALS" returns ["NOEQUALS"]
        // which means key="NOEQUALS", value="" due to unwrap_or
        assert!(map.contains_key("NOEQUALS"));
    }

    #[test]
    fn test_parse_var_args_multiple_equals() {
        let args = vec!["URL=http://a=b=c".into()];
        let map = parse_var_args(&args);
        assert_eq!(map.get("URL").unwrap(), "http://a=b=c");
    }

    // ── parse_toolset_args additional ──

    #[test]
    fn test_parse_toolset_args_multiple_keys_per_tool() {
        let args = vec!["claude:key1=val1".into(), "claude:key2=val2".into()];
        let map = parse_toolset_args(&args);
        assert_eq!(map.len(), 1);
        let claude = map.get("claude").unwrap();
        assert_eq!(claude.len(), 2);
        assert_eq!(claude.get("key1").unwrap(), "val1");
        assert_eq!(claude.get("key2").unwrap(), "val2");
    }

    #[test]
    fn test_parse_toolset_args_value_with_equals() {
        let args = vec!["claude:api_key=sk=key".into()];
        let map = parse_toolset_args(&args);
        assert_eq!(map.get("claude").unwrap().get("api_key").unwrap(), "sk=key");
    }

    // ── FieldMap: was_provided for Csv ──

    #[test]
    fn test_field_map_was_provided_csv_some() {
        let mut fm = FieldMap::default();
        fm.insert("items".into(), FieldValue::Csv(Some(vec!["a".into()])));
        assert!(fm.was_provided("items"));
    }

    #[test]
    fn test_field_map_was_provided_csv_none() {
        let mut fm = FieldMap::default();
        fm.insert("items".into(), FieldValue::Csv(None));
        assert!(!fm.was_provided("items"));
    }

    // ── parse_var_args edge cases ──

    #[test]
    fn test_parse_var_args_value_with_spaces_and_equals() {
        let result = parse_var_args(&["CONN=host=db port=5432".to_string()]);
        assert_eq!(result.get("CONN").unwrap(), "host=db port=5432");
    }

    #[test]
    fn test_parse_var_args_bare_key_no_equals() {
        let result = parse_var_args(&["NOVALUE".to_string()]);
        assert_eq!(result.get("NOVALUE").unwrap(), "");
    }

    #[test]
    fn test_parse_var_args_explicit_empty_value() {
        let result = parse_var_args(&["EMPTY=".to_string()]);
        assert_eq!(result.get("EMPTY").unwrap(), "");
    }

    #[test]
    fn test_parse_var_args_mixed_key_formats() {
        let result = parse_var_args(&[
            "KEY1=val1".to_string(),
            "KEY2=val2".to_string(),
            "KEY3=".to_string(),
        ]);
        assert_eq!(result.len(), 3);
        assert_eq!(result.get("KEY1").unwrap(), "val1");
        assert_eq!(result.get("KEY2").unwrap(), "val2");
        assert_eq!(result.get("KEY3").unwrap(), "");
    }

    // ── edit_only / add_only filtering in extract_field_map ──

    #[test]
    fn test_extract_field_map_skips_add_only_when_edit() {
        // Simulate: edit command should not see add_only fields like "preset"
        let fields = vec![
            CliFieldDef {
                name: "preset".into(),
                short: Some('p'),
                help: "Preset".into(),
                field_type: "string".into(),
                default_value: None,
                required_for_add: false,
                add_only: true,
                edit_only: false,
                preset_fill: false,
                preset_trigger: false,
                is_metadata: false,
                sensitive: false,
                possible_values: vec![],
            },
            CliFieldDef {
                name: "type".into(),
                short: Some('t'),
                help: "Type".into(),
                field_type: "string".into(),
                default_value: None,
                required_for_add: false,
                add_only: false,
                edit_only: false,
                preset_fill: false,
                preset_trigger: false,
                is_metadata: false,
                sensitive: false,
                possible_values: vec![],
            },
        ];
        // is_add=false (edit mode): should skip "preset"
        let matches = clap::Command::new("test")
            .arg(clap::Arg::new("preset").long("preset"))
            .arg(clap::Arg::new("type").long("type"))
            .try_get_matches_from(["test", "--type", "stdio"])
            .unwrap();
        let map = extract_field_map(&matches, &fields, false);
        assert!(!map.was_provided("preset"));
        assert!(map.was_provided("type"));
    }

    #[test]
    fn test_extract_field_map_skips_edit_only_when_add() {
        // Simulate: add command should not see edit_only fields like "remove_var"
        let fields = vec![
            CliFieldDef {
                name: "remove_var".into(),
                short: Some('R'),
                help: "Remove var".into(),
                field_type: "csv".into(),
                default_value: None,
                required_for_add: false,
                add_only: false,
                edit_only: true,
                preset_fill: false,
                preset_trigger: false,
                is_metadata: false,
                sensitive: false,
                possible_values: vec![],
            },
            CliFieldDef {
                name: "var".into(),
                short: Some('v'),
                help: "Var".into(),
                field_type: "kvvec".into(),
                default_value: None,
                required_for_add: false,
                add_only: false,
                edit_only: false,
                preset_fill: false,
                preset_trigger: false,
                is_metadata: false,
                sensitive: false,
                possible_values: vec![],
            },
        ];
        // is_add=true: should skip "remove_var"
        let matches = clap::Command::new("test")
            .arg(clap::Arg::new("remove_var").long("remove-var"))
            .arg(
                clap::Arg::new("var")
                    .short('v')
                    .long("var")
                    .action(clap::ArgAction::Append),
            )
            .try_get_matches_from(["test", "--var", "KEY=VAL"])
            .unwrap();
        let map = extract_field_map(&matches, &fields, true);
        assert!(!map.was_provided("remove_var"));
        assert!(map.was_provided("var"));
    }

    #[test]
    fn test_extract_field_map_edit_only_present_in_edit() {
        // edit_only field SHOULD appear in edit mode
        let fields = vec![CliFieldDef {
            name: "remove_var".into(),
            short: Some('R'),
            help: "Remove var".into(),
            field_type: "csv".into(),
            default_value: None,
            required_for_add: false,
            add_only: false,
            edit_only: true,
            preset_fill: false,
            preset_trigger: false,
            is_metadata: false,
            sensitive: false,
            possible_values: vec![],
        }];
        let matches = clap::Command::new("test")
            .arg(
                clap::Arg::new("remove_var")
                    .long("remove-var")
                    .allow_hyphen_values(true),
            )
            .try_get_matches_from(["test", "--remove-var", "A,B"])
            .unwrap();
        let map = extract_field_map(&matches, &fields, false);
        assert!(map.was_provided("remove_var"));
        assert_eq!(
            map.get_csv("remove_var"),
            Some(vec!["A".into(), "B".into()])
        );
    }
}
