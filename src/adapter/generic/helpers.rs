use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::adapter::agent_format::{format_agent_markdown, format_agent_yaml};
use crate::adapter::doc_engine::{DocFormat, DocTree, DocValue};
use crate::adapter::mapping::{
    mcp_from_toml, mcp_to_toml_value, mcp_to_tool_json, tool_json_to_mcp, McpMappingConfig,
};
use crate::adapter::{copy_dir_recursive, SyncItem, SyncResult};
use crate::model::{
    agent::Agent,
    env::Env,
    hook::{Hook, HookConfig, HookToolOverride},
    mcp::McpServer,
    Resource,
};
use crate::store::TomlStore;

/// Ensure parent directory exists (no-op if already does).
fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

/// Ensure directory exists (creates if needed, no-op if already does).
pub(crate) fn ensure_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

/// Save a resource if not dry_run and it validates.
pub(crate) fn save_if_ok<T: Resource>(resource: &T, store: &TomlStore, dry_run: bool) {
    if !dry_run && resource.validate().is_ok() {
        if let Err(e) = store.save_resource(resource) {
            eprintln!(
                "warning: failed to save resource '{}': {}",
                resource.name(),
                e
            );
        }
    }
}

/// 从 store 加载资源，失败则打印跳过信息并返回 None
pub(crate) fn load_or_skip<T: Resource>(store: &TomlStore, kind: &str, name: &str) -> Option<T> {
    match store.load_resource(kind, name) {
        Ok(r) => Some(r),
        Err(e) => {
            println!("  skipped {} '{}': {}", kind, name, e);
            None
        }
    }
}

/// 遍历名称列表，对每个成功加载的资源执行操作，返回成功数量
pub(crate) fn load_each<T: Resource, F>(
    store: &TomlStore,
    kind: &str,
    names: &[String],
    mut f: F,
) -> usize
where
    F: FnMut(T),
{
    let mut applied = 0;
    for name in names {
        if let Some(resource) = load_or_skip::<T>(store, kind, name) {
            f(resource);
            applied += 1;
        }
    }
    applied
}

/// 打印 apply/add 结果状态（dry-run 或实际应用）
pub(crate) fn print_apply_status(kind: &str, count: usize, path: &Path, dry_run: bool) {
    if dry_run {
        println!("  [dry-run] {} ({}) → {}", kind, count, path.display());
    } else if count > 0 {
        println!("  {} ({}) → {}", kind, count, path.display());
    }
}

/// 通用 read-modify-write 助手：加载文档 → 修改 → 保存
/// dry_run=true 时只执行修改逻辑但不保存
pub(crate) fn with_doc<F, R>(path: &Path, format: DocFormat, dry_run: bool, f: F) -> Result<R>
where
    F: FnOnce(&mut DocTree) -> Result<R>,
{
    let mut doc = DocTree::load(format, path)?;
    let result = f(&mut doc)?;
    if !dry_run {
        ensure_parent(path)?;
        doc.save_to(path)?;
    }
    Ok(result)
}

/// 在 DocTree 的指定 section 中插入 MCP 条目
pub(crate) fn insert_mcp_to_doc(
    doc: &mut DocTree,
    servers_key: &str,
    mapping: &McpMappingConfig,
    name: &str,
    mcp: &McpServer,
    tool_name: &str,
) {
    let value = match doc.format() {
        DocFormat::Toml => mcp_to_toml_value(mapping, mcp).into(),
        _ => mcp_to_tool_json(mapping, mcp, tool_name).into(),
    };
    doc.set_entry(servers_key, name, value);
}

/// 从 DocTree 的指定 section 读取 MCP 条目
pub(crate) fn mcp_from_doc(
    doc: &DocTree,
    servers_key: &str,
    mapping: &McpMappingConfig,
    name: &str,
    tool_name: &str,
) -> Option<McpServer> {
    let entries = doc.entries(servers_key)?;
    let (_, val) = entries.into_iter().find(|(k, _)| k == name)?;
    match doc.format() {
        DocFormat::Toml => {
            let toml_val: toml::Value = val.clone().into();
            mcp_from_toml(mapping, name, &toml_val, tool_name)
        }
        _ => {
            let json_val: serde_json::Value = val.clone().into();
            tool_json_to_mcp(mapping, name, &json_val, tool_name)
        }
    }
}

/// 构建 sync 用的标准 Metadata
pub(crate) fn sync_metadata(tool_name: &str) -> crate::model::Metadata {
    crate::model::Metadata {
        description: Some(
            crate::config::adapter_defaults()
                .defaults
                .sync_description(tool_name),
        ),
        tags: vec![
            crate::config::adapter_defaults().defaults.sync_tag.clone(),
            tool_name.to_string(),
        ],
        ..Default::default()
    }
}

/// 通用同步条目：exists → skip, else → create
/// 如果提供 is_same，则 exists+unchanged → skip, exists+changed → update
/// Sync entry: if resource exists in store, skip; otherwise create.
pub(crate) fn sync_entry_skip<T: Resource>(
    store: &TomlStore,
    kind: &str,
    resource: &T,
    dry_run: bool,
    result: &mut SyncResult,
) {
    let name = resource.name();
    if store.resource_exists(kind, name) {
        result.skipped.push(SyncItem::new(kind, name));
    } else {
        save_if_ok(resource, store, dry_run);
        result.created.push(SyncItem::new(kind, name));
    }
}

/// Sync entry: if resource exists and is unchanged, skip; if changed, update; if new, create.
pub(crate) fn sync_entry_merge<T: Resource, F>(
    store: &TomlStore,
    kind: &str,
    resource: &T,
    dry_run: bool,
    result: &mut SyncResult,
    is_same: F,
) where
    F: Fn(&T, &T) -> bool,
{
    let name = resource.name();
    if store.resource_exists(kind, name) {
        let existing: T = match store.load_resource(kind, name) {
            Ok(e) => e,
            Err(_) => {
                result.skipped.push(SyncItem::new(kind, name));
                return;
            }
        };
        if is_same(&existing, resource) {
            result.skipped.push(SyncItem::new(kind, name));
        } else {
            save_if_ok(resource, store, dry_run);
            result.updated.push(SyncItem::new(kind, name));
        }
    } else {
        save_if_ok(resource, store, dry_run);
        result.created.push(SyncItem::new(kind, name));
    }
}

/// 构造从工具同步来的 Hook
pub(crate) fn build_synced_hook(
    event: &str,
    matcher: &str,
    command: String,
    timeout: u64,
    index: usize,
    tool_name: &str,
    tool_extra: HashMap<String, toml::Value>,
) -> Hook {
    let safe_matcher = matcher.replace(|c: char| !c.is_alphanumeric() && c != '-', "");
    let matcher_clean = if matcher == ".*" || matcher.is_empty() {
        ""
    } else {
        matcher
    };
    let hook_name = if safe_matcher.is_empty() || matcher_clean.is_empty() {
        format!("{}-{}", event.to_lowercase(), index)
    } else {
        format!("{}-{}-{}", event.to_lowercase(), safe_matcher, index)
    };

    let tool_overrides = if tool_extra.is_empty() {
        HashMap::new()
    } else {
        HashMap::from([(
            tool_name.to_string(),
            HookToolOverride {
                extra: tool_extra,
                ..Default::default()
            },
        )])
    };

    Hook {
        name: hook_name,
        id: String::new(),
        r#type: "hook".to_string(),
        config: HookConfig {
            event: event.to_string(),
            matcher: matcher_clean.to_string(),
            command,
            timeout,
        },
        metadata: sync_metadata(tool_name),
        tool: tool_overrides,
    }
}

/// 将 Hook 转换为 DocValue 条目（用于 TOML [[hooks]] 数组）
pub(crate) fn hook_to_doc_entry(hook: &Hook) -> DocValue {
    let mut map = HashMap::from([
        (
            "event".to_string(),
            DocValue::String(hook.config.event.clone()),
        ),
        (
            "command".to_string(),
            DocValue::String(hook.config.command.clone()),
        ),
    ]);
    if !hook.config.matcher.is_empty() {
        map.insert(
            "matcher".to_string(),
            DocValue::String(hook.config.matcher.clone()),
        );
    }
    if hook.config.timeout != crate::config::adapter_defaults().defaults.hook_timeout {
        map.insert(
            "timeout".to_string(),
            DocValue::Integer(hook.config.timeout as i64),
        );
    }
    DocValue::Object(map)
}

/// 收集所有 Hook：默认 Hook + Profile 中启用的 Hook
pub(crate) fn collect_all_hooks(
    store: &TomlStore,
    profile_hooks: &[String],
    tool_name: &str,
) -> Vec<Hook> {
    let mut hooks: Vec<Hook> =
        crate::adapter::load_default(store, "hook", tool_name, crate::adapter::resolve_hook)
            .ok()
            .flatten()
            .into_iter()
            .collect();
    hooks.extend(
        profile_hooks
            .iter()
            .filter_map(|name| load_or_skip::<Hook>(store, "hook", name))
            .map(|raw| crate::adapter::resolve_hook(&raw, tool_name)),
    );
    hooks
}

/// 生成插件引用名（如 "my-plugin@anthropic-official"）
pub(crate) fn plugin_ref_name(plugin: &crate::model::plugin::Plugin) -> String {
    plugin.config.marketplace.as_ref().map_or_else(
        || plugin.name.clone(),
        |mp| format!("{}@{}", plugin.name, mp),
    )
}

/// 解析插件引用名，返回 (name, marketplace)
pub(crate) fn parse_plugin_ref(ref_name: &str) -> (String, Option<String>) {
    match ref_name.rsplit_once('@') {
        Some((name, mp)) => (name.to_string(), Some(mp.to_string())),
        None => (ref_name.to_string(), None),
    }
}

/// 清理目录中不在启用列表里的旧条目（仅清理 symlink，保留用户自建目录）
pub(crate) fn clean_stale_dir_entries(dir: &Path, enabled: &std::collections::HashSet<&String>) {
    if !dir.is_dir() {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !enabled.contains(&name) {
                let path = entry.path();
                // Only remove symlinks (vcc-managed), skip user-created directories
                if path.is_symlink() {
                    if let Err(e) = std::fs::remove_file(&path) {
                        eprintln!("warning: failed to remove '{}': {}", path.display(), e);
                    }
                }
            }
        }
    }
}

/// 链接单个 skill 到目标目录（返回 true 表示成功链接）
pub(crate) fn link_skill(
    skill: &crate::model::skill::Skill,
    target: &Path,
    store: &TomlStore,
    dry_run: bool,
) -> Result<bool> {
    let src_path = match skill.config.source.as_str() {
        "local" => {
            let src = skill.config.path.as_deref().unwrap_or(".");
            if !Path::new(src).exists() {
                println!(
                    "  skipped skill '{}': source path not found: {}",
                    skill.name, src
                );
                return Ok(false);
            }
            Path::new(src).to_path_buf()
        }
        "github" => {
            let cache_dir = store.root().join("cache").join("skills").join(&skill.name);
            if !cache_dir.exists() {
                println!(
                    "  skill '{}' not installed yet. Use 'vcc skill install {}' first.",
                    skill.name, skill.name
                );
                return Ok(false);
            }
            cache_dir
        }
        _ => {
            println!(
                "  skipped skill '{}': unsupported source '{}'",
                skill.name, skill.config.source
            );
            return Ok(false);
        }
    };
    if !dry_run {
        ensure_parent(target)?;
        apply_skill_link(target, &src_path, &skill.config.install_method)?;
    }
    Ok(true)
}

pub(crate) fn apply_skill_link(target: &Path, source: &Path, method: &str) -> Result<()> {
    if target.exists() || target.symlink_metadata().is_ok() {
        if target.is_symlink() {
            std::fs::remove_file(target)?;
        } else {
            std::fs::remove_dir_all(target)?;
        }
    }
    match method {
        "symlink" => {
            std::os::unix::fs::symlink(source, target).with_context(|| {
                format!(
                    "failed to symlink {} -> {}",
                    source.display(),
                    target.display()
                )
            })?;
        }
        "copy" => {
            copy_dir_recursive(source, target)?;
        }
        _ => bail!("unsupported install method: {}", method),
    }
    Ok(())
}

/// 通用目录遍历辅助：遍历 dir 下满足 filter 的条目，用 builder 构造 InspectItem
pub(crate) fn inspect_dir<F, B>(
    dir: std::path::PathBuf,
    filter: F,
    builder: B,
) -> Result<Vec<crate::adapter::InspectItem>>
where
    F: Fn(&Path) -> bool,
    B: Fn(String, &Path) -> crate::adapter::InspectItem,
{
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    std::fs::read_dir(&dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?
        .flatten()
        .filter(|e| filter(&e.path()))
        .map(|e| {
            let path = e.path();
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            Ok(builder(name, &path))
        })
        .collect()
}

/// 从目录中删除指定名称的条目（支持文件/符号链接/目录）
pub(crate) fn remove_dir_entries(
    dir: &Path,
    names: &[String],
    dry_run: bool,
    recursive: bool,
) -> Result<usize> {
    let mut removed = 0;
    for name in names {
        let target = dir.join(name);
        if target.is_symlink() || target.exists() {
            if !dry_run {
                if target.is_symlink() || !recursive {
                    std::fs::remove_file(&target)?;
                } else {
                    std::fs::remove_dir_all(&target)?;
                }
            }
            removed += 1;
        }
    }
    Ok(removed)
}

/// Write an agent file to disk, handling both YAML and Markdown formats.
/// Creates the parent directory if needed. Also writes the system prompt file for YAML agents.
pub(crate) fn write_agent_file(
    agent: &Agent,
    agents_dir: &Path,
    is_yaml: bool,
    ext: &str,
    dry_run: bool,
) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    if !agents_dir.exists() {
        std::fs::create_dir_all(agents_dir)?;
    }
    let target = agents_dir.join(format!("{}.{}", agent.name, ext));
    if is_yaml {
        std::fs::write(&target, format_agent_yaml(agent))?;
        if let Some(ref c) = agent.config.content {
            std::fs::write(agents_dir.join(format!("{}-system.md", agent.name)), c)?;
        }
    } else {
        std::fs::write(&target, format_agent_markdown(agent))?;
    }
    Ok(())
}

/// Write resolved env vars into a DocTree under the given section.
/// Loads each env by name from the store, resolves it for the tool, and sets entries.
/// Optionally applies extra_env overrides after the named envs.
/// Returns the number of env resources successfully written.
pub(crate) fn set_env_vars_in_doc(
    doc: &mut DocTree,
    section_key: &str,
    store: &TomlStore,
    names: &[String],
    tool_name: &str,
    resolve_fn: fn(&Env, &str) -> Env,
    extra_env: Option<&HashMap<String, String>>,
) -> usize {
    // Clear existing env vars before applying new ones, to remove stale entries
    doc.clear_section(section_key);
    doc.ensure_object(section_key);
    let mut count = 0;
    for name in names {
        let raw: Env = match load_or_skip(store, "env", name) {
            Some(e) => e,
            None => continue,
        };
        let env = resolve_fn(&raw, tool_name);
        for (k, v) in &env.config.vars {
            doc.set_entry(section_key, k, DocValue::from(serde_json::json!(v)));
        }
        count += 1;
    }
    if let Some(extra) = extra_env {
        for (k, v) in extra {
            doc.set_entry(section_key, k, DocValue::from(serde_json::json!(v)));
        }
    }
    count
}

/// 删除指定路径列表中的文件
pub(crate) fn remove_files(targets: &[std::path::PathBuf], dry_run: bool) -> Result<usize> {
    let mut removed = 0;
    for target in targets {
        if target.exists() {
            if !dry_run {
                std::fs::remove_file(target)?;
            }
            removed += 1;
        }
    }
    Ok(removed)
}

/// Load DocTree from path: returns None if file missing or parse fails.
pub(crate) fn try_load_doc(format: DocFormat, path: &Path) -> Option<DocTree> {
    if !path.exists() {
        return None;
    }
    DocTree::load(format, path).ok()
}

/// Check if `subset` JSON is a subset of `superset`: all keys in subset exist in superset with equal values.
/// Extra keys in superset are allowed. This is used for sync comparison where the tool config
/// may lose fields that vcc previously stored (env vars, extra fields).
pub(crate) fn json_is_subset(subset: &serde_json::Value, superset: &serde_json::Value) -> bool {
    match (subset, superset) {
        (serde_json::Value::Object(sub), serde_json::Value::Object(sup)) => {
            for (k, v) in sub {
                match sup.get(k) {
                    Some(sv) if !json_is_subset(v, sv) => return false,
                    None => return false,
                    _ => {}
                }
            }
            true
        }
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| json_is_subset(x, y))
        }
        _ => subset == superset,
    }
}

/// Load a doc and apply `f`, returning `default` if the file doesn't exist.
pub(crate) fn with_doc_if_exists<R>(
    format: DocFormat,
    path: &Path,
    default: R,
    f: impl FnOnce(DocTree) -> R,
) -> R {
    match try_load_doc(format, path) {
        Some(doc) => f(doc),
        None => default,
    }
}

/// Helper: convert DocTree → serde_json::Value, modify, convert back.
pub(crate) fn with_settings_json_mut<R>(
    doc: &mut DocTree,
    f: impl FnOnce(&mut serde_json::Value) -> R,
) -> R {
    let old_root = std::mem::replace(doc.root_mut(), DocValue::Object(HashMap::new()));
    let mut json: serde_json::Value = old_root.into();
    let result = f(&mut json);
    *doc.root_mut() = json.into();
    result
}

/// A single hook entry extracted from a DocTree, regardless of TOML or JSON format.
pub(crate) struct HookEntry<'a> {
    pub event: &'a str,
    pub command: &'a str,
    pub matcher: &'a str,
    pub timeout: u64,
    pub index: usize,
    pub tool_extra: HashMap<String, toml::Value>,
}

/// Iterate over all hook entries in a DocTree, calling `f` for each one.
/// Handles both TOML ([[hooks]] flat array) and JSON (event→matcher→hooks nested) formats.
#[allow(clippy::too_many_arguments)]
pub(crate) fn for_each_hook_entry<F>(
    doc: &DocTree,
    is_toml: bool,
    section_key: &str,
    events: &[String],
    command_key: &str,
    matcher_key: &str,
    hooks_key: &str,
    mut f: F,
) where
    F: FnMut(HookEntry<'_>),
{
    if is_toml {
        if let Some(arr) = doc.get(section_key).and_then(|v| v.as_array()) {
            for (i, entry) in arr.iter().enumerate() {
                let command = match entry.get_path_str(command_key) {
                    Some(c) => c,
                    None => continue,
                };
                f(HookEntry {
                    event: entry.get_path_str("event").unwrap_or("unknown"),
                    command,
                    matcher: entry.get_path_str(matcher_key).unwrap_or(""),
                    timeout: entry
                        .get_path("timeout")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(crate::config::adapter_defaults().defaults.hook_timeout as i64)
                        as u64,
                    index: i,
                    tool_extra: extract_hook_tool_extra(entry),
                });
            }
        }
    } else {
        for event_name in events {
            // Try nested path first (Claude: hooks.PreToolUse), then top-level (Gemini: SessionStart)
            let event_arr = if !section_key.is_empty() {
                doc.get(&format!("{}.{}", section_key, event_name))
                    .or_else(|| doc.get(event_name))
            } else {
                doc.get(event_name)
            };
            if let Some(event_arr) = event_arr.and_then(|v| v.as_array()) {
                for entry in event_arr {
                    let matcher = entry.get_path_str(matcher_key).unwrap_or(".*");
                    if let Some(hooks_arr) = entry.get_path(hooks_key).and_then(|v| v.as_array()) {
                        for (i, hook_entry) in hooks_arr.iter().enumerate() {
                            let command = match hook_entry.get_path_str(command_key) {
                                Some(c) => c,
                                None => continue,
                            };
                            f(HookEntry {
                                event: event_name,
                                command,
                                matcher,
                                timeout: hook_entry
                                    .get_path("timeout")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(
                                        crate::config::adapter_defaults().defaults.hook_timeout
                                            as i64,
                                    ) as u64,
                                index: i,
                                tool_extra: extract_hook_tool_extra(hook_entry),
                            });
                        }
                    }
                }
            }
        }
    }
}

/// Extract tool_extra metadata from a JSON hook entry (name, description, env).
pub(crate) fn extract_hook_tool_extra(hook_entry: &DocValue) -> HashMap<String, toml::Value> {
    let mut tool_extra: HashMap<String, toml::Value> = HashMap::new();
    for (key, path) in [("name", "name"), ("description", "description")] {
        if let Some(v) = hook_entry.get_path_str(path) {
            tool_extra.insert(key.to_string(), toml::Value::String(v.to_string()));
        }
    }
    if let Some(env_obj) = hook_entry.get_path("env").and_then(|v| v.as_object()) {
        let env_table: toml::map::Map<String, toml::Value> = env_obj
            .iter()
            .filter_map(|(k, v)| {
                v.as_str()
                    .map(|s| (k.clone(), toml::Value::String(s.to_string())))
            })
            .collect();
        if !env_table.is_empty() {
            tool_extra.insert("env".to_string(), toml::Value::Table(env_table));
        }
    }
    tool_extra
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_plugin_ref ──

    #[test]
    fn test_parse_plugin_ref_with_marketplace() {
        let (name, mp) = parse_plugin_ref("myplugin@latest");
        assert_eq!(name, "myplugin");
        assert_eq!(mp, Some("latest".into()));
    }

    #[test]
    fn test_parse_plugin_ref_no_marketplace() {
        let (name, mp) = parse_plugin_ref("myplugin");
        assert_eq!(name, "myplugin");
        assert_eq!(mp, None);
    }

    #[test]
    fn test_parse_plugin_ref_complex_name() {
        let (name, mp) = parse_plugin_ref("@scope/package@latest");
        // rsplit_once splits on last @
        assert_eq!(name, "@scope/package");
        assert_eq!(mp, Some("latest".into()));
    }

    // ── json_is_subset ──

    #[test]
    fn test_json_is_subset_equal() {
        let a = serde_json::json!({"key": "value", "num": 42});
        assert!(json_is_subset(&a, &a));
    }

    #[test]
    fn test_json_is_subset_proper() {
        let subset = serde_json::json!({"key": "value"});
        let superset = serde_json::json!({"key": "value", "extra": "data"});
        assert!(json_is_subset(&subset, &superset));
    }

    #[test]
    fn test_json_is_subset_missing_key() {
        let subset = serde_json::json!({"missing": "key"});
        let superset = serde_json::json!({"key": "value"});
        assert!(!json_is_subset(&subset, &superset));
    }

    #[test]
    fn test_json_is_subset_value_mismatch() {
        let subset = serde_json::json!({"key": "wrong"});
        let superset = serde_json::json!({"key": "value"});
        assert!(!json_is_subset(&subset, &superset));
    }

    #[test]
    fn test_json_is_subset_nested() {
        let subset = serde_json::json!({"a": {"b": 1}});
        let superset = serde_json::json!({"a": {"b": 1, "c": 2}});
        assert!(json_is_subset(&subset, &superset));
    }

    #[test]
    fn test_json_is_subset_arrays() {
        let subset = serde_json::json!([1, 2]);
        let superset = serde_json::json!([1, 2]);
        assert!(json_is_subset(&subset, &superset));
        let diff = serde_json::json!([1, 3]);
        assert!(!json_is_subset(&diff, &superset));
    }

    #[test]
    fn test_json_is_subset_empty_objects() {
        let a = serde_json::json!({});
        assert!(json_is_subset(&a, &a));
    }

    #[test]
    fn test_json_is_subset_primitives() {
        assert!(json_is_subset(
            &serde_json::json!(42),
            &serde_json::json!(42)
        ));
        assert!(!json_is_subset(
            &serde_json::json!(42),
            &serde_json::json!(43)
        ));
        assert!(json_is_subset(
            &serde_json::json!("hello"),
            &serde_json::json!("hello")
        ));
        assert!(!json_is_subset(
            &serde_json::json!("hello"),
            &serde_json::json!("world")
        ));
        assert!(json_is_subset(
            &serde_json::json!(true),
            &serde_json::json!(true)
        ));
        assert!(json_is_subset(
            &serde_json::json!(null),
            &serde_json::json!(null)
        ));
    }

    // ── build_synced_hook ──

    #[test]
    fn test_build_synced_hook_with_matcher() {
        let hook = build_synced_hook(
            "PreToolUse",
            "Read",
            "echo test".into(),
            30,
            0,
            "claude",
            HashMap::new(),
        );
        assert_eq!(hook.name, "pretooluse-Read-0");
        assert_eq!(hook.config.event, "PreToolUse");
        assert_eq!(hook.config.matcher, "Read");
        assert_eq!(hook.config.command, "echo test");
    }

    #[test]
    fn test_build_synced_hook_without_matcher() {
        let hook = build_synced_hook(
            "PostToolUse",
            "",
            "echo done".into(),
            60,
            1,
            "claude",
            HashMap::new(),
        );
        assert_eq!(hook.name, "posttooluse-1");
        assert_eq!(hook.config.matcher, "");
    }

    #[test]
    fn test_build_synced_hook_wildcard_matcher() {
        let hook = build_synced_hook(
            "Notification",
            ".*",
            "notify".into(),
            30,
            2,
            "claude",
            HashMap::new(),
        );
        assert_eq!(hook.name, "notification-2"); // ".*" → empty matcher
        assert_eq!(hook.config.matcher, "");
    }

    #[test]
    fn test_build_synced_hook_special_chars_matcher() {
        let hook = build_synced_hook(
            "PreToolUse",
            "Read|Write",
            "echo".into(),
            30,
            0,
            "claude",
            HashMap::new(),
        );
        // Special chars stripped: "Read|Write" → "ReadWrite"
        assert!(hook.name.contains("ReadWrite"));
    }

    #[test]
    fn test_build_synced_hook_tool_extra() {
        let mut tool_extra = HashMap::new();
        tool_extra.insert("name".into(), toml::Value::String("my-hook".into()));
        let hook = build_synced_hook(
            "PreToolUse",
            "Read",
            "echo".into(),
            30,
            0,
            "claude",
            tool_extra,
        );
        assert!(hook.tool.contains_key("claude"));
        assert_eq!(
            hook.tool["claude"].extra.get("name").unwrap().as_str(),
            Some("my-hook")
        );
    }

    // ── hook_to_doc_entry ──

    #[test]
    fn test_hook_to_doc_entry_basic() {
        let hook = Hook {
            name: "test".into(),
            id: String::new(),
            r#type: "hook".into(),
            config: HookConfig {
                event: "PreToolUse".into(),
                matcher: "Read".into(),
                command: "echo test".into(),
                timeout: crate::config::adapter_defaults().defaults.hook_timeout,
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let entry = hook_to_doc_entry(&hook);
        let obj = entry.as_object().unwrap();
        assert_eq!(obj.get("event").unwrap().as_str(), Some("PreToolUse"));
        assert_eq!(obj.get("command").unwrap().as_str(), Some("echo test"));
        assert_eq!(obj.get("matcher").unwrap().as_str(), Some("Read"));
        // Default timeout should not be included
        assert!(obj.get("timeout").is_none());
    }

    #[test]
    fn test_hook_to_doc_entry_custom_timeout() {
        let hook = Hook {
            name: "test".into(),
            id: String::new(),
            r#type: "hook".into(),
            config: HookConfig {
                event: "PreToolUse".into(),
                matcher: "Read".into(),
                command: "echo test".into(),
                timeout: 120,
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let entry = hook_to_doc_entry(&hook);
        let obj = entry.as_object().unwrap();
        assert_eq!(obj.get("timeout").unwrap().as_i64(), Some(120));
    }

    #[test]
    fn test_hook_to_doc_entry_empty_matcher() {
        let hook = Hook {
            name: "test".into(),
            id: String::new(),
            r#type: "hook".into(),
            config: HookConfig {
                event: "PreToolUse".into(),
                matcher: String::new(),
                command: "echo test".into(),
                timeout: 30,
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        let entry = hook_to_doc_entry(&hook);
        let obj = entry.as_object().unwrap();
        assert!(obj.get("matcher").is_none());
    }

    // ── extract_hook_tool_extra ──

    #[test]
    fn test_extract_hook_tool_extra_with_name() {
        let entry = DocValue::Object(HashMap::from([
            ("name".into(), DocValue::String("my-hook".into())),
            ("command".into(), DocValue::String("echo".into())),
        ]));
        let extra = extract_hook_tool_extra(&entry);
        assert_eq!(extra.get("name").unwrap().as_str(), Some("my-hook"));
    }

    #[test]
    fn test_extract_hook_tool_extra_with_env() {
        let entry = DocValue::Object(HashMap::from([
            ("command".into(), DocValue::String("echo".into())),
            (
                "env".into(),
                DocValue::Object(HashMap::from([
                    ("API_KEY".into(), DocValue::String("sk-123".into())),
                    ("PATH".into(), DocValue::String("/usr/bin".into())),
                ])),
            ),
        ]));
        let extra = extract_hook_tool_extra(&entry);
        let env = extra.get("env").unwrap().as_table().unwrap();
        assert_eq!(env.get("API_KEY").unwrap().as_str(), Some("sk-123"));
        assert_eq!(env.get("PATH").unwrap().as_str(), Some("/usr/bin"));
    }

    #[test]
    fn test_extract_hook_tool_extra_empty() {
        let entry = DocValue::Object(HashMap::from([(
            "command".into(),
            DocValue::String("echo".into()),
        )]));
        let extra = extract_hook_tool_extra(&entry);
        assert!(extra.is_empty());
    }

    #[test]
    fn test_extract_hook_tool_extra_with_description() {
        let entry = DocValue::Object(HashMap::from([
            ("name".into(), DocValue::String("hook-name".into())),
            ("description".into(), DocValue::String("A test hook".into())),
            ("command".into(), DocValue::String("echo".into())),
        ]));
        let extra = extract_hook_tool_extra(&entry);
        assert_eq!(extra.get("name").unwrap().as_str(), Some("hook-name"));
        assert_eq!(
            extra.get("description").unwrap().as_str(),
            Some("A test hook")
        );
    }

    // ── with_settings_json_mut ──

    #[test]
    fn test_with_settings_json_mut() {
        let root = DocValue::Object(HashMap::from([(
            "key".into(),
            DocValue::String("value".into()),
        )]));
        let mut doc = DocTree::new_test(DocFormat::Json, root);
        let result = with_settings_json_mut(&mut doc, |json| {
            json["new_key"] = serde_json::json!("new_value");
            "ok"
        });
        assert_eq!(result, "ok");
        assert_eq!(doc.get_str("new_key"), Some("new_value"));
    }

    // ── plugin_ref_name ──

    #[test]
    fn test_plugin_ref_name_with_marketplace() {
        let plugin = crate::model::plugin::Plugin {
            name: "myplugin".into(),
            id: String::new(),
            r#type: "plugin".into(),
            config: crate::model::PluginConfig {
                source: String::new(),
                repo: None,
                path: None,
                marketplace: Some("latest".into()),
                install_method: String::new(),
                format: None,
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert_eq!(plugin_ref_name(&plugin), "myplugin@latest");
    }

    #[test]
    fn test_plugin_ref_name_without_marketplace() {
        let plugin = crate::model::plugin::Plugin {
            name: "myplugin".into(),
            id: String::new(),
            r#type: "plugin".into(),
            config: crate::model::PluginConfig {
                source: String::new(),
                repo: None,
                path: None,
                marketplace: None,
                install_method: String::new(),
                format: None,
            },
            metadata: Default::default(),
            tool: HashMap::new(),
        };
        assert_eq!(plugin_ref_name(&plugin), "myplugin");
    }

    // ── for_each_hook_entry (TOML) ──

    #[test]
    fn test_for_each_hook_entry_toml() {
        let root = DocValue::Object(HashMap::from([(
            "hooks".into(),
            DocValue::Array(vec![
                DocValue::Object(HashMap::from([
                    ("event".into(), DocValue::String("PreToolUse".into())),
                    ("command".into(), DocValue::String("echo pre".into())),
                ])),
                DocValue::Object(HashMap::from([
                    ("event".into(), DocValue::String("PostToolUse".into())),
                    ("command".into(), DocValue::String("echo post".into())),
                    ("matcher".into(), DocValue::String("Read".into())),
                ])),
            ]),
        )]));
        let doc = DocTree::new_test(DocFormat::Toml, root);

        let mut hooks = Vec::new();
        for_each_hook_entry(
            &doc,
            true,
            "hooks",
            &[],
            "command",
            "matcher",
            "hooks",
            |entry| {
                hooks.push((entry.event.to_string(), entry.command.to_string()));
            },
        );
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0], ("PreToolUse".into(), "echo pre".into()));
        assert_eq!(hooks[1], ("PostToolUse".into(), "echo post".into()));
    }

    // ── for_each_hook_entry (JSON) ──

    #[test]
    fn test_for_each_hook_entry_json() {
        // Matches real Claude format: {"hooks": {"PreToolUse": [{"matcher":".*","hooks":[{"command":"echo json"}]}]}}
        let root = DocValue::Object(HashMap::from([(
            "hooks".into(),
            DocValue::Object(HashMap::from([(
                "PreToolUse".into(),
                DocValue::Array(vec![DocValue::Object(HashMap::from([
                    ("matcher".into(), DocValue::String(".*".into())),
                    (
                        "hooks".into(),
                        DocValue::Array(vec![DocValue::Object(HashMap::from([(
                            "command".into(),
                            DocValue::String("echo json".into()),
                        )]))]),
                    ),
                ]))]),
            )])),
        )]));
        let doc = DocTree::new_test(DocFormat::Json, root);

        let mut hooks = Vec::new();
        for_each_hook_entry(
            &doc,
            false,
            "hooks",
            &["PreToolUse".into()],
            "command",
            "matcher",
            "hooks",
            |entry| {
                hooks.push(entry.command.to_string());
            },
        );
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0], "echo json");
    }

    // Gemini format: events at top level, no "hooks" nesting
    #[test]
    fn test_for_each_hook_entry_json_gemini() {
        let root = DocValue::Object(HashMap::from([(
            "SessionStart".into(),
            DocValue::Array(vec![DocValue::Object(HashMap::from([
                ("matcher".into(), DocValue::String(".*".into())),
                (
                    "hooks".into(),
                    DocValue::Array(vec![DocValue::Object(HashMap::from([(
                        "command".into(),
                        DocValue::String("echo session".into()),
                    )]))]),
                ),
            ]))]),
        )]));
        let doc = DocTree::new_test(DocFormat::Json, root);

        let mut hooks = Vec::new();
        for_each_hook_entry(
            &doc,
            false,
            "hooks",
            &["SessionStart".into()],
            "command",
            "matcher",
            "hooks",
            |entry| {
                hooks.push(entry.command.to_string());
            },
        );
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0], "echo session");
    }

    // ── json_is_subset: more edge cases ──

    #[test]
    fn test_json_is_subset_scalars() {
        assert!(json_is_subset(
            &serde_json::json!(42),
            &serde_json::json!(42)
        ));
        assert!(!json_is_subset(
            &serde_json::json!(42),
            &serde_json::json!(43)
        ));
        assert!(json_is_subset(
            &serde_json::json!("hi"),
            &serde_json::json!("hi")
        ));
        assert!(json_is_subset(
            &serde_json::json!(true),
            &serde_json::json!(true)
        ));
    }

    #[test]
    fn test_json_is_subset_empty_object() {
        let empty = serde_json::json!({});
        let full = serde_json::json!({"a": 1});
        assert!(json_is_subset(&empty, &full));
    }

    #[test]
    fn test_json_is_subset_different_types() {
        assert!(!json_is_subset(
            &serde_json::json!("42"),
            &serde_json::json!(42)
        ));
        assert!(!json_is_subset(
            &serde_json::json!(null),
            &serde_json::json!(0)
        ));
    }

    #[test]
    fn test_json_is_subset_array_length_mismatch() {
        let a = serde_json::json!([1, 2]);
        let b = serde_json::json!([1, 2, 3]);
        assert!(!json_is_subset(&a, &b));
    }

    // ── with_doc_if_exists ──

    #[test]
    fn test_with_doc_if_exists_missing_file() {
        let result = with_doc_if_exists(
            DocFormat::Json,
            Path::new("/nonexistent/path/settings.json"),
            "default",
            |_| "loaded",
        );
        assert_eq!(result, "default");
    }

    // ── sync_metadata ──

    #[test]
    fn test_sync_metadata_has_description() {
        let meta = sync_metadata("claude");
        assert!(meta.description.is_some());
        assert!(meta.description.unwrap().contains("claude"));
    }

    #[test]
    fn test_sync_metadata_has_sync_tag() {
        let meta = sync_metadata("test-tool");
        assert!(meta.tags.iter().any(|t| t == "test-tool"));
    }

    #[test]
    fn test_sync_metadata_homepage_none() {
        let meta = sync_metadata("claude");
        assert!(meta.homepage.is_none());
    }

    // ── insert_mcp_to_doc / mcp_from_doc ──

    #[test]
    fn test_insert_mcp_to_doc_json() {
        let mapping = McpMappingConfig::default();
        let mut doc = DocTree::new_test(DocFormat::Json, DocValue::Object(HashMap::new()));
        doc.ensure_object("mcpServers");
        let mcp = McpServer::new_with_name("fs");
        insert_mcp_to_doc(&mut doc, "mcpServers", &mapping, "fs", &mcp, "claude");
        assert!(doc.get("mcpServers.fs").is_some());
    }

    #[test]
    fn test_insert_mcp_to_doc_toml() {
        let mapping = McpMappingConfig::default();
        let mut doc = DocTree::new_test(DocFormat::Toml, DocValue::Object(HashMap::new()));
        doc.ensure_object("mcpServers");
        let mcp = McpServer::new_with_name("fs");
        insert_mcp_to_doc(&mut doc, "mcpServers", &mapping, "fs", &mcp, "claude");
        assert!(doc.get("mcpServers.fs").is_some());
    }

    #[test]
    fn test_insert_mcp_overwrites_existing() {
        let mapping = McpMappingConfig::default();
        let mut doc = DocTree::new_test(DocFormat::Json, DocValue::Object(HashMap::new()));
        doc.ensure_object("mcpServers");
        let mcp1 = McpServer::new_with_name("fs");
        insert_mcp_to_doc(&mut doc, "mcpServers", &mapping, "fs", &mcp1, "claude");
        let mcp2 = McpServer::new_with_name("fs");
        insert_mcp_to_doc(&mut doc, "mcpServers", &mapping, "fs", &mcp2, "claude");
        // Should still be one entry (overwritten)
        let entries = doc.entries("mcpServers").unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_mcp_from_doc_json() {
        let mapping = McpMappingConfig::default();
        // Build a JSON doc with an MCP entry manually
        let root = DocValue::Object(HashMap::from([(
            "mcpServers".into(),
            DocValue::Object(HashMap::from([(
                "fs".into(),
                DocValue::Object(HashMap::from([(
                    "command".into(),
                    DocValue::String("npx".into()),
                )])),
            )])),
        )]));
        let doc = DocTree::new_test(DocFormat::Json, root);
        let loaded = mcp_from_doc(&doc, "mcpServers", &mapping, "fs", "claude");
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().name, "fs");
    }

    #[test]
    fn test_mcp_from_doc_nonexistent() {
        let mapping = McpMappingConfig::default();
        let doc = DocTree::new_test(DocFormat::Json, DocValue::Object(HashMap::new()));
        let loaded = mcp_from_doc(&doc, "mcpServers", &mapping, "nonexistent", "claude");
        assert!(loaded.is_none());
    }

    // ── remove_files (non-existent files) ──

    #[test]
    fn test_remove_files_nonexistent() {
        let targets = vec![
            std::path::PathBuf::from("/nonexistent/file1"),
            std::path::PathBuf::from("/nonexistent/file2"),
        ];
        let count = remove_files(&targets, false).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_remove_files_dry_run() {
        let tmp = std::env::temp_dir().join("vcc_test_remove_files");
        let _ = std::fs::create_dir_all(&tmp);
        let file = tmp.join("test.txt");
        std::fs::write(&file, "test").unwrap();
        let count = remove_files(std::slice::from_ref(&file), true).unwrap();
        assert_eq!(count, 1);
        assert!(file.exists()); // dry-run, should NOT delete
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── try_load_doc ──

    #[test]
    fn test_try_load_doc_missing_file() {
        let result = try_load_doc(DocFormat::Json, Path::new("/nonexistent/config.json"));
        assert!(result.is_none());
    }

    // ── ensure_dir ──

    #[test]
    fn test_ensure_dir_creates_and_idempotent() {
        let tmp = std::env::temp_dir().join("vcc_test_ensure_dir");
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(ensure_dir(&tmp).is_ok());
        assert!(tmp.is_dir());
        assert!(ensure_dir(&tmp).is_ok()); // idempotent
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── inspect_dir on non-existent dir ──

    #[test]
    fn test_inspect_dir_nonexistent() {
        let result = inspect_dir(
            std::path::PathBuf::from("/nonexistent/dir"),
            |_| true,
            |name, _| crate::adapter::InspectItem {
                name,
                enabled: true,
                detail: String::new(),
            },
        );
        assert!(result.unwrap().is_empty());
    }

    // ── remove_dir_entries on non-existent dir ──

    #[test]
    fn test_remove_dir_entries_nonexistent() {
        let result =
            remove_dir_entries(Path::new("/nonexistent/dir"), &["foo".into()], false, false);
        assert_eq!(result.unwrap(), 0);
    }
}
