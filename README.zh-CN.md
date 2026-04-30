# VCC

[![Crates.io](https://img.shields.io/crates/v/vcc?style=flat-square)](https://crates.io/crates/vcc)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](https://github.com/ejfkdev/vcc-cli/blob/main/LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![CI](https://img.shields.io/github/actions/workflow/status/ejfkdev/vcc-cli/ci.yml?style=flat-square&branch=main)](https://github.com/ejfkdev/vcc-cli/actions)
[![GitHub Release](https://img.shields.io/github/v/release/ejfkdev/vcc-cli?style=flat-square)](https://github.com/ejfkdev/vcc-cli/releases/latest)

AI 编码工具统一配置管理器。一份配置，管理所有工具。

[English](README.md)

VCC 让你只需定义一次 AI 编码工具的配置——Provider、MCP 服务器、Hook、Prompt、Skill、Agent、环境变量、插件——然后一键同步到所有支持的工具。无需再手动编辑各种 JSON/TOML/YAML 配置文件。

## 安装

### macOS / Linux

```bash
curl -fsSL https://raw.githubusercontent.com/ejfkdev/vcc-cli/main/install.sh | sh
```

### Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/ejfkdev/vcc-cli/main/install.ps1 | iex
```

### Homebrew

```bash
brew tap ejfkdev/tap
brew install vcc
```

### Cargo

```bash
cargo install vcc
```

### 从源码编译

```bash
git clone https://github.com/ejfkdev/vcc-cli.git
cd vcc-cli
cargo install --path .
```

## 支持的工具

| 工具 | 配置格式 | 支持的资源 |
|------|---------|-----------|
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | JSON (JSONC) | Provider, MCP, Hook, Env, Agent, Prompt, Plugin |
| [Codex CLI](https://github.com/openai/codex) | JSON + TOML | Provider, MCP, Prompt, Plugin |
| [Gemini CLI](https://github.com/google-gemini/gemini-cli) | JSON + .env | Provider, MCP, Hook, Env, Prompt, Plugin |
| [OpenCode](https://github.com/opencode-ai/opencode) | JSON (JSONC) | Provider, MCP, Prompt, Plugin |
| [Droid](https://github.com/nicepkg/droid) | JSON | Provider, MCP, Prompt, Plugin |
| [Kimi](https://github.com/MoonshotAI/kimi-cli) | JSON + TOML + YAML | Provider, MCP, Hook, Agent, Prompt, Plugin |
| [Aider](https://github.com/paul-gauthier/aider) | YAML | Provider |
| [Cursor](https://cursor.com) | CSV | Session |
| [Copilot](https://github.com/features/copilot) | JSONL | Session |
| [Amp](https://ampcode.com) | JSON | Session |
| [RooCode](https://roocode.com) | JSON | Session |
| [KiloCode](https://kilocode.com) | JSON | Session |
| [Kilo](https://github.com/nicepkg/kilo) | SQLite | Session |
| [Crush](https://github.com/charmbracelet/crush) | SQLite | Session |
| [Hermes](https://github.com/NousResearch/hermes-agent) | SQLite | Session |
| [Qwen](https://github.com/QwenLM/qwen-code) | JSONL | Session |
| [Pi](https://pi.ai) | JSONL | Session |
| [OpenClaw](https://github.com/openclaw/openclaw) | JSONL | Session |
| [Mux](https://mux.com) | JSON | Session |

## 快速上手

```bash
# 创建一个配置档案
vcc profile add default

# 添加 Provider（交互式或使用参数）
vcc provider add my-openai --preset openai --key sk-xxx

# 添加 MCP 服务器
vcc mcp add filesystem --preset filesystem

# 添加 Hook
vcc hook add my-hook --event PreToolUse --command "echo 'running'"

# 同步到工具
vcc apply claude              # 同步所有资源到 Claude Code
vcc apply --profile default   # 同步指定档案

# 查看工具配置
vcc inspect claude            # 查看 Claude 的实际配置
vcc inspect -o mcp,provider   # 按资源类型过滤

# 增量变更
vcc apply claude --add-mcp db --remove-mcp old-server
```

## 命令一览

| 命令 | 说明 |
|------|------|
| `vcc status` | 显示已配置工具和资源的概览 |
| `vcc apply [TOOL]` | 将档案/变更同步到指定工具 |
| `vcc inspect [TOOL]` | 查看工具的实际配置 |
| `vcc import [SOURCE]` | 从 cc-switch 或 Cherry Studio 导入配置 |
| `vcc preset` | 列出和应用内置预设 |
| `vcc usage` | 显示 Token 用量统计 |
| `vcc profile` | 管理命名档案 |
| `vcc config` | 管理 VCC 设置 |
| `vcc session` | 管理会话和查看用量 |
| `vcc provider` | 管理 AI Provider |
| `vcc mcp` | 管理 MCP 服务器 |
| `vcc hook` | 管理生命周期 Hook |
| `vcc agent` | 管理 Agent 定义 |
| `vcc skill` | 管理 Skill 包 |
| `vcc prompt` | 管理 Prompt 模板 |
| `vcc env` | 管理环境变量 |
| `vcc plugin` | 管理工具插件 |

每个资源命令支持 `list`、`add`、`remove`、`show` 子命令。MCP 和 Plugin 还支持 `toggle` 来启用/禁用。

## 工作原理

VCC 维护一个**资源注册表**（`~/.config/VibeCodingControl/registry/`），每个资源对应一个 TOML 文件。执行 `apply` 时，VCC 读取注册表并以正确的格式（JSON/TOML/YAML）写入各工具的配置目录。

```
~/.config/VibeCodingControl/registry/
  provider/
    my-openai.toml
    my-anthropic.toml
  mcp/
    filesystem.toml
  hook/
    my-hook.toml
  ...
```

适配器负责在 VCC 统一格式和各工具原生格式之间进行转换。适配器配置（映射规则、字段名、文件路径）以 TOML 定义，无需编写 Rust 代码即可添加新工具支持。

### 配置驱动的适配器

VCC 不为每个工具编写专用 Rust 代码，而是使用通用适配器读取 TOML 映射配置。每个工具的格式——文件路径、键名、字段映射规则——都在 `src/config/adapter_mappings/` 中声明。添加新工具只需编写一个映射文件。

### 工具级别覆盖

任何资源都可以设置针对特定工具的覆盖值。例如，同一个 MCP 服务器在不同工具中使用不同的 `command` 或 `env`：

```toml
# ~/.vcc/registry/mcp/my-server.toml
name = "my-server"
server_type = "stdio"
command = "node"

[tool.opencode]
command = "npx"
```

### 档案（Profile）

档案定义哪些资源处于激活状态。切换不同档案以适应不同场景：

```bash
vcc profile add work
vcc profile apply work        # 启用 work 档案的资源
vcc apply claude --profile work
```

### 哈希变更检测

VCC 使用 XXH3-64 哈希检测同步是否真正产生了变更，使 `apply` 具备幂等性——执行两次结果相同。

## 资源类型

| 类型 | 关键字段 |
|------|---------|
| **Provider** | API Key、Base URL、Provider 类型、模型、请求头、环境变量 |
| **MCP** | 服务器类型（stdio/sse/streamable-http）、命令、参数、环境变量、URL、请求头 |
| **Hook** | 事件（PreToolUse/PostToolUse）、匹配器、命令、超时 |
| **Agent** | 模式（subagent/primary）、描述、模型、工具、权限 |
| **Skill** | 来源（github/local/url）、仓库、路径、安装方式 |
| **Prompt** | 内容 |
| **Env** | 变量（键值对） |
| **Plugin** | 来源、仓库、路径、市场、安装方式 |

## 内置预设

**Provider：** anthropic、openai、google、deepseek、xai、mistral、openrouter、groq

**MCP 服务器：** filesystem、context7、deepwiki、icm、sequential-thinking、brave-search

## 会话用量追踪

VCC 可以扫描 AI 工具的会话文件并报告 Token 用量：

```bash
vcc usage                    # 汇总所有工具的用量
vcc session list             # 列出会话
```

增量解析确保重复扫描高效——仅处理自上次扫描以来的新数据。

## 开发

```bash
# 构建
cargo build

# 运行测试
cargo test

# 带日志运行
RUST_LOG=debug cargo run -- apply claude

# 格式检查
cargo fmt --check

# 代码检查
cargo clippy
```

## 架构

```
src/
  main.rs              CLI 入口，clap 定义
  cli/                 命令实现
  model/               核心类型：Resource trait、8 种资源结构体、Profile
  config/              编译时嵌入的 TOML 配置（OnceLock 单例）
  adapter/             Adapter trait、GenericAdapter、DocTree、字段映射
  datasource/          外部数据源导入（cc-switch、Cherry Studio）
  session/             会话扫描和用量提取
  store/               TOML 文件存储（资源注册表）
```

## 许可证

MIT
