use super::*;

#[test]
fn installation_uses_the_adapter_selection_snapshot() {
    let profile = Profile::new("unknown: keep\n");
    let validator = aw_config::Validator::new().unwrap();
    let example = validator
        .parse(include_bytes!("../../../../../examples/aw.hermes.yaml"))
        .unwrap();
    let mut document = example.as_value().clone();
    document["spec"]["agents"]["hermes"]["argv"] = json!(["/snapshot-hermes", "gateway"]);
    let snapshot = validator
        .parse(&serde_json::to_vec(&document).unwrap())
        .unwrap();
    let path = profile.0.join("aw.json");
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let selected = adapter::select(
        snapshot.as_value()["spec"]["agents"]["hermes"]["adapter"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    document["spec"]["agents"]["hermes"]["adapter"] = json!("qoder");
    document["spec"]["agents"]["hermes"]["argv"] = json!(["/replacement-hermes", "chat"]);
    let replacement = serde_json::to_vec(&document).unwrap();
    validator.parse(&replacement).unwrap();
    let saved = profile.0.join("replacement.json");
    fs::write(&saved, replacement).unwrap();
    fs::rename(saved, &path).unwrap();
    let args = Arguments::parse(
        [
            "--config".into(),
            path.to_str().unwrap().into(),
            "--agent".into(),
            "hermes".into(),
            "--native-profile".into(),
            profile.0.to_str().unwrap().into(),
        ]
        .into_iter(),
    )
    .unwrap();
    let error = selected
        .install(&args, &snapshot)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("local chat command only"), "{error}");
    assert!(!profile.0.join("plugins").exists());
    assert!(!profile.0.join(".aw-install.lock").exists());
    assert_eq!(
        fs::read(profile.0.join("config.yaml")).unwrap(),
        b"unknown: keep\n"
    );
}
