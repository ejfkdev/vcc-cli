# VCC

[![Crates.io](https://img.shields.io/crates/v/vcc?style=flat-square)](https://crates.io/crates/vcc)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](https://github.com/ejfkdev/vcc-cli/blob/main/LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![CI](https://img.shields.io/github/actions/workflow/status/ejfkdev/vcc-cli/ci.yml?style=flat-square&branch=main)](https://github.com/ejfkdev/vcc-cli/actions)
[![GitHub Release](https://img.shields.io/github/v/release/ejfkdev/vcc-cli?style=flat-square)](https://github.com/ejfkdev/vcc-cli/releases/latest)

Unified configuration manager for AI coding tools. One config to rule them all.

[中文文档](README.zh-CN.md)

VCC lets you define your AI coding tool configuration once — providers, MCP servers, hooks, prompts, skills, agents, env vars, plugins — then apply it to any supported tool. No more manually editing JSON/TOML/YAML across multiple tools.

## Install

### From GitHub Releases

Download the latest binary for your platform from [Releases](https://github.com/ejfkdev/vcc-cli/releases/latest).

### From source

```bash
git clone https://github.com/ejfkdev/vcc-cli.git
cd vcc-cli
cargo install --path .
```

## Supported Tools

| Tool | Config Format | Resources |
|------|--------------|-----------|
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | JSON (JSONC) | Provider, MCP, Hook, Env, Skill, Agent, Prompt, Plugin |
| [Codex CLI](https://github.com/openai/codex) | JSON + TOML | Provider, MCP |
| [Gemini CLI](https://github.com/google-gemini/gemini-cli) | JSON + YAML | Provider, MCP, Env, Skill |
| [OpenCode](https://github.com/opencode-ai/opencode) | TOML | Provider, MCP, Hook, Skill, Prompt |
| [Aider](https://github.com/Aider-AI/aider) | YAML | Provider |
| [Kimi](https://github.com/anthropics/kimi) | JSON + YAML | Provider, MCP, Hook, Skill |
| [Droid](https://github.com/nicepkg/droid) | JSON | Provider, MCP, Plugin, Prompt |

## Quick Start

```bash
# Initialize with a profile
vcc profile add default

# Add a provider (interactive or with flags)
vcc provider add my-openai --preset openai --key sk-xxx

# Add an MCP server
vcc mcp add filesystem --preset filesystem

# Add a hook
vcc hook add my-hook --event PreToolUse --command "echo 'running'"

# Apply to a tool
vcc apply claude              # Apply all resources to Claude Code
vcc apply --profile default   # Apply a specific profile

# Inspect what's configured
vcc inspect claude            # See Claude's actual config
vcc inspect -o mcp,provider   # Filter by resource type

# Incremental changes
vcc apply claude --add-mcp db --remove-mcp old-server
```

## Commands

| Command | Description |
|---------|-------------|
| `vcc status` | Show overview of configured tools and resources |
| `vcc apply [TOOL]` | Apply profile/changes to a tool |
| `vcc inspect [TOOL]` | View a tool's actual configuration |
| `vcc import [SOURCE]` | Import existing config from cc-switch or Cherry Studio |
| `vcc preset` | List and apply built-in presets |
| `vcc usage` | Show token usage statistics |
| `vcc profile` | Manage named profiles |
| `vcc config` | Manage VCC settings |
| `vcc session` | Manage sessions and view usage |
| `vcc provider` | Manage AI providers |
| `vcc mcp` | Manage MCP servers |
| `vcc hook` | Manage lifecycle hooks |
| `vcc agent` | Manage agent definitions |
| `vcc skill` | Manage skill packages |
| `vcc prompt` | Manage prompt templates |
| `vcc env` | Manage environment variables |
| `vcc plugin` | Manage tool plugins |

Each resource command supports `list`, `add`, `remove`, and `show` subcommands. MCP and plugin also support `toggle` to enable/disable.

## How It Works

VCC maintains a **resource registry** (`~/.vcc/registry/`) with TOML files for each resource. When you `apply`, VCC reads the registry and writes the correct format (JSON/TOML/YAML) to each tool's config directory.

```
~/.vcc/registry/
  provider/
    my-openai.toml
    my-anthropic.toml
  mcp/
    filesystem.toml
  hook/
    my-hook.toml
  ...
```

Adapters handle the translation between VCC's unified format and each tool's native format. The adapter config (mapping rules, field names, file paths) is defined in TOML, making it easy to add new tools without writing Rust code.

### Config-Driven Adapters

Instead of per-tool Rust code, VCC uses a generic adapter that reads TOML mapping configs. Each tool's format — file paths, key names, field mapping rules — is declared in `src/config/adapter_mappings/`. Adding support for a new tool is just a matter of writing a new mapping file.

### Tool Overrides

Any resource can have per-tool value overrides. For example, the same MCP server can use different `command` or `env` values for different tools:

```toml
# ~/.vcc/registry/mcp/my-server.toml
name = "my-server"
server_type = "stdio"
command = "node"

[tool.opencode]
command = "npx"
```

### Profiles

Profiles define which resources are active. Apply different profiles to switch contexts:

```bash
vcc profile add work
vcc profile apply work        # Enable work resources
vcc apply claude --profile work
```

### Hash-Based Change Detection

VCC uses XXH3-64 hashes to detect whether a sync actually modified anything, making `apply` idempotent — running it twice produces the same result.

## Resource Types

| Type | Key Fields |
|------|-----------|
| **Provider** | API key, base URL, provider type, models, headers, env |
| **MCP** | Server type (stdio/sse/streamable-http), command, args, env, URL, headers |
| **Hook** | Event (PreToolUse/PostToolUse), matcher, command, timeout |
| **Agent** | Mode (subagent/primary), description, model, tools, permission |
| **Skill** | Source (github/local/url), repo, path, install method |
| **Prompt** | Content |
| **Env** | Variables (key-value map) |
| **Plugin** | Source, repo, path, marketplace, install method |

## Built-in Presets

**Providers:** anthropic, openai, google, deepseek, xai, mistral, openrouter, groq

**MCP Servers:** filesystem, context7, deepwiki, icm, sequential-thinking, brave-search

## Session Usage Tracking

VCC can scan AI tool session files and report token usage:

```bash
vcc usage                    # Aggregate usage across all tools
vcc session list             # List sessions
```

Incremental parsing ensures repeated scans are fast — only new data since the last scan is processed.

## Development

```bash
# Build
cargo build

# Run tests
cargo test

# Run with logs
RUST_LOG=debug cargo run -- apply claude

# Format check
cargo fmt --check

# Lint
cargo clippy
```

## Architecture

```
src/
  main.rs              CLI entry point, clap definitions
  cli/                 Command implementations
  model/               Core types: Resource trait, 8 resource structs, Profile
  config/              Compile-time embedded TOML configs (OnceLock singletons)
  adapter/             Adapter trait, GenericAdapter, DocTree, field mapping
  datasource/          External data source import (cc-switch, Cherry Studio)
  session/             Session scanning and usage extraction
  store/               TOML file store for the resource registry
```

## License

MIT
