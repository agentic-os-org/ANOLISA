// Lib-only regressions for the readonly-pipeline option-operand guards:
// `library_tests!` expands to nothing in the bin target (src/main.rs), so
// these tests never enter the exact lib/bin overlap count, mirroring
// `command_risk_tests.rs`. Keeping them in a dedicated `_tests.rs` module
// also leaves `readonly_pipeline.rs` under the 700-line large-file gate.
library_tests! {
    use super::readonly_pipeline::validate_readonly_pipeline;

    // Options that take a path or program operand inline (`--output=PATH`,
    // `-oPATH`) or in the next token (`-f FILE`) are invisible to the
    // positional file-operand check: sort would write the pipeline output
    // to an arbitrary path, execute `--compress-program`, spill temp files
    // into `--temporary-directory`, or read operand files via
    // `--files0-from`/`--random-source`; grep/rg would read a pattern
    // file via `-f`. GNU accepts unambiguous abbreviations, so `--out=`
    // and `--o=` are spellings of `--output=` and must be rejected too.
    // Verified against GNU coreutils 9.1: every `--output`/`--out`/`--o`/
    // `-oPATH`/`-noPATH` form below writes the named file.
    #[test]
    fn readonly_pipeline_rejects_filter_stage_path_option_operands() {
        for command in [
            "ps aux | sort --output=/tmp/cosh-pwned",
            "ps aux | sort --out=/tmp/cosh-pwned",
            "ps aux | sort --o=/tmp/cosh-pwned",
            "ps aux | sort -o/tmp/cosh-pwned",
            "ps aux | sort -o=/tmp/cosh-pwned",
            "ps aux | sort -no/tmp/cosh-pwned",
            "ps aux | sort -o -n",
            "ps aux | sort -o",
            "ps aux | sort -do /tmp/cosh-pwned",
            "ps aux | sort --compress-program=/usr/bin/id",
            "ps aux | sort --compress-p=/usr/bin/id",
            "ps aux | sort --temporary-directory=/tmp",
            "ps aux | sort --t=/tmp",
            "ps aux | sort --files0-from=-",
            "ps aux | sort --random-source=/dev/zero",
            "ps aux | wc --files0-from=-",
            "ps aux | uniq -o",
        ] {
            assert!(
                validate_readonly_pipeline(command).is_err(),
                "{command} should be rejected"
            );
        }
    }

    // grep/rg read operand files through more pattern-file spellings than
    // `-f`: grep's `--exclude-from=FILE` and rg's `--ignore-file=FILE`
    // take a path inline or in the next token, and GNU grep resolves
    // unambiguous abbreviations, so `--exclude-f`/`--exclude-fr` reach
    // the same read. The other spellings below are denied as a
    // conservative fail-safe boundary, not as live read vectors: GNU
    // grep has no `--include-from` option (it rejects `--include-from=`
    // and `--include-f=` at option parsing), and ripgrep's clap parser
    // does not resolve long-option abbreviations (`--ignore-f` is an
    // unrecognized flag there), so neither spelling ever reaches a file
    // read. The denied boundary is still the shortest prefix that
    // separates the file options from the harmless same-prefix GLOB
    // options (`--exclude`/`--include` take a pattern;
    // `--ignore-case`/`--ignore-vcs` never take a file).
    #[test]
    fn readonly_pipeline_rejects_search_stage_pattern_file_spellings() {
        for command in [
            "ps aux | grep --exclude-from=/etc/passwd root",
            "ps aux | grep --exclude-from /etc/passwd root",
            "ps aux | grep --exclude-f=/etc/passwd root",
            "ps aux | grep --exclude-fr /etc/passwd root",
            "ps aux | grep --include-from=/etc/passwd root",
            "ps aux | grep --include-f=/etc/passwd root",
            "ps aux | rg --ignore-file=/etc/passwd root",
            "ps aux | rg --ignore-file /etc/passwd root",
            "ps aux | rg --ignore-f=/etc/passwd root",
        ] {
            assert!(
                validate_readonly_pipeline(command).is_err(),
                "{command} should be rejected"
            );
        }
    }

    // The reject list must stay narrow: ordinary filter flags, value-taking
    // options whose values are not paths (`-k`, `-S`, `-t`, `-d`), and
    // long options that merely start like a denied name of ANOTHER
    // program (`cut`'s `--output-delimiter=`, its `--f=1` spelling of
    // `--fields=1`; grep's `--exclude=`/`--include=` GLOB forms) keep the
    // auto-approve route — the deny tables are per program, and the
    // denied search boundaries start past the harmless same-prefix
    // options.
    #[test]
    fn readonly_pipeline_keeps_ordinary_filter_flags() {
        for command in [
            "ps aux | sort",
            "ps aux | sort -n",
            "ps aux | sort -k2,2 -u",
            "ps aux | sort -t: -k2",
            "ps aux | sort -S4M",
            "ps aux | sort --sort=general-numeric",
            "ps aux | sort -R",
            "ps aux | cut -d= -f1",
            "ps aux | cut -do -f1",
            "ps aux | cut --output-delimiter=, -f1",
            "ps aux | cut --f=1",
            "ps aux | cut --fields=2",
            "ps aux | grep --exclude=systemd root",
            "ps aux | grep --include=sshd root",
            "ps aux | grep --files-with-matches root",
            "ps aux | grep --files-without-match root",
            "ps aux | head -5",
            "ps aux | head -c 4096",
            "ps aux | wc -l",
            "ps aux | uniq -c",
        ] {
            assert!(
                validate_readonly_pipeline(command).is_ok(),
                "{command} should stay allowed"
            );
        }
    }

    // `-f`/`--file` (pattern file) bypass the single-positional allowance:
    // `grep -f /etc/passwd` counts `/etc/passwd` as the pattern operand
    // while grep actually reads it as a pattern source, and the attached
    // form `-fFILE` (accepted by GNU grep) never even reaches the
    // positional count. rg's `--pre` preprocessor and `--hostname-bin`
    // execute a program; the readonly-rules broker path already denies
    // `rg --pre=cat`, and the pipeline gate must not be weaker.
    #[test]
    fn readonly_pipeline_rejects_search_stage_operand_options() {
        for command in [
            "ps aux | grep -f /etc/passwd",
            "ps aux | grep -f/etc/passwd",
            "ps aux | grep -nf /etc/passwd",
            "ps aux | grep -qf /etc/passwd",
            "ps aux | grep -e root -f/etc/passwd",
            "ps aux | rg -f /etc/passwd",
            "ps aux | rg --file /etc/passwd",
            "ps aux | rg --pre=/tmp/preprocess.sh root",
            "ps aux | rg --hostname-bin=/tmp/resolve.sh root",
        ] {
            assert!(
                validate_readonly_pipeline(command).is_err(),
                "{command} should be rejected"
            );
        }
    }

    #[test]
    fn readonly_pipeline_keeps_ordinary_search_flags() {
        for command in [
            "ps aux | grep root",
            "ps aux | grep -i root",
            "ps aux | grep -F root",
            "ps aux | grep -A2 root",
            "ps aux | grep -m5 root",
            "ps aux | grep -dskip root",
            "ps aux | grep --color=auto root",
            "ps aux | rg root",
            "ps aux | rg -i root",
            "ps aux | rg --pretty root",
            "ps aux | rg --heading root",
        ] {
            assert!(
                validate_readonly_pipeline(command).is_ok(),
                "{command} should stay allowed"
            );
        }
    }
}
