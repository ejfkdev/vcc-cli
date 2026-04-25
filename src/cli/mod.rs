pub mod apply_cmd;
pub mod config_cmd;
pub mod dynamic;
pub mod import;
pub mod inspect_cmd;
pub mod output;
pub mod presets;
pub mod profile;
pub mod resource;
pub mod session_cmd;
pub mod status;

/// 解析逗号分隔的字符串为 Vec，自动 trim 并过滤空值
pub(crate) fn parse_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// 对 Vec<String> 执行增量添加/移除操作
pub(crate) fn add_remove_vec(vec: &mut Vec<String>, add: Option<&str>, remove: Option<&str>) {
    if let Some(a) = add {
        for item in parse_csv(a) {
            if !vec.contains(&item) {
                vec.push(item);
            }
        }
    }
    if let Some(r) = remove {
        let to_remove: std::collections::HashSet<String> = parse_csv(r).into_iter().collect();
        vec.retain(|x| !to_remove.contains(x));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_csv ──

    #[test]
    fn test_parse_csv_basic() {
        assert_eq!(parse_csv("a,b,c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_parse_csv_trims_whitespace() {
        assert_eq!(parse_csv(" a , b , c "), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_parse_csv_single() {
        assert_eq!(parse_csv("hello"), vec!["hello"]);
    }

    #[test]
    fn test_parse_csv_empty() {
        assert!(parse_csv("").is_empty());
    }

    #[test]
    fn test_parse_csv_filters_empty_parts() {
        assert_eq!(parse_csv("a,,b,,"), vec!["a", "b"]);
    }

    #[test]
    fn test_parse_csv_whitespace_only() {
        assert!(parse_csv("   ").is_empty());
        assert!(parse_csv(" , , ").is_empty());
    }

    #[test]
    fn test_parse_csv_trailing_comma() {
        assert_eq!(parse_csv("a,b,"), vec!["a", "b"]);
    }

    #[test]
    fn test_parse_csv_leading_comma() {
        assert_eq!(parse_csv(",a,b"), vec!["a", "b"]);
    }

    // ── add_remove_vec ──

    #[test]
    fn test_add_remove_vec_add() {
        let mut v = vec!["a".into()];
        add_remove_vec(&mut v, Some("b,c"), None);
        assert_eq!(v, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_add_remove_vec_remove() {
        let mut v = vec!["a".into(), "b".into(), "c".into()];
        add_remove_vec(&mut v, None, Some("b"));
        assert_eq!(v, vec!["a", "c"]);
    }

    #[test]
    fn test_add_remove_vec_add_and_remove() {
        let mut v = vec!["a".into()];
        add_remove_vec(&mut v, Some("d"), Some("a"));
        assert_eq!(v, vec!["d"]);
    }

    #[test]
    fn test_add_remove_vec_no_dup_on_add() {
        let mut v = vec!["a".into()];
        add_remove_vec(&mut v, Some("a"), None);
        assert_eq!(v, vec!["a"]);
    }

    #[test]
    fn test_add_remove_vec_none_ops() {
        let mut v = vec!["a".into()];
        add_remove_vec(&mut v, None, None);
        assert_eq!(v, vec!["a"]);
    }

    #[test]
    fn test_add_remove_vec_remove_nonexistent() {
        let mut v = vec!["a".into()];
        add_remove_vec(&mut v, None, Some("z"));
        assert_eq!(v, vec!["a"]);
    }

    #[test]
    fn test_add_remove_vec_remove_multiple() {
        let mut v = vec!["a".into(), "b".into(), "c".into()];
        add_remove_vec(&mut v, None, Some("a,c"));
        assert_eq!(v, vec!["b"]);
    }
}
