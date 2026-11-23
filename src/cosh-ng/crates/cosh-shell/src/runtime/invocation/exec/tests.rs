use super::*;
use std::os::unix::ffi::OsStringExt;

fn plan(origin: ExecOrigin) -> ExecPlan {
    ExecPlan {
        origin,
        shell_override: None,
        arg0: OsString::from("cosh"),
        args: vec![
            "-c".into(),
            "printf '%s' \"$1\"".into(),
            OsString::from_vec(vec![0xff]),
        ],
        isolated: false,
    }
}

#[test]
fn managed_default_exec_uses_the_recorded_program_and_preserves_original_arguments() {
    let plan = plan(ExecOrigin::Primary);
    let before = plan.clone();
    let shell = choose_shell(
        &plan,
        || OsString::from("shadowed-bash"),
        || Ok(Some("/usr/bin/bash".into())),
    )
    .unwrap();
    assert_eq!(shell, "/usr/bin/bash");
    assert_eq!(plan, before);
}

#[test]
fn managed_default_exec_never_probes_raw_or_explicit_shell_overrides() {
    let raw = plan(ExecOrigin::Raw);
    assert_eq!(
        choose_shell(
            &raw,
            || "raw-default".into(),
            || panic!("raw must stay transparent")
        )
        .unwrap(),
        "raw-default"
    );
    let mut explicit = plan(ExecOrigin::Primary);
    explicit.shell_override = Some(OsString::from_vec(vec![b'/', 0xff]));
    assert_eq!(
        choose_shell(
            &explicit,
            || panic!("explicit shell owns selection"),
            || panic!("explicit shell must not query account")
        )
        .unwrap(),
        explicit.shell_override.unwrap()
    );
}

#[test]
fn managed_default_exec_keeps_non_cosh_defaults_and_does_not_fall_back_to_path_on_query_failure() {
    let plan = plan(ExecOrigin::Primary);
    assert_eq!(
        choose_shell(&plan, || "legacy-default".into(), || Ok(None)).unwrap(),
        "legacy-default"
    );
    assert!(choose_shell(
        &plan,
        || "shadowed-bash".into(),
        || Err("bounded account query failed".into())
    )
    .is_err());
}
