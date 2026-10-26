//! Ordinary resident Post admission and publication failure regression.

use super::*;

#[test]
fn strict_post_failures_close_the_query_and_report_continues() {
    for action in ["withhold_result", "report"] {
        let f = Fixture::new();
        let mut document = setup(&f);
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qoder_sdk");
        fs::copy(source.join("post-synthetic.mjs"), f.root.join("app.mjs")).unwrap();
        document["spec"]["providers"]["policy"]["transport"]["argv"] =
            json!([f.python, source.join("post-policy.py")]);
        document["spec"]["events"]["tool.before"]["steps"][0]["effects"] = json!(["observe"]);
        document["spec"]["events"]["tool.after"]["steps"][0]["operation"] = json!("check");
        document["spec"]["events"]["tool.after"]["steps"][0]["on_error"] = json!(action);
        let service = Service::start(&f, &document);
        let mut command = launch(&f);
        command
            .env("QODER_TEST_ROOT", &f.root)
            .env("QODER_POST_CASE", "failure");
        assert_eq!(
            f.successful(command)["withheld"],
            action == "withhold_result"
        );
        assert!(f.root.join("executed").exists());
        service.released(&f);
    }
}

#[test]
fn strict_post_joins_original_callbacks_and_preserves_native_results() {
    let f = Fixture::new();
    let mut command = Command::new("node");
    command
        .current_dir(&f.root)
        .args(["--test", "--test-reporter=tap"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qoder_sdk/post.test.mjs"));
    let output = f.run(command);
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("# pass 9"));
}
