use asc_intent::{ActionCategory, PathScope, summarize};

#[test]
fn summarizes_read_scope_and_unconstrained_network() {
    let text = concat!(
        r#"permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File) when { resource.path == "/tmp/report.csv" };"#,
        "\n",
        r#"permit(principal, action == SecCore::Action::"network.use", resource is SecCore::Network);"#,
    );
    let summary = summarize(text).unwrap();
    assert_eq!(summary.permits.len(), 2);
    assert_eq!(summary.permits[0].action, ActionCategory::Read);
    assert_eq!(
        summary.permits[0].path,
        Some(PathScope::Exact("/tmp/report.csv".into()))
    );
    assert_eq!(summary.permits[1].action, ActionCategory::Network);
    assert_eq!(summary.permits[1].path, None);
    assert!(summary.forbids.is_empty());
}

#[test]
fn summarizes_whole_network_equality() {
    // Models write `== SecCore::Network::"all"` for the single enum value; it states the same
    // whole-network scope as an unconstrained resource.
    let text = r#"permit(principal, action == SecCore::Action::"network.use", resource == SecCore::Network::"all");"#;
    let summary = summarize(text).unwrap();
    assert_eq!(summary.permits.len(), 1);
    assert_eq!(summary.permits[0].action, ActionCategory::Network);
    assert_eq!(summary.permits[0].path, None);
}

#[test]
fn summarizes_write_operation_program_and_subtree() {
    let text = r#"permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File) when { resource.path like "/work/*" && context.input.operation == SecCore::WriteOperation::"create" && context.input.program == "/usr/bin/touch" };"#;
    let summary = summarize(text).unwrap();
    let permission = &summary.permits[0];
    assert_eq!(permission.action, ActionCategory::Write);
    assert_eq!(permission.path, Some(PathScope::Subtree("/work".into())));
    assert_eq!(
        permission.path.as_ref().map(PathScope::display).as_deref(),
        Some("/work/*")
    );
    assert_eq!(permission.operation.as_deref(), Some("create"));
    assert_eq!(permission.program.as_deref(), Some("/usr/bin/touch"));
}

#[test]
fn folds_the_exact_or_subtree_disjunction_into_one_scope() {
    let text = r#"permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File) when { resource.path == "/work/crates" || resource.path like "/work/crates/*" };"#;
    let summary = summarize(text).unwrap();
    assert_eq!(
        summary.permits[0].path,
        Some(PathScope::Subtree("/work/crates".into()))
    );
}

#[test]
fn keeps_forbidden_paths_out_of_permits() {
    let text = r#"forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File) when { resource.path == "/etc/shadow" };"#;
    let summary = summarize(text).unwrap();
    assert!(summary.permits.is_empty());
    assert_eq!(
        summary.forbids[0].path,
        Some(PathScope::Exact("/etc/shadow".into()))
    );
}

#[test]
fn unsupported_constructs_fall_back_to_exact_text() {
    for text in [
        r#"permit(principal, action == SecCore::Action::"file.chmod", resource is SecCore::File);"#,
        r#"permit(principal, action == SecCore::Action::"file.read", resource is SecCore::Process);"#,
        r#"permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File) when { resource.path != "/etc/shadow" };"#,
        r#"permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File) when { resource.path == "/a" || resource.path == "/b" };"#,
    ] {
        assert!(summarize(text).is_none(), "{text}");
    }
}
