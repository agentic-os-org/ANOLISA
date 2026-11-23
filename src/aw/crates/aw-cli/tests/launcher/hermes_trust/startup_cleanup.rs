//! Native Hermes dispatch releases real registrations before its owner exits.

use super::*;

#[test]
fn hermes_native_dispatch_releases_service_and_assets_before_owner_exit() {
    let fixture = Fixture::new();
    let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("node"))
        .find(|path| path.is_file())
        .expect("native startup fixture requires Node");
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qoder_sdk");
    let application = fixture.root.join("app.mjs");
    fs::copy(source.join("synthetic.mjs"), &application).unwrap();
    let sdk = fixture.root.join("node_modules/@qoder-ai/qoder-agent-sdk");
    fs::create_dir_all(sdk.join("dist")).unwrap();
    fs::write(sdk.join("package.json"), json!({"name":"@qoder-ai/qoder-agent-sdk", "version":"1.0.56", "qoderCliVersion":"1.1.78", "qoderSdkBrand":"global", "type":"module", "main":"dist/index.js"}).to_string()).unwrap();
    fs::copy(source.join("sdk.mjs"), sdk.join("dist/index.js")).unwrap();
    let mut document = fixture.document();
    document["spec"]["agents"] = json!({"sdk":{"adapter":"qoder-sdk","argv":[node,application]}});
    let service = Service::start(&fixture, &document);
    let mut command = Command::new(&fixture.python);
    command
        .current_dir(&fixture.root)
        .arg("-B")
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/hermes/startup_cleanup.py"),
        )
        .arg(env!("CARGO_BIN_EXE_aw"))
        .arg(&fixture.root)
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../aw-adapter/native/hermes/startup.py"),
        );
    let result = fixture.run(command);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        report["cases"],
        json!([
            "return",
            "exit-zero",
            "exit",
            "exception",
            "import",
            "validation"
        ])
    );
    assert_eq!(report["native_assets_removed"], true);
    service.released(&fixture);
}
