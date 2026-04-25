use std::cell::Cell;

thread_local! {
    static JSON_MODE: Cell<bool> = const { Cell::new(false) };
}

/// 设置输出模式
pub(crate) fn set_json_mode(json: bool) {
    JSON_MODE.with(|f| f.set(json));
}

/// 是否 JSON 输出模式
pub(crate) fn is_json_mode() -> bool {
    JSON_MODE.with(|f| f.get())
}

/// 输出操作成功消息
pub(crate) fn output_success(message: &str) {
    if is_json_mode() {
        println!(
            "{}",
            serde_json::json!({
                "success": true,
                "message": message
            })
        );
    } else {
        println!("{}", message);
    }
}

/// 输出单个对象的 JSON（show 命令）
pub(crate) fn output_item<T: serde::Serialize>(item: &T) {
    if is_json_mode() {
        println!(
            "{}",
            serde_json::to_string_pretty(item)
                .unwrap_or_else(|e| format!("{{\"error\": \"{}\"}}", e))
        );
    }
    // 非 JSON 模式由调用方自行处理（如 toml 输出）
}

/// 输出列表的 JSON（list 命令）
pub(crate) fn output_list<T: serde::Serialize>(items: &[T]) {
    if is_json_mode() {
        println!(
            "{}",
            serde_json::to_string_pretty(items)
                .unwrap_or_else(|e| format!("{{\"error\": \"{}\"}}", e))
        );
    }
    // 非 JSON 模式由调用方自行处理表格输出
}

/// 输出任意 JSON 值
pub(crate) fn output_json(value: &serde_json::Value) {
    if is_json_mode() {
        println!(
            "{}",
            serde_json::to_string_pretty(value)
                .unwrap_or_else(|e| format!("{{\"error\": \"{}\"}}", e))
        );
    }
    // 非 JSON 模式由调用方自行处理
}

/// Print DRY RUN banner if not in JSON mode.
pub(crate) fn print_dry_run_banner(dry_run: bool) {
    if dry_run && !is_json_mode() {
        println!("=== DRY RUN (no changes will be made) ===\n");
    }
}
