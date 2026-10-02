// Owner: shell_host. Bash/zsh marker scripts live in per-shell owner files;
// Bash uses a thin Rust loader plus marker/bash.sh so shell syntax does not
// grow the Rust module. This hub keeps the single shell_host::marker module
// path so marker and attempt-generation changes remain atomic.
mod bash;
mod zsh;

pub(super) use bash::bash_marker_script;
pub(super) use zsh::zsh_marker_script;

#[cfg(test)]
mod tests {
    use super::{bash_marker_script, zsh_marker_script};
    use crate::types::{BOUNDED_HANDOFF_COMMAND, NON_INTERACTIVE_PAGER_PREFIX};

    const BYPASS_ASSIGNMENT: &str = "_COSH_HANDOFF_PREFIX='COSH_SHELL_HANDOFF_BYPASS=1 '\n";

    #[test]
    fn bash_marker_matches_the_bounded_handoff_transport_command() {
        let assignment = format!("_COSH_BOUNDED_HANDOFF_COMMAND='{BOUNDED_HANDOFF_COMMAND}'\n");

        assert!(
            bash_marker_script().contains(&assignment),
            "the Bash marker and Rust transport must match byte-for-byte"
        );
    }

    /// The Rust transport defines the prefixes and the marker scripts strip them
    /// back off. Drift in either direction leaks transport environment into
    /// history, OSC markers and evidence, so pin all three definitions here.
    #[test]
    fn marker_scripts_strip_the_same_transport_prefixes_rust_emits() {
        let pager_assignment =
            format!("_COSH_HANDOFF_PAGER_PREFIX='{NON_INTERACTIVE_PAGER_PREFIX}'\n");

        for (shell, script) in [("bash", bash_marker_script()), ("zsh", zsh_marker_script())] {
            assert!(script.contains(BYPASS_ASSIGNMENT), "{shell}");
            assert!(script.contains(&pager_assignment), "{shell}");
            assert!(
                script.contains("local command=\"${1#$_COSH_HANDOFF_PREFIX}\"")
                    && script.contains("printf '%s' \"${command#$_COSH_HANDOFF_PAGER_PREFIX}\""),
                "{shell} must strip both transport prefixes in transport order"
            );
        }
    }

    /// `handoff_pty_bytes` always emits the bypass prefix first, so a bare pager
    /// prefix is never a transport line — it is a user command that happens to
    /// start with those assignments, and stripping it would rewrite the user's
    /// own history and evidence.
    #[test]
    fn marker_scripts_treat_only_the_bypass_prefix_as_a_handoff_wrapper() {
        for (shell, script) in [("bash", bash_marker_script()), ("zsh", zsh_marker_script())] {
            assert!(
                script.contains("    \"$_COSH_HANDOFF_PREFIX\"*)\n"),
                "{shell} must match the bypass prefix"
            );
            assert!(
                !script.contains("\"$_COSH_HANDOFF_PAGER_PREFIX\"*)"),
                "{shell} must not treat a bare pager prefix as a handoff wrapper"
            );
        }
    }

    /// The out-of-band policy path exports the variables itself, so the export
    /// statement, the save/restore set and the Rust constant must agree.
    #[test]
    fn marker_scripts_neutralize_exactly_the_rust_pager_variable_set() {
        let variables = NON_INTERACTIVE_PAGER_PREFIX
            .split_whitespace()
            .map(|assignment| {
                let (name, value) = assignment
                    .split_once('=')
                    .unwrap_or_else(|| panic!("assignment form: {assignment}"));
                assert_eq!(value, "cat", "{name}");
                name
            })
            .collect::<Vec<_>>();
        assert_eq!(
            variables,
            ["PAGER", "GIT_PAGER", "MANPAGER", "SYSTEMD_PAGER"]
        );

        let loop_header = format!("  for name in {}; do\n", variables.join(" "));
        for (shell, script, injection_guard, readonly_demotion) in [
            (
                "bash",
                bash_marker_script(),
                "  if [[ \"${!name-}\" != cat\n     || \"$(_cosh_pager_var_state \"$name\")\" != export ]]; then\n",
                "      readonly_export)\n        export -n \"$name\"\n",
            ),
            (
                "zsh",
                zsh_marker_script(),
                "  if [[ \"${(P)name}\" != cat\n     || \"$(_cosh_pager_var_state \"$name\")\" != export ]]; then\n",
                "      readonly_export)\n        typeset +x \"$name\"\n",
            ),
        ] {
            assert!(
                script.contains("      export \"$name=cat\"\n"),
                "{shell} must neutralize each variable in the shared list"
            );
            assert!(
                script.matches(loop_header.as_str()).count() == 2,
                "{shell} must walk the same variable list when applying and restoring"
            );
            assert!(
                script.contains("${COSH_HANDOFF_REQUEST_FILE}.no-pager"),
                "{shell} must read the out-of-band policy sidecar"
            );
            assert!(
                script.contains("_cosh_apply_handoff_pager_policy\n")
                    && script.contains("_cosh_restore_handoff_pager_policy\n"),
                "{shell} must apply and restore the policy around one command"
            );
            // Value equality alone is not enough: `export -n PAGER` leaves the
            // value at `cat` while dropping the attribute, and reverting that
            // would discard what the handoff command asked for.
            assert!(
                script.contains(injection_guard),
                "{shell} must require the full injected declaration state before reverting"
            );
            assert!(
                script.contains(readonly_demotion)
                    && script.contains("      readonly_shell)\n")
                    && script.contains("      *)\n        export \"$name=cat\"\n"),
                "{shell} must hide exported readonly values without assigning to them"
            );
            for state in [
                "unset",
                "shell",
                "export",
                "readonly_export",
                "readonly_shell",
            ] {
                assert!(
                    script.contains(&format!("printf {state}\n"))
                        || script.contains(&format!("    {state})\n")),
                    "{shell} must handle the {state} declaration state"
                );
            }
            assert!(
                script.contains("_COSH_${name}_STATE") && script.contains("_COSH_${name}_SAVED"),
                "{shell} must record the prior state and value of each variable"
            );
        }
    }

    #[test]
    fn non_interactive_pager_prefix_only_covers_documented_pager_variables() {
        assert_eq!(
            NON_INTERACTIVE_PAGER_PREFIX,
            "PAGER=cat GIT_PAGER=cat MANPAGER=cat SYSTEMD_PAGER=cat "
        );
        assert!(NON_INTERACTIVE_PAGER_PREFIX.ends_with(' '));
    }
}
