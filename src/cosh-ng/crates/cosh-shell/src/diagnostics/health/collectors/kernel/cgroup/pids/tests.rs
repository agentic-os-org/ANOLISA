use super::*;

#[test]
fn pids_events_max_line_parses_only_max() {
    assert_eq!(parse_pids_events_max("low 0\nhigh 0\nmax 5\n"), Some(5));
    assert_eq!(parse_pids_events_max("max 0"), Some(0));
    assert_eq!(parse_pids_events_max("low 1\nhigh 2"), None);
    assert_eq!(parse_pids_events_max("max abc"), None);
    assert_eq!(parse_pids_events_max(""), None);
}

#[test]
fn ancestor_paths_walk_up_to_root() {
    let components: Vec<&str> = "/a/b/c".split('/').filter(|p| !p.is_empty()).collect();
    let path_at = |depth: usize| format!("/{}", components[..components.len() - depth].join("/"));
    assert_eq!(path_at(1), "/a/b");
    assert_eq!(path_at(2), "/a");
    assert_eq!(path_at(3), "/");
}
