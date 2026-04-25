use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;

use super::{json_str, json_str_opt, json_str_or, sanitize_name};
use crate::datasource::datasource_config::DatasourceConfig;
use crate::model::{
    env::Env,
    hook::Hook,
    mcp::{McpServer, McpToolOverride},
    prompt::Prompt,
    provider::{Provider, ProviderConfig, ProviderToolOverride},
    skill::{Skill, SkillToolOverride},
    Metadata, Resource,
};

pub(crate) struct CcSwitchSource {
    db_path: PathBuf,
    config: DatasourceConfig,
}

impl CcSwitchSource {
    pub fn new() -> Option<Self> {
        let config = DatasourceConfig::load("cc-switch")?;
        let db_path = config.resolve_db_path()?;
        db_path.exists().then_some(Self { db_path, config })
    }
    pub fn db_path(&self) -> &std::path::Path {
        &self.db_path
    }
    fn open(&self) -> Result<rusqlite::Connection> {
        rusqlite::Connection::open_with_flags(
            &self.db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .with_context(|| {
            format!(
                "failed to open cc-switch database: {}",
                self.db_path.display()
            )
        })
    }
    fn query_setting(&self, conn: &rusqlite::Connection, key: &str) -> Option<String> {
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![key],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    }

    pub fn import_providers(&self) -> Result<Vec<Provider>> {
        let conn = self.open()?;
        let mut endpoints: HashMap<(String, String), String> = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT provider_id, app_type, url FROM provider_endpoints")?;
            for row in stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (pid, app, url) = row?;
                endpoints.insert((pid, app), url);
            }
        }
        let mut providers: Vec<Provider> = Vec::new();
        let mut seen_names: HashMap<String, usize> = HashMap::new();
        let mut stmt = conn.prepare("SELECT id, app_type, name, settings_config, category, provider_type, meta FROM providers ORDER BY is_current DESC")?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
            ))
        })? {
            let (id, app_type, name, settings_json, category, provider_type, meta_json) = row?;
            let settings: serde_json::Value = match serde_json::from_str(&settings_json) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some((api_key, base_url, npm, extra_env, default_model, models)) =
                extract_provider(&app_type, &settings, &id, &endpoints, &self.config)
            else {
                continue;
            };
            if api_key.is_empty() {
                continue;
            }
            let pt = provider_type
                .or_else(|| category.clone())
                .unwrap_or_else(|| "custom".into());
            let vcc_type = self
                .config
                .provider_type_map
                .get(&format!("{}:{}", pt, app_type))
                .cloned()
                .unwrap_or_else(|| "custom".into());
            let mut env = extra_env;
            if base_url.is_some() {
                for key in self
                    .config
                    .env_known_keys
                    .values()
                    .flat_map(|k| k.all.iter().filter(|k| k.ends_with("BASE_URL")).cloned())
                {
                    env.remove(&key);
                }
            }
            let config = ProviderConfig {
                provider_type: vcc_type.clone(),
                api_key,
                base_url,
                default_model,
                models,
                npm,
                headers: HashMap::new(),
                env,
                model_meta: HashMap::new(),
                extra: HashMap::new(),
            };
            let meta: serde_json::Value = serde_json::from_str(&meta_json).unwrap_or_else(|e| {
                eprintln!("warning: failed to parse provider meta JSON: {}", e);
                serde_json::Value::default()
            });
            let description = meta
                .get("description")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from);
            if let Some(&idx) = seen_names.get(&name) {
                providers[idx]
                    .tool
                    .insert(app_type, ProviderToolOverride::from(config.clone()));
            } else {
                seen_names.insert(name.clone(), providers.len());
                let mut p = Provider::new_with_name(&name);
                p.config = config;
                p.metadata = ccswitch_metadata(description).into();
                providers.push(p);
            }
        }
        Ok(providers)
    }

    pub fn import_mcp_servers(&self) -> Result<Vec<McpServer>> {
        let conn = self.open()?;
        let mut servers = Vec::new();
        let enabled_cols = &self.config.enabled_columns;
        // Validate column names to prevent SQL injection — only allow alphanumeric + underscore
        let col_names: Vec<&str> = enabled_cols
            .keys()
            .filter(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .map(|s| s.as_str())
            .collect();
        let has_cols = !col_names.is_empty()
            && conn
                .prepare(&format!("SELECT {} FROM mcp_servers LIMIT 1", col_names[0]))
                .is_ok();
        let sql = if has_cols {
            format!("SELECT id, name, server_config, description, homepage, docs, tags, {} FROM mcp_servers", col_names.join(", "))
        } else {
            "SELECT id, name, server_config, description, homepage, docs, tags FROM mcp_servers"
                .to_string()
        };
        let col_count = 7 + col_names.len();
        let mut stmt = conn.prepare(&sql)?;
        for row in stmt.query_map([], |r| {
            let mut ev = Vec::new();
            for i in 7..col_count {
                ev.push(r.get::<_, bool>(i)?);
            }
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
                ev,
            ))
        })? {
            let (id, _name, config_json, description, homepage, _docs, tags_json, enabled_vals) =
                row?;
            let cv: serde_json::Value = match serde_json::from_str(&config_json) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let cs_type = cv.get("type").and_then(|v| v.as_str()).unwrap_or("stdio");
            let server_type = self
                .config
                .map_mcp_type(cs_type)
                .unwrap_or_else(|| "stdio".into());
            let mut mcp_config = super::mcp_config_from_json(
                &cv,
                &self.config.mcp_extra_fields,
                self.config.mcp_known_keys(),
            );
            mcp_config.server_type = server_type;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_else(|e| {
                eprintln!("warning: failed to parse MCP tags JSON: {}", e);
                Vec::new()
            });
            let tool = build_enabled_overrides(&col_names, enabled_cols, &enabled_vals, || {
                McpToolOverride {
                    disabled_tools: vec!["*".into()],
                    ..Default::default()
                }
            });
            let mut mcp = McpServer::new_with_name(&id);
            mcp.config = mcp_config;
            mcp.metadata = Metadata {
                description,
                homepage,
                tags: {
                    let mut t = tags;
                    t.push("cc-switch".into());
                    t
                },
            };
            mcp.tool = tool;
            servers.push(mcp);
        }
        Ok(servers)
    }

    pub fn import_hooks(&self) -> Result<Vec<Hook>> {
        let conn = self.open()?;
        let defaults = crate::config::adapter_defaults();
        let hook_key = self
            .config
            .common_config_keys
            .iter()
            .find(|(_, t)| *t == "claude")
            .map(|(k, _)| k.as_str())
            .unwrap_or("common_config_claude");
        let Some(hooks_json) = self.query_setting(&conn, hook_key) else {
            return Ok(Vec::new());
        };
        let config: serde_json::Value = serde_json::from_str(&hooks_json).unwrap_or_else(|e| {
            eprintln!("warning: failed to parse hooks JSON: {}", e);
            serde_json::Value::default()
        });
        let Some(hooks_obj) = config.get("hooks").and_then(|v| v.as_object()) else {
            return Ok(Vec::new());
        };
        let mut hooks = Vec::new();
        for (event, entries) in hooks_obj {
            for entry in entries.as_array().into_iter().flatten() {
                let matcher = entry
                    .get("matcher")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                for (idx, he) in entry
                    .get("hooks")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    let command = match he.get("command").and_then(|v| v.as_str()) {
                        Some(c) => c.to_string(),
                        None => continue,
                    };
                    if he.get("type").and_then(|v| v.as_str()).unwrap_or("command") != "command" {
                        continue;
                    }
                    let name = if he
                        .get("hooks")
                        .and_then(|v| v.as_array())
                        .map_or(true, |a| a.len() <= 1)
                    {
                        format!("{}-{}", event.to_lowercase(), matcher_to_name(&matcher))
                    } else {
                        format!(
                            "{}-{}-{}",
                            event.to_lowercase(),
                            matcher_to_name(&matcher),
                            idx + 1
                        )
                    };
                    let mut hook = Hook::new_with_name(&name);
                    hook.config = crate::model::hook::HookConfig {
                        event: event.clone(),
                        matcher: matcher.clone(),
                        command,
                        timeout: defaults.defaults.hook_timeout,
                    };
                    hook.metadata =
                        ccswitch_metadata(Some(defaults.defaults.sync_description("cc-switch")))
                            .into();
                    hooks.push(hook);
                }
            }
        }
        Ok(hooks)
    }

    pub fn import_envs(&self) -> Result<Vec<Env>> {
        let conn = self.open()?;
        let defaults = crate::config::adapter_defaults();
        let mut all_envs: HashMap<String, HashMap<String, String>> = HashMap::new();
        for (key, tool_name) in &self.config.common_config_keys {
            if let Some(json) = self.query_setting(&conn, key) {
                if let Some(env_obj) = serde_json::from_str::<serde_json::Value>(&json)
                    .ok()
                    .and_then(|c| c.get("env").and_then(|v| v.as_object()).cloned())
                {
                    let vars: HashMap<String, String> = env_obj
                        .iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect();
                    if !vars.is_empty() {
                        all_envs.insert(format!("cc-switch-{}", tool_name), vars);
                    }
                }
            }
        }
        Ok(all_envs
            .into_iter()
            .map(|(name, vars)| {
                let mut env = Env::new_with_name(&name);
                env.config.vars = vars;
                env.metadata =
                    ccswitch_metadata(Some(defaults.defaults.sync_description("cc-switch"))).into();
                env
            })
            .collect())
    }

    pub fn import_prompts(&self) -> Result<Vec<Prompt>> {
        let conn = self.open()?;
        let defaults = crate::config::adapter_defaults();
        let mut stmt = conn.prepare(
            "SELECT id, app_type, name, content, description FROM prompts WHERE enabled = 1",
        )?;
        let rows: Vec<_> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows
            .into_iter()
            .map(|(_, app_type, name, content, description)| {
                let mut prompt = Prompt::new_with_name(&format!(
                    "cc-switch-{}-{}",
                    app_type,
                    sanitize_name(&name)
                ));
                prompt.config.content = content;
                prompt.metadata = ccswitch_metadata(description.or_else(|| {
                    Some(
                        defaults
                            .defaults
                            .sync_description(&format!("cc-switch ({})", app_type)),
                    )
                }))
                .with_tag(app_type)
                .into();
                prompt
            })
            .collect())
    }

    pub fn import_skills(&self) -> Result<Vec<Skill>> {
        let mut skills = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let defaults = crate::config::adapter_defaults();
        let lock_info = load_agents_lock();
        let db_skills = self.load_db_skills()?;
        for (dir, source_tag) in &self.config.resolve_skill_scan_dirs() {
            if !dir.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                if dir_name.starts_with('.')
                    || seen.contains(&dir_name)
                    || !path.join("SKILL.md").exists()
                {
                    continue;
                }
                seen.insert(dir_name.clone());
                let (skill_name, description) = parse_skill_md(&path.join("SKILL.md"));
                let (source, repo, tool_overrides) = if let Some(info) = lock_info.get(&dir_name) {
                    ("github".into(), Some(info.clone()), HashMap::new())
                } else if let Some(info) = db_skills.get(&dir_name) {
                    (
                        info.source.clone(),
                        info.repo.clone(),
                        info.tool_overrides.clone(),
                    )
                } else {
                    ("local".into(), None, HashMap::new())
                };
                let skill_name_clean = sanitize_name(if skill_name.is_empty() {
                    &dir_name
                } else {
                    &skill_name
                });
                let mut skill = Skill::new_with_name(&skill_name_clean);
                skill.config.source = source;
                skill.config.repo = repo;
                skill.config.path = Some(path.to_string_lossy().to_string());
                skill.config.install_method = "symlink".into();
                skill.metadata = ccswitch_metadata(
                    description.or_else(|| Some(defaults.defaults.sync_description(source_tag))),
                )
                .with_tag(source_tag)
                .into();
                skill.tool = tool_overrides;
                skills.push(skill);
            }
        }
        Ok(skills)
    }

    fn load_db_skills(&self) -> Result<HashMap<String, DbSkillInfo>> {
        let conn = self.open()?;
        let mut result: HashMap<String, DbSkillInfo> = HashMap::new();
        let enabled_cols = &self.config.enabled_columns;
        // Validate column names to prevent SQL injection — only allow alphanumeric + underscore
        let col_names: Vec<&str> = enabled_cols
            .keys()
            .filter(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .map(|s| s.as_str())
            .collect();
        let sql = if col_names.is_empty() {
            "SELECT id, name, directory, repo_owner, repo_name FROM skills".to_string()
        } else {
            format!(
                "SELECT id, name, directory, repo_owner, repo_name, {} FROM skills",
                col_names.join(", ")
            )
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Ok(result),
        };
        let col_count = 5 + col_names.len();
        for row in stmt.query_map([], |r| {
            let mut ev = Vec::new();
            for i in 5..col_count {
                ev.push(r.get::<_, bool>(i)?);
            }
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                ev,
            ))
        })? {
            let (id, _name, directory, repo_owner, repo_name, enabled_vals) = row?;
            let dir_name = std::path::Path::new(&directory)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&id)
                .to_string();
            let (source, repo) = match (repo_owner, repo_name) {
                (Some(owner), Some(name)) => ("github".into(), Some(format!("{}/{}", owner, name))),
                _ => ("local".into(), None),
            };
            let tool_overrides =
                build_enabled_overrides(&col_names, enabled_cols, &enabled_vals, || {
                    SkillToolOverride { disabled: true }
                });
            result.insert(
                dir_name,
                DbSkillInfo {
                    source,
                    repo,
                    tool_overrides,
                },
            );
        }
        Ok(result)
    }
}

/// Build tool-override entries from enabled-columns data.
/// Shared between MCP servers (McpToolOverride with disabled_tools=["*"])
/// and skills (SkillToolOverride with disabled=true).
fn build_enabled_overrides<T: Default>(
    col_names: &[&str],
    enabled_cols: &HashMap<String, String>,
    enabled_vals: &[bool],
    make_override: impl Fn() -> T,
) -> HashMap<String, T> {
    let mut map = HashMap::new();
    for (i, cn) in col_names.iter().enumerate() {
        if let Some(tn) = enabled_cols.get(*cn) {
            if i < enabled_vals.len() && !enabled_vals[i] {
                map.insert(tn.to_string(), make_override());
            }
        }
    }
    map
}

type ExtractedProvider = (
    String,
    Option<String>,
    Option<String>,
    HashMap<String, String>,
    Option<String>,
    Vec<String>,
);

fn extract_extra_env(env: &serde_json::Value, known_keys: &[String]) -> HashMap<String, String> {
    env.as_object()
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| {
                    if known_keys.iter().any(|kk| kk == k) {
                        None
                    } else {
                        v.as_str().map(|s| (k.clone(), s.to_string()))
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn find_base_url(
    env: &serde_json::Value,
    id: &str,
    app_type: &str,
    endpoints: &HashMap<(String, String), String>,
    url_key: &str,
) -> Option<String> {
    endpoints
        .get(&(id.to_string(), app_type.to_string()))
        .cloned()
        .or_else(|| env.get(url_key).and_then(|v| v.as_str()).map(String::from))
}

fn extract_provider(
    app_type: &str,
    settings: &serde_json::Value,
    id: &str,
    endpoints: &HashMap<(String, String), String>,
    config: &DatasourceConfig,
) -> Option<ExtractedProvider> {
    match app_type {
        "claude" => {
            let env = settings.get("env")?;
            let api_key = json_str(env, "ANTHROPIC_API_KEY");
            let base_url = find_base_url(env, id, app_type, endpoints, "ANTHROPIC_BASE_URL");
            let default_model = json_str_opt(env, "ANTHROPIC_MODEL");
            let mut models = default_model.iter().cloned().collect::<Vec<_>>();
            for key in config.model_env_keys_for("claude") {
                let mid = json_str(env, key);
                if !mid.is_empty() && !models.contains(&mid) {
                    models.push(mid);
                }
            }
            Some((
                api_key,
                base_url,
                None,
                extract_extra_env(env, config.env_known_keys_for("claude")),
                default_model,
                if models.len() <= 1 {
                    Vec::new()
                } else {
                    models
                },
            ))
        }
        "codex" => {
            let auth = settings.get("auth")?;
            let api_key = if json_str_or(auth, "auth_mode", "") == "chatgpt" {
                String::new()
            } else {
                auth.get("OPENAI_API_KEY")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| {
                        settings
                            .get("env")
                            .map(|e| json_str(e, "OPENAI_API_KEY"))
                            .unwrap_or_default()
                    })
            };
            let base_url = endpoints
                .get(&(id.to_string(), app_type.to_string()))
                .cloned()
                .or_else(|| {
                    settings
                        .get("config")
                        .and_then(|v| v.as_str())
                        .and_then(|s| {
                            toml::from_str::<toml::Value>(s)
                                .ok()?
                                .get("model_providers")
                                .and_then(|mp| mp.as_table())?
                                .values()
                                .filter_map(|v| {
                                    v.get("base_url").and_then(|u| u.as_str()).map(String::from)
                                })
                                .next()
                        })
                });
            let default_model = settings
                .get("config")
                .and_then(|v| v.as_str())
                .and_then(|s| {
                    toml::from_str::<toml::Value>(s)
                        .ok()?
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                });
            Some((
                api_key,
                base_url,
                None,
                extract_extra_env(
                    settings.get("env").unwrap_or(&serde_json::Value::Null),
                    config.env_known_keys_for("codex"),
                ),
                default_model,
                Vec::new(),
            ))
        }
        "opencode" => {
            let options = settings.get("options")?;
            let api_key = json_str(options, "apiKey");
            let base_url = endpoints
                .get(&(id.to_string(), app_type.to_string()))
                .cloned()
                .or_else(|| json_str_opt(options, "baseURL"));
            let default_model = settings
                .get("models")
                .and_then(|m| m.as_object())
                .and_then(|models| models.keys().next().cloned());
            Some((
                api_key,
                base_url,
                json_str_opt(settings, "npm"),
                extract_extra_env(settings.get("env").unwrap_or(&serde_json::Value::Null), &[]),
                default_model,
                Vec::new(),
            ))
        }
        "gemini" => {
            let env = settings.get("env")?;
            let known_keys = config.env_known_keys_for("gemini");
            let model_keys = config.model_env_keys_for("gemini");
            let api_key = known_keys
                .iter()
                .filter(|k| !model_keys.contains(k) && !k.ends_with("BASE_URL"))
                .find_map(|k| env.get(k).and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let base_url = endpoints
                .get(&(id.to_string(), app_type.to_string()))
                .cloned()
                .or_else(|| {
                    known_keys
                        .iter()
                        .filter(|k| k.ends_with("BASE_URL"))
                        .find_map(|k| env.get(k).and_then(|v| v.as_str()).map(String::from))
                });
            Some((
                api_key,
                base_url,
                None,
                extract_extra_env(env, known_keys),
                None,
                Vec::new(),
            ))
        }
        _ => None,
    }
}

fn ccswitch_metadata(description: Option<String>) -> MetadataBuilder {
    MetadataBuilder {
        description,
        extra_tags: Vec::new(),
    }
}

struct MetadataBuilder {
    description: Option<String>,
    extra_tags: Vec<String>,
}
impl MetadataBuilder {
    fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.extra_tags.push(tag.into());
        self
    }
    fn build(self) -> Metadata {
        let defaults = crate::config::adapter_defaults();
        let mut tags = vec![defaults.defaults.sync_tag.clone()];
        tags.extend(self.extra_tags);
        Metadata {
            description: self.description,
            homepage: None,
            tags,
        }
    }
}
impl From<MetadataBuilder> for Metadata {
    fn from(b: MetadataBuilder) -> Self {
        b.build()
    }
}

fn matcher_to_name(m: &str) -> String {
    if m.is_empty() || m == ".*" {
        "all".into()
    } else {
        super::sanitize_name(m)
    }
}

fn load_agents_lock() -> HashMap<String, String> {
    let Some(lock_path) = dirs::home_dir()
        .map(|h| h.join(".agents").join(".skill-lock.json"))
        .filter(|p| p.exists())
    else {
        return HashMap::new();
    };
    let lock: serde_json::Value = std::fs::read_to_string(&lock_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    lock.get("skills")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(n, i)| {
                    i.get("source")
                        .and_then(|v| v.as_str())
                        .map(|s| (n.clone(), s.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_skill_md(path: &std::path::Path) -> (String, Option<String>) {
    let (fm, _) = std::fs::read_to_string(path)
        .ok()
        .map(|c| crate::adapter::agent_format::parse_markdown_frontmatter(&c))
        .unwrap_or_default();
    (
        fm.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        fm.get("description")
            .and_then(|v| v.as_str())
            .map(String::from),
    )
}

struct DbSkillInfo {
    source: String,
    repo: Option<String>,
    tool_overrides: HashMap<String, SkillToolOverride>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── matcher_to_name ──

    #[test]
    fn test_matcher_to_name_empty() {
        assert_eq!(matcher_to_name(""), "all");
    }

    #[test]
    fn test_matcher_to_name_wildcard() {
        assert_eq!(matcher_to_name(".*"), "all");
    }

    #[test]
    fn test_matcher_to_name_pattern() {
        assert_eq!(matcher_to_name("Bash"), "Bash");
    }

    #[test]
    fn test_matcher_to_name_special_chars() {
        assert_eq!(matcher_to_name("Read|Write"), "Read-Write");
    }

    // ── build_enabled_overrides ──

    #[test]
    fn test_build_enabled_overrides_all_enabled() {
        let col_names = vec!["col_a", "col_b"];
        let mut enabled_cols = HashMap::new();
        enabled_cols.insert("col_a".to_string(), "tool_a".to_string());
        enabled_cols.insert("col_b".to_string(), "tool_b".to_string());
        let enabled_vals = vec![true, true];
        let result: HashMap<String, String> =
            build_enabled_overrides(&col_names, &enabled_cols, &enabled_vals, || {
                "disabled".to_string()
            });
        assert!(result.is_empty());
    }

    #[test]
    fn test_build_enabled_overrides_some_disabled() {
        let col_names = vec!["col_a", "col_b"];
        let mut enabled_cols = HashMap::new();
        enabled_cols.insert("col_a".to_string(), "tool_a".to_string());
        enabled_cols.insert("col_b".to_string(), "tool_b".to_string());
        let enabled_vals = vec![true, false];
        let result: HashMap<String, String> =
            build_enabled_overrides(&col_names, &enabled_cols, &enabled_vals, || {
                "disabled".to_string()
            });
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("tool_b").unwrap(), "disabled");
    }

    #[test]
    fn test_build_enabled_overrides_no_matching_col() {
        let col_names = vec!["col_a"];
        let enabled_cols = HashMap::new();
        let enabled_vals = vec![false];
        let result: HashMap<String, String> =
            build_enabled_overrides(&col_names, &enabled_cols, &enabled_vals, || {
                "disabled".to_string()
            });
        assert!(result.is_empty());
    }

    #[test]
    fn test_build_enabled_overrides_short_vals() {
        let col_names = vec!["col_a", "col_b"];
        let mut enabled_cols = HashMap::new();
        enabled_cols.insert("col_a".to_string(), "tool_a".to_string());
        enabled_cols.insert("col_b".to_string(), "tool_b".to_string());
        let enabled_vals = vec![false]; // only 1 value for 2 columns
        let result: HashMap<String, String> =
            build_enabled_overrides(&col_names, &enabled_cols, &enabled_vals, || {
                "disabled".to_string()
            });
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("tool_a").unwrap(), "disabled");
    }

    // ── extract_extra_env ──

    #[test]
    fn test_extract_extra_env_basic() {
        let env = serde_json::json!({"ANTHROPIC_API_KEY": "sk-123", "CUSTOM_VAR": "hello"});
        let known: Vec<String> = vec!["ANTHROPIC_API_KEY".to_string()];
        let result = extract_extra_env(&env, &known);
        assert!(!result.contains_key("ANTHROPIC_API_KEY"));
        assert_eq!(result.get("CUSTOM_VAR").unwrap(), "hello");
    }

    #[test]
    fn test_extract_extra_env_non_string_skipped() {
        let env = serde_json::json!({"KEY": 42, "STR": "val"});
        let result = extract_extra_env(&env, &[]);
        assert!(!result.contains_key("KEY"));
        assert_eq!(result.get("STR").unwrap(), "val");
    }

    #[test]
    fn test_extract_extra_env_not_object() {
        let env = serde_json::json!("not an object");
        let result = extract_extra_env(&env, &[]);
        assert!(result.is_empty());
    }

    // ── find_base_url ──

    #[test]
    fn test_find_base_url_from_endpoints() {
        let mut endpoints = HashMap::new();
        endpoints.insert(
            ("id1".to_string(), "claude".to_string()),
            "https://from-endpoint.com".to_string(),
        );
        let env = serde_json::json!({"ANTHROPIC_BASE_URL": "https://from-env.com"});
        let result = find_base_url(&env, "id1", "claude", &endpoints, "ANTHROPIC_BASE_URL");
        assert_eq!(result.as_deref(), Some("https://from-endpoint.com"));
    }

    #[test]
    fn test_find_base_url_fallback_to_env() {
        let endpoints = HashMap::new();
        let env = serde_json::json!({"ANTHROPIC_BASE_URL": "https://from-env.com"});
        let result = find_base_url(&env, "id1", "claude", &endpoints, "ANTHROPIC_BASE_URL");
        assert_eq!(result.as_deref(), Some("https://from-env.com"));
    }

    #[test]
    fn test_find_base_url_not_found() {
        let endpoints = HashMap::new();
        let env = serde_json::json!({});
        let result = find_base_url(&env, "id1", "claude", &endpoints, "ANTHROPIC_BASE_URL");
        assert!(result.is_none());
    }

    // ── sanitize_name (shared from parent) ──

    #[test]
    fn test_sanitize_name_in_ccswitch_context() {
        assert_eq!(super::super::sanitize_name("My Prompt!"), "My-Prompt");
    }
}
