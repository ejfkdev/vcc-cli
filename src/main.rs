mod adapter;
mod cli;
mod config;
mod datasource;
mod model;
mod session;
mod store;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "vcc",
    version,
    about = "VibeCodingControl CLI - AI编码工具配置管理器"
)]
struct Args {
    /// 以 JSON 格式输出
    #[arg(long, global = true)]
    json: bool,

    /// 启用性能调试输出（[PERF] 日志）
    #[arg(long, global = true, hide = true)]
    debug: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// 显示当前状态
    #[command(
        after_help = "Examples:\n  vcc status                                    # 显示概览\n  vcc status --json                            # JSON 格式输出"
    )]
    Status,

    /// 从工具配置导入/同步到 registry
    #[command(
        after_help = "Examples:\n  vcc import                                    # 从所有已安装工具导入\n  vcc import claude                             # 只从 claude 导入\n  vcc import --dry-run                          # 模拟运行\n  vcc import cc-switch                          # 从 cc-switch 数据库导入\n  vcc import --json                             # JSON 格式输出（含详细信息）"
    )]
    Import {
        /// 数据源 (claude/codex/gemini/opencode/aider/kimi/cc-switch/cherry-studio)，不指定则同步所有已安装工具
        tool: Option<String>,
        /// 模拟运行，只显示会做什么，不实际写入
        #[arg(long)]
        dry_run: bool,
    },

    /// 应用 profile 配置到指定工具，或增量增删资源
    #[command(
        after_help = "Examples:\n  vcc apply -p default                          # 应用 default profile 到所有工具\n  vcc apply claude -p work                      # 只应用到 claude\n  vcc apply --add-mcp fs --add-mcp icm          # 增量添加 MCP\n  vcc apply --remove-hook myhook                # 增量移除 Hook\n  vcc apply --dry-run -p default                # 模拟运行"
    )]
    Apply {
        /// 目标工具 (claude/codex/gemini/opencode/aider/kimi)，不指定则应用到所有已安装工具
        tool: Option<String>,
        /// Profile 名称（与 --add-*/--remove-* 至少提供一个）
        #[arg(short, long)]
        profile: Option<String>,
        /// 只应用指定类型的资源 (provider/mcp/hook/skill/prompt/env/plugin/agent)，逗号分隔
        #[arg(short, long)]
        only: Option<String>,
        /// 增加的 Provider（逗号分隔）
        #[arg(long)]
        add_provider: Option<String>,
        /// 移除的 Provider（逗号分隔）
        #[arg(long)]
        remove_provider: Option<String>,
        /// 增加的 MCP Server（逗号分隔）
        #[arg(long)]
        add_mcp: Option<String>,
        /// 移除的 MCP Server（逗号分隔）
        #[arg(long)]
        remove_mcp: Option<String>,
        /// 增加的 Hook（逗号分隔）
        #[arg(long)]
        add_hook: Option<String>,
        /// 移除的 Hook（逗号分隔）
        #[arg(long)]
        remove_hook: Option<String>,
        /// 增加的 Skill（逗号分隔）
        #[arg(long)]
        add_skill: Option<String>,
        /// 移除的 Skill（逗号分隔）
        #[arg(long)]
        remove_skill: Option<String>,
        /// 增加的 Agent（逗号分隔）
        #[arg(long)]
        add_agent: Option<String>,
        /// 移除的 Agent（逗号分隔）
        #[arg(long)]
        remove_agent: Option<String>,
        /// 增加的 Plugin（逗号分隔）
        #[arg(long)]
        add_plugin: Option<String>,
        /// 移除的 Plugin（逗号分隔）
        #[arg(long)]
        remove_plugin: Option<String>,
        /// 增加的环境变量组（逗号分隔）
        #[arg(long)]
        add_env: Option<String>,
        /// 移除的环境变量组（逗号分隔）
        #[arg(long)]
        remove_env: Option<String>,
        /// 增加的 Prompt（逗号分隔）
        #[arg(long)]
        add_prompt: Option<String>,
        /// 移除的 Prompt（逗号分隔）
        #[arg(long)]
        remove_prompt: Option<String>,
        /// 模拟运行，只显示会做什么，不实际写入
        #[arg(long)]
        dry_run: bool,
    },

    /// 查看工具的实际配置
    #[command(
        after_help = "Examples:\n  vcc inspect                                   # 查看所有工具配置\n  vcc inspect claude                            # 只查看 claude\n  vcc inspect -o mcp,provider                   # 只看 MCP 和 Provider"
    )]
    Inspect {
        /// 目标工具 (claude/codex/gemini/opencode/aider/kimi)，不指定则显示所有已安装工具
        tool: Option<String>,
        /// 只显示指定类型的资源 (mcp/plugin/hook/env/agent/prompt/provider/skill)，逗号分隔
        #[arg(short, long)]
        only: Option<String>,
    },

    /// 列出可用预设
    #[command(
        after_help = "Examples:\n  vcc preset                                   # 列出预设概览\n  vcc preset provider list                     # 列出所有平台和模型\n  vcc preset provider show openai              # 查看 openai 平台的模型\n  vcc preset provider show gpt-4o              # 查看 gpt-4o 模型详情\n  vcc preset provider update                   # 下载/更新平台数据\n  vcc preset mcp                               # 列出 MCP 预设"
    )]
    Preset {
        #[command(subcommand)]
        action: Option<PresetAction>,
    },

    /// 显示 Token 用量统计
    #[command(
        after_help = "Examples:\n  vcc usage                                    # 本周用量\n  vcc usage --today                            # 今日用量\n  vcc usage --month                            # 本月用量\n  vcc usage --all                              # 全部用量\n  vcc usage -t claude                          # 只看 claude\n  vcc usage --from 2025-01-01 --to 2025-01-31  # 指定日期范围\n  vcc usage --by day,model                     # 按日+模型聚合"
    )]
    Usage {
        /// 目标工具 (claude/codex/gemini/opencode/kimi)，不指定则显示所有
        #[arg(short, long)]
        tool: Option<String>,
        /// 今日用量
        #[arg(long, conflicts_with_all = ["week", "month", "all"])]
        today: bool,
        /// 本周用量（默认）
        #[arg(long, conflicts_with_all = ["today", "month", "all"])]
        week: bool,
        /// 本月用量
        #[arg(long, conflicts_with_all = ["today", "week", "all"])]
        month: bool,
        /// 全部用量
        #[arg(long, conflicts_with_all = ["today", "week", "month"])]
        all: bool,
        /// 起始日期 (YYYY-MM-DD)
        #[arg(long, conflicts_with_all = ["today", "week", "month", "all"])]
        from: Option<String>,
        /// 结束日期 (YYYY-MM-DD)
        #[arg(long, conflicts_with_all = ["today", "week", "month", "all"])]
        to: Option<String>,
        /// 聚合维度，逗号分隔 (day,tool,model)，默认 "tool,model"
        #[arg(short, long)]
        r#by: Option<String>,
    },
}

#[derive(clap::Subcommand, Debug)]
enum PresetAction {
    /// 列出/管理 Provider 平台
    Provider {
        #[command(subcommand)]
        action: Option<ProviderAction>,
    },
    /// 列出 MCP 预设
    Mcp,
}

#[derive(clap::Subcommand, Debug)]
enum ProviderAction {
    /// 列出所有平台和模型
    List,
    /// 查看指定平台或模型的详情
    Show {
        /// 平台 ID（如 openai, anthropic）或模型 ID（如 gpt-4o, claude-sonnet-4-6）
        name: String,
    },
    /// 下载/更新 models.dev 平台数据
    Update,
}

#[tokio::main]
async fn main() {
    // 动态构建命令：先从 derive 构建，再追加子命令
    let app = <Args as clap::CommandFactory>::command();
    let app = cli::dynamic::augment_fixed_subcommands(app);
    let app = cli::dynamic::augment_resource_commands(app);
    let app_for_help = app.clone();
    let matches = match app.try_get_matches() {
        Ok(m) => m,
        Err(e) => {
            let should_show_help = matches!(
                e.kind(),
                clap::error::ErrorKind::MissingSubcommand
                    | clap::error::ErrorKind::MissingRequiredArgument
                    | clap::error::ErrorKind::InvalidSubcommand
            );
            e.print().ok();
            if should_show_help {
                print_help_for_partial_args(&app_for_help);
            }
            std::process::exit(e.exit_code());
        }
    };

    // 解析全局 --json
    let json = matches.get_flag("json");
    cli::output::set_json_mode(json);

    // 解析全局 --debug
    let debug = matches.get_flag("debug");
    cli::output::set_debug_mode(debug);

    // 解析子命令
    let sub = matches.subcommand();
    let result = match sub {
        Some((name, m)) => dispatch_command(name, m).await,
        None => cli::status::run(),
    };

    if let Err(e) = result {
        if cli::output::is_json_mode() {
            eprintln!(
                "{}",
                serde_json::json!({"success": false, "error": format!("{e:#}")})
            );
        } else {
            eprintln!("vcc: error: {e:#}");
        }
        std::process::exit(1);
    }
}

/// 根据命令行参数找到最深的子命令并打印帮助
fn print_help_for_partial_args(app: &clap::Command) {
    let args: Vec<String> = std::env::args().collect();
    let mut cmd = app.clone();

    for arg in args.iter().skip(1) {
        if arg.starts_with('-') {
            continue;
        }
        if let Some(sub) = cmd.find_subcommand(arg) {
            cmd = sub.clone();
        } else {
            break;
        }
    }

    eprintln!();
    let _ = cmd.write_help(&mut std::io::stderr());
    eprintln!();
}

async fn dispatch_command(name: &str, matches: &clap::ArgMatches) -> anyhow::Result<()> {
    // 资源类型子命令（provider/mcp/hook/agent/skill/prompt/env/plugin）
    let registry = config::resource_registry();
    if let Some(_res_cfg) = registry.resource_for(name) {
        let action = cli::dynamic::parse_resource_command(name, matches)?;
        return cli::resource::handle_resource(name, action);
    }

    // 非资源子命令：需要重新用 Args derive 解析
    // 因为 derive 和 builder 混用，这里手动分派
    match name {
        "status" => cli::status::run(),
        "import" => {
            let tool = matches.get_one::<String>("tool").cloned();
            let dry_run = matches.get_flag("dry_run");
            match tool.as_deref() {
                Some("cc-switch") => cli::import::run_ccswitch(dry_run),
                Some("cherry-studio") => cli::import::run_cherry_studio(dry_run),
                Some(t) => cli::import::run_adapter(t, dry_run),
                None => cli::import::run_all(dry_run),
            }
        }
        "apply" => {
            let tool = matches.get_one::<String>("tool").cloned();
            let profile = matches.get_one::<String>("profile").cloned();
            let only = matches.get_one::<String>("only").cloned();
            let csv = |name: &str| {
                matches
                    .try_get_one::<String>(name)
                    .ok()
                    .flatten()
                    .map(|s| cli::parse_csv(s))
                    .unwrap_or_default()
            };
            let dry_run = matches.get_flag("dry_run");
            let mut resource_ops = std::collections::HashMap::new();
            for kind in config::resource_registry().all_kinds() {
                let add_key = format!("add_{}", kind);
                let remove_key = format!("remove_{}", kind);
                let add = csv(&add_key);
                let remove = csv(&remove_key);
                if !add.is_empty() || !remove.is_empty() {
                    resource_ops
                        .insert(kind.to_string(), cli::apply_cmd::AddRemove { add, remove });
                }
            }
            cli::apply_cmd::run(cli::apply_cmd::ApplyArgs {
                tool,
                profile,
                only,
                resource_ops,
                dry_run,
            })
        }
        "preset" => {
            match matches.subcommand() {
                Some(("provider", pm)) => {
                    match pm.subcommand() {
                        Some(("list", _)) => cli::presets::list_providers(),
                        Some(("show", sm)) => {
                            let name = sm.get_one::<String>("name").map(|s| s.as_str()).unwrap_or("");
                            cli::presets::show_provider_or_model(name)
                        }
                        Some(("update", _)) => cli::presets::update_provider().await,
                        _ => cli::presets::list_providers(),
                    }
                }
                Some(("mcp", _)) => cli::presets::list_mcp(),
                _ => cli::presets::list_presets_overview(),
            }
        }
        "inspect" => {
            let tool = matches.get_one::<String>("tool").cloned();
            let only = matches.get_one::<String>("only").cloned();
            cli::inspect_cmd::run(tool.as_deref(), only.as_deref())
        }
        "profile" => cli::profile::handle_subcommand(matches),
        "config" => cli::config_cmd::handle_subcommand(matches),
        "session" => cli::session_cmd::handle_subcommand(matches),
        "usage" => {
            let tool = matches.get_one::<String>("tool").cloned();
            let today = matches.get_flag("today");
            let _week = matches.get_flag("week");
            let month = matches.get_flag("month");
            let all = matches.get_flag("all");
            let from = matches.get_one::<String>("from").cloned();
            let to = matches.get_one::<String>("to").cloned();
            let r#by = matches.get_one::<String>("by").cloned();
            let range = if today {
                session::model::TimeRange::Today
            } else if month {
                session::model::TimeRange::Month
            } else if all {
                session::model::TimeRange::All
            } else {
                session::model::TimeRange::Week
            };
            cli::session_cmd::show_usage(
                tool.as_deref(),
                range,
                from.as_deref(),
                to.as_deref(),
                r#by.as_deref(),
            )
        }
        _ => anyhow::bail!("unknown command: '{}'. Run 'vcc --help' for usage.", name),
    }
}
