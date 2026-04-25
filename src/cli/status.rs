use anyhow::Result;

use super::output::{is_json_mode, output_json};
use crate::model::{
    agent::Agent, env::Env, hook::Hook, mcp::McpServer, plugin::Plugin, prompt::Prompt,
    provider::Provider, skill::Skill,
};
use crate::store::TomlStore;

pub(crate) fn run() -> Result<()> {
    let store = TomlStore::new()?;

    // 资源统计
    let providers = store.list_resources::<Provider>("provider")?;
    let mcps = store.list_resources::<McpServer>("mcp")?;
    let hooks = store.list_resources::<Hook>("hook")?;
    let agents = store.list_resources::<Agent>("agent")?;
    let skills = store.list_resources::<Skill>("skill")?;
    let prompts = store.list_resources::<Prompt>("prompt")?;
    let envs = store.list_resources::<Env>("env")?;
    let plugins = store.list_resources::<Plugin>("plugin")?;
    let profiles = store.list_profiles()?;

    if is_json_mode() {
        output_json(&serde_json::json!({
            "config_dir": store.root().to_string_lossy().to_string(),
            "resources": {
                "providers": providers.len(),
                "mcp_servers": mcps.len(),
                "hooks": hooks.len(),
                "agents": agents.len(),
                "skills": skills.len(),
                "prompts": prompts.len(),
                "env_groups": envs.len(),
                "plugins": plugins.len(),
                "profiles": profiles.len(),
            }
        }));
        return Ok(());
    }

    println!("Config: {}", store.root().display());
    println!();
    println!("Resources:");
    println!("  Providers:   {}", providers.len());
    println!("  MCP Servers: {}", mcps.len());
    println!("  Hooks:       {}", hooks.len());
    println!("  Agents:      {}", agents.len());
    println!("  Skills:      {}", skills.len());
    println!("  Prompts:     {}", prompts.len());
    println!("  Env Groups:  {}", envs.len());
    println!("  Plugins:     {}", plugins.len());
    println!("  Profiles:    {}", profiles.len());

    Ok(())
}
