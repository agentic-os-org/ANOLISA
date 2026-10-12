//! Core-only initialization, explicit policy selection and clean template transitions.

use super::*;
use crate::{Policy, Settings};
use serde_json::{json, Value};

fn settings<'a>(fixture: &'a Fixture, config: &'a Path) -> Settings<'a> {
    Settings {
        prefix: &fixture.prefix,
        config,
        state: &fixture.state,
        qoder: Some(Path::new("/bin/true")),
        node: None,
        openclaw: None,
        policy: Policy::None,
    }
}

fn read(config: &Path) -> Value {
    let bytes = fs::read(config).unwrap();
    aw_config::Validator::new()
        .unwrap()
        .parse(&bytes)
        .unwrap()
        .as_value()
        .clone()
}

#[test]
fn core_alone_initializes_each_agent_without_provider_or_second_agent_inputs() {
    for qoder in [false, true] {
        let f = Fixture::new();
        install(&f.core, &f.prefix).unwrap();
        let config = f.root.join("aw.yaml");
        let mut settings = settings(&f, &config);
        if !qoder {
            settings.qoder = None;
            settings.node = Some(Path::new("/bin/true"));
            settings.openclaw = Some(Path::new("/bin/true"));
        }
        crate::configure(&settings).unwrap();
        let value = read(&config);
        assert_eq!(value["spec"]["agents"].as_object().unwrap().len(), 1);
        assert_eq!(value["spec"]["providers"], json!({}));
        assert_eq!(value["spec"]["events"], json!({}));
        assert!(!f.prefix.join("libexec").exists());
        assert_eq!(config.metadata().unwrap().mode() & 0o777, 0o600);
    }
}

#[test]
fn explicit_enable_then_remove_generates_complete_documents_without_dangling_steps() {
    let f = Fixture::new();
    install(&f.core, &f.prefix).unwrap();
    let config = f.root.join("aw.yaml");
    let mut settings = settings(&f, &config);
    settings.policy = Policy::SecCore {
        socket: Path::new("/run/aw-test/sec.sock"),
    };
    assert!(crate::configure(&settings)
        .unwrap_err()
        .to_string()
        .contains("install the sec-core Provider"));
    assert!(!config.exists());
    install(&f.provider, &f.prefix).unwrap();
    crate::configure(&settings).unwrap();
    let enabled = read(&config);
    assert_eq!(
        enabled["spec"]["providers"]["security"]["config"]["tools"],
        json!({"Bash":{"language":"bash","input_pointer":"/command"}})
    );
    assert_eq!(
        enabled["spec"]["events"]["tool.before"]["steps"][0]["on_error"],
        "block"
    );
    // This task owns complete template generation; existing-document editing is separate.
    fs::remove_file(&config).unwrap();
    settings.policy = Policy::None;
    crate::configure(&settings).unwrap();
    let removed = read(&config);
    assert_eq!(removed["spec"]["providers"], json!({}));
    assert_eq!(removed["spec"]["events"], json!({}));
    assert_eq!(receipts(&f.prefix).unwrap().len(), 2);
}

#[test]
fn unused_provider_damage_does_not_become_a_core_configuration_dependency() {
    let f = Fixture::new();
    install(&f.combined, &f.prefix).unwrap();
    let config = f.root.join("aw.yaml");
    let mut settings = settings(&f, &config);
    fs::remove_file(
        f.prefix
            .join("libexec/aw/providers/sec-core/aw-provider-sec-core"),
    )
    .unwrap();
    crate::configure(&settings).unwrap();
    fs::remove_file(&config).unwrap();
    settings.policy = Policy::SecCore {
        socket: Path::new("/run/aw-test/sec.sock"),
    };
    assert!(crate::configure(&settings).is_err());
    assert!(!config.exists());
    // Even an invalid optional receipt must not become a core prerequisite.
    fs::write(
        f.prefix.join(".aw-packages/aw-provider-sec-core.json"),
        b"invalid receipt",
    )
    .unwrap();
    settings.policy = Policy::None;
    crate::configure(&settings).unwrap();
    fs::remove_file(&config).unwrap();
    settings.policy = Policy::SecCore {
        socket: Path::new("/run/aw-test/sec.sock"),
    };
    assert!(crate::configure(&settings).is_err());
    assert!(!config.exists());
}

#[test]
fn command_template_needs_only_core_and_keeps_effects_out_of_user_argv() {
    let f = Fixture::new();
    install(&f.core, &f.prefix).unwrap();
    let config = f.root.join("aw.yaml");
    let mut settings = settings(&f, &config);
    let argv = vec!["/bin/true".into(), "literal ; $(touch NEVER)".into()];
    settings.policy = Policy::Command {
        argv: &argv,
        effect: "block",
        reason_code: "parameter_matches",
    };
    crate::configure(&settings).unwrap();
    let value = read(&config);
    assert_eq!(
        value["spec"]["providers"]["command"]["transport"]["argv"],
        json!([f.prefix.join("bin/aw"), "policy"])
    );
    assert_eq!(
        value["spec"]["providers"]["command"]["config"]["argv"],
        json!(argv)
    );
    assert_eq!(
        value["spec"]["providers"]["command"]["config"]["on_true"],
        json!({"type":"block","reason_code":"parameter_matches"})
    );
    assert_eq!(
        value["spec"]["events"]["tool.before"]["steps"][0]["effects"],
        json!(["block"])
    );
    assert!(!f.prefix.join("libexec").exists());
    assert!(!f.root.join("NEVER").exists());
}

#[test]
fn invalid_agent_or_command_inputs_preserve_existing_configuration() {
    let f = Fixture::new();
    install(&f.core, &f.prefix).unwrap();
    let config = f.root.join("aw.yaml");
    let existing = b"user-owned configuration";
    fs::write(&config, existing).unwrap();
    for invalid in [
        "missing-agent",
        "partial-openclaw",
        "invalid-effect",
        "missing-command",
    ] {
        let argv = vec![if invalid == "missing-command" {
            "/missing-program"
        } else {
            "/bin/true"
        }
        .into()];
        let mut settings = settings(&f, &config);
        match invalid {
            "missing-agent" => settings.qoder = None,
            "partial-openclaw" => settings.node = Some(Path::new("/bin/true")),
            _ => {
                settings.policy = Policy::Command {
                    argv: &argv,
                    effect: if invalid == "invalid-effect" {
                        "ask"
                    } else {
                        "block"
                    },
                    reason_code: "check",
                }
            }
        }
        assert!(crate::configure(&settings).is_err());
        assert_eq!(fs::read(&config).unwrap(), existing);
    }
}
