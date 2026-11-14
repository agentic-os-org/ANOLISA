use super::*;

#[test]
fn root_help_is_reserved_only_for_the_exact_installed_invocation() {
    let help = [OsString::from("--help")];
    for argv0 in ["cosh", "-cosh", "/usr/bin/cosh"] {
        for stdin in [false, true] {
            for stdout in [false, true] {
                for stderr in [false, true] {
                    assert_eq!(
                        classify_invocation(OsStr::new(argv0), &help, stdin, stdout, stderr),
                        Invocation::Help
                    );
                }
            }
        }
    }
    for args in [
        vec!["--shell", "bash", "--help"],
        vec!["--help", "extra"],
        vec!["-c", "printf help", "--help"],
    ] {
        let args: Vec<_> = args.into_iter().map(OsString::from).collect();
        assert!(matches!(
            classify_invocation(OsStr::new("cosh"), &args, true, true, true),
            Invocation::ExecShell(_)
        ));
    }
    assert!(matches!(
        classify_invocation(OsStr::new("cosh-shell"), &help, true, true, true),
        Invocation::ExecShell(_)
    ));
}
