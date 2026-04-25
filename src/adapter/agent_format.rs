use std::collections::HashMap;

use crate::model::agent::{Agent, AgentConfig, AgentTools};
use crate::model::Metadata;

/// 解析 Markdown frontmatter + body
pub(crate) fn parse_markdown_frontmatter(
    raw: &str,
) -> (HashMap<String, serde_json::Value>, String) {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return (HashMap::new(), raw.to_string());
    }

    let rest = &trimmed[3..];
    let end = match rest.find("---") {
        Some(i) => i,
        None => return (HashMap::new(), raw.to_string()),
    };

    let yaml_str = &rest[..end];
    let body = rest[end + 3..].trim().to_string();

    let mut frontmatter: HashMap<String, serde_json::Value> = HashMap::new();
    for line in yaml_str.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(": ") {
            let key = key.trim().to_string();
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value)
                .to_string();
            frontmatter.insert(key, serde_json::Value::String(value));
        }
    }

    (frontmatter, body)
}

/// 从 frontmatter 中解析 tools 字段
pub(crate) fn parse_tools_from_frontmatter(fm: &HashMap<String, serde_json::Value>) -> AgentTools {
    let tools_str = match fm.get("tools").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return AgentTools::default(),
    };

    let enabled: Vec<String> = tools_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    AgentTools {
        enabled,
        disabled: Vec::new(),
    }
}

/// 从 Markdown frontmatter 格式解析 Agent（Claude/Codex/Aider 格式）
pub(crate) fn parse_agent_markdown(
    raw: &str,
    fallback_name: &str,
    tool_name: &str,
) -> Option<Agent> {
    let (frontmatter, body) = parse_markdown_frontmatter(raw);
    if frontmatter.is_empty() && body.is_empty() {
        return None;
    }

    let name = frontmatter
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(fallback_name)
        .to_string();

    let description = frontmatter
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let model = frontmatter
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && s != &"inherit")
        .map(|s| s.to_string());

    let tools = parse_tools_from_frontmatter(&frontmatter);

    Some(Agent {
        name,
        id: String::new(),
        r#type: "agent".to_string(),
        config: AgentConfig {
            mode: "subagent".to_string(),
            description,
            model,
            tools,
            permission: HashMap::new(),
            temperature: None,
            content: if body.is_empty() { None } else { Some(body) },
        },
        metadata: Metadata {
            description: None,
            homepage: None,
            tags: vec![
                crate::config::adapter_defaults().defaults.sync_tag.clone(),
                tool_name.to_string(),
            ],
        },
        tool: HashMap::new(),
    })
}

/// 将 Agent 序列化为 markdown + YAML frontmatter 格式
pub(crate) fn format_agent_markdown(agent: &Agent) -> String {
    let mut frontmatter = String::from("---\n");
    frontmatter.push_str(&format!("name: {:?}\n", agent.name));
    if let Some(ref desc) = agent.config.description {
        frontmatter.push_str(&format!("description: {:?}\n", desc));
    }
    if let Some(ref model) = agent.config.model {
        frontmatter.push_str(&format!("model: {:?}\n", model));
    }
    if !agent.config.tools.enabled.is_empty() {
        frontmatter.push_str(&format!(
            "tools: {}\n",
            agent.config.tools.enabled.join(", ")
        ));
    }
    frontmatter.push_str("---\n");
    if let Some(ref content) = agent.config.content {
        frontmatter.push_str(content);
    }
    frontmatter
}

/// 将 Agent 序列化为 Kimi YAML 格式
/// Kimi 格式: version: 1, agent: { name, system_prompt_path, tools, exclude_tools, extend, subagents }
pub(crate) fn format_agent_yaml(agent: &Agent) -> String {
    let mut yaml = String::from("version: 1\nagent:\n");

    if agent.config.mode == "primary" {
        yaml.push_str("  extend: default\n");
    }

    yaml.push_str(&format!("  name: {:?}\n", agent.name));

    if let Some(desc) = &agent.config.description {
        yaml.push_str(&format!("  description: {:?}\n", desc));
    }

    if let Some(model) = &agent.config.model {
        yaml.push_str(&format!("  model: {:?}\n", model));
    }

    if agent.config.content.is_some() {
        yaml.push_str(&format!(
            "  system_prompt_path: ./{}-system.md\n",
            agent.name
        ));
    }

    if let Some(temp) = &agent.config.temperature {
        yaml.push_str(&format!("  temperature: {}\n", temp));
    }

    if !agent.config.tools.enabled.is_empty() {
        yaml.push_str("  tools:\n");
        for tool in &agent.config.tools.enabled {
            yaml.push_str(&format!("    - \"{}\"\n", tool));
        }
    }

    if !agent.config.tools.disabled.is_empty() {
        yaml.push_str("  exclude_tools:\n");
        for tool in &agent.config.tools.disabled {
            yaml.push_str(&format!("    - \"{}\"\n", tool));
        }
    }

    yaml
}

/// 从 Kimi YAML 格式解析 Agent
pub(crate) fn parse_agent_yaml(raw: &str, fallback_name: &str) -> Option<Agent> {
    let mut fields: HashMap<String, String> = HashMap::new();
    let mut list_fields: HashMap<String, Vec<String>> = HashMap::new();
    let mut in_agent = false;
    let mut current_list_key: Option<String> = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if trimmed == "agent:" {
            in_agent = true;
            continue;
        }
        if !in_agent {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') && !trimmed.starts_with('-') {
            break;
        }
        if let Some(stripped) = trimmed.strip_prefix("- ") {
            if let Some(key) = &current_list_key {
                list_fields
                    .entry(key.clone())
                    .or_default()
                    .push(stripped.trim().trim_matches('"').to_string());
            }
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let (key, value) = (key.trim(), value.trim());
            current_list_key = None;
            if value.is_empty() && ["tools", "exclude_tools", "subagents"].contains(&key) {
                current_list_key = Some(key.to_string());
            } else {
                fields.insert(key.to_string(), value.trim_matches('"').to_string());
            }
        }
    }

    let extend = fields.get("extend").cloned();
    if fields.is_empty() && list_fields.is_empty() {
        return None;
    }
    Some(Agent {
        name: fields
            .get("name")
            .cloned()
            .unwrap_or_else(|| fallback_name.to_string()),
        id: String::new(),
        r#type: "agent".to_string(),
        config: AgentConfig {
            mode: if extend.as_deref() == Some("default") {
                "primary".to_string()
            } else {
                "subagent".to_string()
            },
            description: fields.get("description").cloned().or_else(|| {
                fields
                    .get("system_prompt_path")
                    .map(|p| format!("prompt: {}", p))
            }),
            model: fields.get("model").and_then(|m| {
                if m.is_empty() || m == "inherit" {
                    None
                } else {
                    Some(m.clone())
                }
            }),
            tools: AgentTools {
                enabled: list_fields.get("tools").cloned().unwrap_or_default(),
                disabled: list_fields
                    .get("exclude_tools")
                    .cloned()
                    .unwrap_or_default(),
            },
            permission: HashMap::new(),
            temperature: fields
                .get("temperature")
                .and_then(|t| t.parse::<f64>().ok()),
            content: None,
        },
        metadata: Metadata {
            description: None,
            homepage: None,
            tags: vec![crate::config::adapter_defaults().defaults.sync_tag.clone()],
        },
        tool: HashMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_markdown_frontmatter ──

    #[test]
    fn test_parse_frontmatter_basic() {
        let raw = "---\nname: my-agent\ndescription: Test agent\n---\nYou are helpful.";
        let (fm, body) = parse_markdown_frontmatter(raw);
        assert_eq!(fm.get("name").unwrap().as_str(), Some("my-agent"));
        assert_eq!(fm.get("description").unwrap().as_str(), Some("Test agent"));
        assert_eq!(body, "You are helpful.");
    }

    #[test]
    fn test_parse_frontmatter_no_frontmatter() {
        let raw = "Just body text, no frontmatter.";
        let (fm, body) = parse_markdown_frontmatter(raw);
        assert!(fm.is_empty());
        assert_eq!(body, raw);
    }

    #[test]
    fn test_parse_frontmatter_unclosed() {
        let raw = "---\nname: test\nNo closing delimiter";
        let (fm, body) = parse_markdown_frontmatter(raw);
        assert!(fm.is_empty());
        assert_eq!(body, raw);
    }

    #[test]
    fn test_parse_frontmatter_quoted_value() {
        let raw = "---\nname: \"my agent\"\n---\nBody";
        let (fm, _) = parse_markdown_frontmatter(raw);
        assert_eq!(fm.get("name").unwrap().as_str(), Some("my agent"));
    }

    #[test]
    fn test_parse_frontmatter_empty_body() {
        let raw = "---\nname: test\n---\n";
        let (fm, body) = parse_markdown_frontmatter(raw);
        assert_eq!(fm.get("name").unwrap().as_str(), Some("test"));
        assert!(body.is_empty());
    }

    #[test]
    fn test_parse_frontmatter_comments_skipped() {
        let raw = "---\n# comment\nname: test\n---\nBody";
        let (fm, _) = parse_markdown_frontmatter(raw);
        assert_eq!(fm.len(), 1);
    }

    // ── parse_tools_from_frontmatter ──

    #[test]
    fn test_parse_tools_basic() {
        let mut fm = HashMap::new();
        fm.insert(
            "tools".into(),
            serde_json::Value::String("Read, Write, Bash".into()),
        );
        let tools = parse_tools_from_frontmatter(&fm);
        assert_eq!(tools.enabled, vec!["Read", "Write", "Bash"]);
    }

    #[test]
    fn test_parse_tools_empty() {
        let fm = HashMap::new();
        let tools = parse_tools_from_frontmatter(&fm);
        assert!(tools.enabled.is_empty());
    }

    #[test]
    fn test_parse_tools_single() {
        let mut fm = HashMap::new();
        fm.insert("tools".into(), serde_json::Value::String("Read".into()));
        let tools = parse_tools_from_frontmatter(&fm);
        assert_eq!(tools.enabled, vec!["Read"]);
    }

    // ── parse_agent_markdown ──

    #[test]
    fn test_parse_agent_markdown_full() {
        let raw = "---\nname: coder\ndescription: Code agent\nmodel: claude-sonnet\ntools: Read, Write\n---\nYou write code.";
        let agent = parse_agent_markdown(raw, "fallback", "claude").unwrap();
        assert_eq!(agent.name, "coder");
        assert_eq!(agent.config.description, Some("Code agent".into()));
        assert_eq!(agent.config.model, Some("claude-sonnet".into()));
        assert_eq!(agent.config.tools.enabled, vec!["Read", "Write"]);
        assert_eq!(agent.config.content, Some("You write code.".into()));
    }

    #[test]
    fn test_parse_agent_markdown_uses_fallback_name() {
        let raw = "---\ndescription: No name\n---\nBody";
        let agent = parse_agent_markdown(raw, "fallback-name", "claude").unwrap();
        assert_eq!(agent.name, "fallback-name");
    }

    #[test]
    fn test_parse_agent_markdown_inherit_model_excluded() {
        let raw = "---\nname: test\nmodel: inherit\n---\nBody";
        let agent = parse_agent_markdown(raw, "fb", "claude").unwrap();
        assert!(agent.config.model.is_none());
    }

    #[test]
    fn test_parse_agent_markdown_empty_returns_none() {
        assert!(parse_agent_markdown("", "fb", "claude").is_none());
    }

    // ── format_agent_markdown ──

    #[test]
    fn test_format_agent_markdown_roundtrip() {
        let agent = Agent {
            name: "coder".into(),
            id: String::new(),
            r#type: "agent".into(),
            config: AgentConfig {
                mode: "subagent".into(),
                description: Some("Code helper".into()),
                model: Some("gpt-4".into()),
                tools: AgentTools {
                    enabled: vec!["Read".into(), "Write".into()],
                    disabled: vec![],
                },
                permission: HashMap::new(),
                temperature: None,
                content: Some("You write code.".into()),
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let md = format_agent_markdown(&agent);
        assert!(md.starts_with("---\n"));
        assert!(md.contains("name: \"coder\""));
        assert!(md.contains("description: \"Code helper\""));
        assert!(md.contains("model: \"gpt-4\""));
        assert!(md.contains("tools: Read, Write"));
        assert!(md.contains("You write code."));
    }

    #[test]
    fn test_format_agent_markdown_minimal() {
        let agent = Agent {
            name: "minimal".into(),
            id: String::new(),
            r#type: "agent".into(),
            config: AgentConfig::default(),
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let md = format_agent_markdown(&agent);
        assert!(md.contains("name: \"minimal\""));
        assert!(!md.contains("description:"));
        assert!(!md.contains("model:"));
        assert!(!md.contains("tools:"));
    }

    // ── format_agent_yaml ──

    #[test]
    fn test_format_agent_yaml_subagent() {
        let agent = Agent {
            name: "helper".into(),
            id: String::new(),
            r#type: "agent".into(),
            config: AgentConfig {
                mode: "subagent".into(),
                description: None,
                model: None,
                tools: AgentTools {
                    enabled: vec!["Read".into()],
                    disabled: vec!["Bash".into()],
                },
                permission: HashMap::new(),
                temperature: None,
                content: Some("Be helpful.".into()),
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let yaml = format_agent_yaml(&agent);
        assert!(yaml.starts_with("version: 1\n"));
        assert!(yaml.contains("name: \"helper\""));
        assert!(yaml.contains("system_prompt_path: ./helper-system.md"));
        assert!(yaml.contains("tools:\n"));
        assert!(yaml.contains("- \"Read\""));
        assert!(yaml.contains("exclude_tools:\n"));
        assert!(yaml.contains("- \"Bash\""));
        assert!(!yaml.contains("extend:"));
    }

    #[test]
    fn test_format_agent_yaml_primary() {
        let agent = Agent {
            name: "main".into(),
            id: String::new(),
            r#type: "agent".into(),
            config: AgentConfig {
                mode: "primary".into(),
                description: None,
                model: None,
                tools: AgentTools::default(),
                permission: HashMap::new(),
                temperature: None,
                content: None,
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let yaml = format_agent_yaml(&agent);
        assert!(yaml.contains("extend: default"));
        assert!(!yaml.contains("system_prompt_path"));
    }

    #[test]
    fn test_format_agent_yaml_full_fields() {
        let agent = Agent {
            name: "reviewer".into(),
            id: String::new(),
            r#type: "agent".into(),
            config: AgentConfig {
                mode: "subagent".into(),
                description: Some("Code review agent".into()),
                model: Some("claude-sonnet".into()),
                tools: AgentTools {
                    enabled: vec!["Read".into(), "Grep".into()],
                    disabled: vec![],
                },
                permission: HashMap::new(),
                temperature: Some(0.7),
                content: Some("Focus on bugs.".into()),
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let yaml = format_agent_yaml(&agent);
        assert!(yaml.contains("description: \"Code review agent\""));
        assert!(yaml.contains("model: \"claude-sonnet\""));
        assert!(yaml.contains("temperature: 0.7"));
        assert!(yaml.contains("system_prompt_path: ./reviewer-system.md"));
        assert!(yaml.contains("tools:\n"));
        assert!(!yaml.contains("exclude_tools"));
    }

    #[test]
    fn test_format_agent_yaml_no_optional_fields() {
        let agent = Agent {
            name: "minimal".into(),
            id: String::new(),
            r#type: "agent".into(),
            config: AgentConfig {
                mode: "subagent".into(),
                description: None,
                model: None,
                tools: AgentTools::default(),
                permission: HashMap::new(),
                temperature: None,
                content: None,
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let yaml = format_agent_yaml(&agent);
        assert!(!yaml.contains("description:"));
        assert!(!yaml.contains("model:"));
        assert!(!yaml.contains("temperature:"));
        assert!(!yaml.contains("system_prompt_path:"));
    }

    #[test]
    fn test_roundtrip_agent_yaml_with_model() {
        let yaml = "version: 1\nagent:\n  name: \"test\"\n  description: \"Test agent\"\n  model: \"gpt-4\"\n  temperature: 0.5\n  tools:\n    - \"Read\"\n";
        let agent = parse_agent_yaml(yaml, "fb").unwrap();
        assert_eq!(agent.config.description.as_deref(), Some("Test agent"));
        assert_eq!(agent.config.model.as_deref(), Some("gpt-4"));
        assert_eq!(agent.config.temperature, Some(0.5));
        let out = format_agent_yaml(&agent);
        assert!(out.contains("description: \"Test agent\""));
        assert!(out.contains("model: \"gpt-4\""));
        assert!(out.contains("temperature: 0.5"));
    }

    // ── parse_agent_yaml ──

    #[test]
    fn test_parse_agent_yaml_basic() {
        let yaml = "version: 1\nagent:\n  name: \"coder\"\n  tools:\n    - \"Read\"\n    - \"Write\"\n  exclude_tools:\n    - \"Bash\"\n";
        let agent = parse_agent_yaml(yaml, "fallback").unwrap();
        assert_eq!(agent.name, "coder");
        assert_eq!(agent.config.tools.enabled, vec!["Read", "Write"]);
        assert_eq!(agent.config.tools.disabled, vec!["Bash"]);
        assert_eq!(agent.config.mode, "subagent");
    }

    #[test]
    fn test_parse_agent_yaml_primary_extend() {
        let yaml = "version: 1\nagent:\n  name: \"main\"\n  extend: default\n";
        let agent = parse_agent_yaml(yaml, "fb").unwrap();
        assert_eq!(agent.name, "main");
        assert_eq!(agent.config.mode, "primary");
    }

    #[test]
    fn test_parse_agent_yaml_fallback_name() {
        let yaml = "version: 1\nagent:\n  tools:\n    - \"Read\"\n";
        let agent = parse_agent_yaml(yaml, "fallback-name").unwrap();
        assert_eq!(agent.name, "fallback-name");
    }

    #[test]
    fn test_parse_agent_yaml_empty() {
        assert!(parse_agent_yaml("", "fb").is_none());
    }

    #[test]
    fn test_parse_agent_yaml_model_inherit_excluded() {
        let yaml = "version: 1\nagent:\n  name: \"test\"\n  model: inherit\n";
        let agent = parse_agent_yaml(yaml, "fb").unwrap();
        assert!(agent.config.model.is_none());
    }

    // ── Roundtrip: markdown format → parse → format ──

    #[test]
    fn test_markdown_roundtrip() {
        let agent = Agent {
            name: "roundtrip".into(),
            id: String::new(),
            r#type: "agent".into(),
            config: AgentConfig {
                mode: "subagent".into(),
                description: Some("Test".into()),
                model: None,
                tools: AgentTools {
                    enabled: vec!["Read".into()],
                    disabled: vec![],
                },
                permission: HashMap::new(),
                temperature: None,
                content: Some("Do things.".into()),
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let md = format_agent_markdown(&agent);
        let parsed = parse_agent_markdown(&md, "fb", "claude").unwrap();
        assert_eq!(parsed.name, "roundtrip");
        assert_eq!(parsed.config.description, Some("Test".into()));
        assert_eq!(parsed.config.tools.enabled, vec!["Read"]);
        assert_eq!(parsed.config.content, Some("Do things.".into()));
    }
}
