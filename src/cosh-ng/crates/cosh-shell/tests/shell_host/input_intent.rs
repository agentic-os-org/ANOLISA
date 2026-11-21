use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::process::Command as Process;
use std::time::{Duration, Instant};

const SHELL_INTENT_HELPERS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/shell_host/input_intent.sh"
));

fn shell_intent_helpers() -> String {
    let high_bytes = (128_u16..=255)
        .map(|byte| format!("    $'\\x{byte:02X}') _COSH_BYTE_AT_RESULT='{byte}'; return 0 ;;"))
        .collect::<Vec<_>>()
        .join("\n");
    SHELL_INTENT_HELPERS.replace("__COSH_HIGH_BYTE_CASES__", &high_bytes)
}

fn shell_available(shell: &str) -> bool {
    Process::new(shell).arg("--version").output().is_ok()
}

fn classify(shell: &str, input: impl AsRef<OsStr>, top_token: &str) -> Option<String> {
    classify_with_context(shell, input, top_token, ":", None)
}

fn classify_with_setup(
    shell: &str,
    input: impl AsRef<OsStr>,
    top_token: &str,
    setup: &str,
) -> Option<String> {
    classify_with_context(shell, input, top_token, setup, None)
}

fn classify_with_context(
    shell: &str,
    input: impl AsRef<OsStr>,
    top_token: &str,
    setup: &str,
    locale: Option<&str>,
) -> Option<String> {
    classify_with_context_in(shell, input, top_token, setup, locale, None)
}

fn classify_in(shell: &str, input: &str, top_token: &str, dir: &std::path::Path) -> Option<String> {
    classify_with_context_in(shell, input, top_token, ":", None, Some(dir))
}

fn classify_with_context_in(
    shell: &str,
    input: impl AsRef<OsStr>,
    top_token: &str,
    setup: &str,
    locale: Option<&str>,
    dir: Option<&std::path::Path>,
) -> Option<String> {
    let mut command = Process::new(shell);
    if shell == "bash" {
        command.args(["--noprofile", "--norc"]);
    } else {
        command.arg("-f");
    }
    if let Some(locale) = locale {
        command.env("LANG", locale).env("LC_ALL", locale);
    }
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let script = format!(
        "{}\n{setup}\n_cosh_classify_missing \"$1\" \"$2\"",
        shell_intent_helpers(),
    );
    let output = command
        .args(["-c", &script, "cosh-intent-test"])
        .arg(input)
        .arg(top_token)
        .output()
        .ok()?;
    assert!(
        output.status.success(),
        "{shell}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn literal_first_word_matches(shell: &str, input: &str, attempt: &str, command: &str) -> bool {
    let mut process = Process::new(shell);
    if shell == "bash" {
        process.args(["--noprofile", "--norc"]);
    } else {
        process.arg("-f");
    }
    let script = format!(
        "{}\n_cosh_literal_first_word_matches \"$1\" \"$2\" \"$3\"",
        shell_intent_helpers(),
    );
    process
        .args(["-c", &script, "cosh-literal-test", input, attempt, command])
        .status()
        .is_ok_and(|status| status.success())
}

fn arguments_have_no_unquoted_expansion(shell: &str, input: &str, dir: &std::path::Path) -> bool {
    arguments_have_no_unquoted_expansion_in(shell, input, dir, None)
}

fn arguments_have_no_unquoted_expansion_in(
    shell: &str,
    input: &str,
    dir: &std::path::Path,
    locale: Option<&str>,
) -> bool {
    let mut process = Process::new(shell);
    if shell == "bash" {
        process.args(["--noprofile", "--norc"]);
    } else {
        process.arg("-f");
    }
    if let Some(locale) = locale {
        process.env("LANG", locale).env("LC_ALL", locale);
    }
    let script = format!(
        "{}\n_cosh_arguments_have_no_unquoted_expansion \"$1\"",
        shell_intent_helpers(),
    );
    process
        .args(["-c", &script, "cosh-expansion-test", input])
        .current_dir(dir)
        .status()
        .is_ok_and(|status| status.success())
}

/// `C.UTF-8` is where `?` matches one character instead of one byte, which is
/// what an interactive shell on a UTF-8 terminal does. It is not present
/// everywhere (macOS ships `UTF-8` instead), so the locale-sensitive cases skip
/// instead of failing on those hosts.
fn utf8_locale() -> Option<&'static str> {
    let probe = Process::new("bash")
        .args(["--noprofile", "--norc", "-c", "locale charmap"])
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .output()
        .ok()?;
    (String::from_utf8_lossy(&probe.stdout).trim() == "UTF-8").then_some("C.UTF-8")
}

#[test]
fn routing_c2_matcher_table_covers_supported_quote_removal_subset() {
    let accepted = [
        (
            "子曰\"三人行必有我师\"",
            "子曰\"三人行必有我师\"",
            "子曰三人行必有我师",
        ),
        ("子曰\" 三人行必有我师\"", "子曰\"", "子曰 三人行必有我师"),
        ("'子曰 三人行'后续", "'子曰", "子曰 三人行后续"),
        ("\"子 曰\"x", "\"子", "子 曰x"),
        ("子曰'\"'后续", "子曰'\"'后续", "子曰\"后续"),
        ("\"\"子曰", "\"\"子曰", "子曰"),
        ("子曰?", "子曰?", "子曰?"),
        ("   解释", "解释", "解释"),
    ];
    let rejected = [
        ("\"\"", "\"\"", ""),
        ("'子曰", "'子曰", "子曰"),
        (r"子曰\ x", r"子曰\", "子曰 x"),
        (r#"子曰"$HOME""#, r#"子曰"$HOME""#, "子曰/tmp"),
        ("子曰$(id)", "子曰$(id)", "子曰uid"),
        ("子曰*", "子曰*", "子曰a"),
    ];
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        for (input, attempt, command) in accepted {
            assert!(
                literal_first_word_matches(shell, input, attempt, command),
                "{shell}: accepted {input:?}"
            );
        }
        for (input, attempt, command) in rejected {
            assert!(
                !literal_first_word_matches(shell, input, attempt, command),
                "{shell}: rejected {input:?}"
            );
        }
    }
}

#[test]
fn unquoted_expansion_guard_matrix() {
    let dir = std::env::temp_dir().join(format!("cosh-guard-{}", super::unique_suffix()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let clean = [
        // First word is a command; glob carriers after it are ordinary text.
        "ls *.log",
        "ls ~",
        // Han sentences whose carrier trails the first word.
        "查看 ~ 目录下的文件",
        "列出所有 *.log 文件",
        "有没有名字带 [abc] 的文件",
        "看看 {a,b} 哪个大",
        "what is 2+2?",
        // English questions whose `?` sits inside the sentence, bare or
        // wrapped in quotes, carry no expansion.
        "What is 6 multiplied by 7? Reply with only the number.",
        "\"What is 6 multiplied by 7? Reply with only the number.\"",
        // A question mark closing a Han word is punctuation, not a glob.
        "你好你是谁?",
        "你还好吗? 我想问问",
        "这个是干什么的?",
        // Mid-sentence `?` before more Han text and a doubled `??` are the
        // same question punctuation, not command-position globs (#10267).
        "你还好吗?我想问问",
        "什么是mutex??",
        "你还好吗?我想问问?",
        // Quoting suppresses expansion, so a quoted carrier never reaches the
        // classifier as a rewritten word.
        "解释 \"文?.log\" 好吗",
        "这个 \"文?\" 是什么意思",
        "解释一下 \"why?\" 这个词",
        "\"你好吗?\" 我想问",
        "他问 '你好吗?'",
        // The `$` carrier is refused by the classifier, not by this guard.
        "打印 $HOME 环境变量",
    ];
    let bails = [
        // Carriers in the command position still reach the shell.
        "*.log",
        "~/run.sh",
        "  *.log",
        "\"列\"*.log",
        "列*?",
        "中文?abc",
        // Documented boundary: an ASCII word cannot be told apart from a glob
        // and `_cosh_literal_first_word_matches` rejects the same carriers.
        "why?",
        // A leading control byte must not pass as non-ASCII punctuation: the
        // word before the `?` is still ASCII, so these stay shell-owned.
        "\twhy?",
        "\t?",
    ];
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        for input in clean {
            assert!(
                arguments_have_no_unquoted_expansion(shell, input, &dir),
                "{shell}: expected clean guard for {input:?}"
            );
        }
        for input in bails {
            assert!(
                !arguments_have_no_unquoted_expansion(shell, input, &dir),
                "{shell}: expected bailing guard for {input:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn matched_tail_glob_keeps_the_line_native() {
    let dir = std::env::temp_dir().join(format!("cosh-guard-match-{}", super::unique_suffix()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("match.log"), b"x").expect("write fixture");
    std::fs::write(dir.join("文件.log"), b"x").expect("write fixture");
    std::fs::write(dir.join("a"), b"x").expect("write fixture");
    std::fs::write(dir.join("a\\b.log"), b"x").expect("write fixture");
    std::fs::write(dir.join("a\"b.log"), b"x").expect("write fixture");
    // The leading backslash is part of the name on Unix, not a separator, so
    // the absolute-path lint misreads this fixture.
    #[allow(clippy::join_absolute_paths)]
    std::fs::write(dir.join("\\x.log"), b"x").expect("write fixture");
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.join("missing.log"), dir.join("dang.log"))
        .expect("write dangling symlink");
    let locale = utf8_locale();
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        // The shell expands the tail before the classifier reads the line, so
        // the literal line would not be judged: stay native.
        for input in ["说明 *.log", "列出所有 *.log 文件"] {
            assert!(
                !arguments_have_no_unquoted_expansion(shell, input, &dir),
                "{shell}: expected native routing for {input:?} once the glob matches"
            );
        }
        // A carrier that matches nothing keeps the literal line intact.
        assert!(
            arguments_have_no_unquoted_expansion(shell, "列出所有 *.none", &dir),
            "{shell}: unmatched glob should keep the line literal"
        );
        // A brace pair alone is punctuation of the sentence even when one
        // literal names a file: there is no carrier, so the word never leaves
        // the classifier.
        assert!(
            arguments_have_no_unquoted_expansion(shell, "看看 {a,b} 哪个大", &dir),
            "{shell}: braces alone should stay punctuation with `a` present"
        );
        // The shell brace-expands before it globs, so `{a,b}*.log` is really
        // `a*.log b*.log` and the literal line would already be rewritten.
        assert!(
            !arguments_have_no_unquoted_expansion(shell, "说明 {a,b}*.log", &dir),
            "{shell}: a brace pair next to an unquoted glob must stay native"
        );
        // The bail runs before any file probe, so a brace pair whose glob
        // matches nothing still stays native: conservative, not a match.
        assert!(
            !arguments_have_no_unquoted_expansion(shell, "说明 {a,b}*.none", &dir),
            "{shell}: a brace pair keeps the line native even without a match"
        );
        // Quoted braces never brace-expand, and the quoted `?` is literal, so
        // neither of these matches the fixtures.
        for input in ["解释 \"{a,b}\"*.log 是什么意思", "说明 \"文?\"*.log"] {
            assert!(
                arguments_have_no_unquoted_expansion(shell, input, &dir),
                "{shell}: quoting must keep the probe as narrow as the shell for {input:?}"
            );
        }
        #[cfg(unix)]
        assert!(
            !arguments_have_no_unquoted_expansion(shell, "说明 dang.*", &dir),
            "{shell}: a dangling symlink is still a glob match"
        );
        // Inside double quotes `\"` is an escaped literal quote, so the shell
        // removes the quote and matches `a"b.log`; the probe must do the same.
        assert!(
            !arguments_have_no_unquoted_expansion(shell, "说明 \"a\\\"b\"*.log", &dir),
            "{shell}: an escaped quote inside double quotes is literal content"
        );
        // Outside quotes `\\` is a literal backslash and the `*` that follows
        // still globs, so the word names `\x.log`; the probe must escape only
        // the backslash.
        assert!(
            !arguments_have_no_unquoted_expansion(shell, "说明 \\\\*.log", &dir),
            "{shell}: an escaped backslash must not escape the probe's glob"
        );
        // Quoting suppresses expansion, so these stay literal and reach the
        // classifier.
        for input in ["解释 \"文?.log\" 好吗", "解释一下 \"why?\" 这个词"] {
            assert!(
                arguments_have_no_unquoted_expansion(shell, input, &dir),
                "{shell}: quoted carriers should stay literal for {input:?}"
            );
        }
        // The shell removes the quotes before expanding, so the unquoted `*`
        // here names `match.log` and the line stays native.
        assert!(
            !arguments_have_no_unquoted_expansion(shell, "说明 \"match\"*.log", &dir),
            "{shell}: a quoted prefix must not hide the unquoted glob"
        );
        // A quoted backslash is content, not an escape: both hosts expand
        // `'a\'*.log` and `"a\\"*.log` to `a\b.log`, so the line stays native.
        for input in ["说明 'a\\'*.log", "说明 \"a\\\\\"*.log"] {
            assert!(
                !arguments_have_no_unquoted_expansion(shell, input, &dir),
                "{shell}: a quoted backslash must not escape the probe's glob for {input:?}"
            );
        }
        let Some(locale) = locale else {
            continue;
        };
        // `?` is one character in the interactive shell, so this names
        // `文件.log` there; a byte-wise probe would report no match.
        assert!(
            !arguments_have_no_unquoted_expansion_in(shell, "说明 文?.log", &dir, Some(locale)),
            "{shell}: expected native routing for `说明 文?.log` under {locale}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn glob_tail_han_sentences_reach_the_classifier() {
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        for (input, top_token, expected) in [
            ("列出所有 *.log 文件", "列出所有", "natural_language"),
            ("查看 ~ 目录下的文件", "查看", "natural_language"),
            (
                "有没有名字带 [abc] 的文件",
                "有没有名字带",
                "natural_language",
            ),
            // A real command with a glob still stays with the shell.
            ("ls *.log", "ls", "command"),
            ("ls ~", "ls", "command"),
        ] {
            assert_eq!(
                classify(shell, input, top_token).as_deref(),
                Some(expected),
                "{shell}: {input:?}"
            );
        }
    }
}

#[test]
fn classification_is_locale_independent() {
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        for locale in ["C", "C.UTF-8"] {
            for (input, top_token, expected) in [
                ("Just do it", "Just", "natural_language"),
                (
                    "\u{5e2e}\u{6211}\u{770b}",
                    "\u{5e2e}\u{6211}\u{770b}",
                    "natural_language",
                ),
                (
                    "\u{3042}\u{308a}\u{304c}\u{3068}\u{3046}",
                    "\u{3042}\u{308a}\u{304c}\u{3068}\u{3046}",
                    "ambiguous",
                ),
            ] {
                assert_eq!(
                    classify_with_context(shell, input, top_token, ":", Some(locale)).as_deref(),
                    Some(expected),
                    "{shell}: {locale}: {input:?}"
                );
            }
        }
    }
}

#[test]
fn classification_ignores_user_ifs() {
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        assert_eq!(
            classify_with_setup(shell, "Just do it", "Just", "IFS=:").as_deref(),
            Some("natural_language"),
            "{shell}"
        );
    }
}

#[test]
fn long_ascii_input_avoids_per_byte_subprocess_cost() {
    let input = "a".repeat(4096);
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        let started = Instant::now();
        assert_eq!(
            classify(shell, &input, "a").as_deref(),
            Some("ambiguous"),
            "{shell}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{shell}: {:?}",
            started.elapsed()
        );
    }
}

#[test]
fn max_non_ascii_input_needs_no_byte_subprocess() {
    let input = "é".repeat(2048);
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        assert_eq!(
            classify_with_setup(shell, &input, &input, "PATH=/nonexistent").as_deref(),
            Some("ambiguous"),
            "{shell}"
        );
    }
}

#[test]
fn input_limit_is_measured_in_bytes() {
    let within_limit = format!("{}a", "\u{5e2e}".repeat(1365));
    let over_limit = "\u{5e2e}".repeat(1366);

    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        for locale in ["C", "C.UTF-8"] {
            assert_eq!(within_limit.len(), 4096);
            assert_eq!(
                classify_with_context(shell, &within_limit, &within_limit, ":", Some(locale))
                    .as_deref(),
                Some("natural_language"),
                "{shell}: {locale}: 4096 bytes"
            );
            assert_eq!(over_limit.len(), 4098);
            assert_eq!(
                classify_with_context(shell, &over_limit, &over_limit, ":", Some(locale))
                    .as_deref(),
                Some("unsafe"),
                "{shell}: {locale}: 4098 bytes"
            );
        }
    }
}

fn assert_bash_zsh(input: &str, top_token: &str, expected: &str) {
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        assert_eq!(
            classify(shell, input, top_token).as_deref(),
            Some(expected),
            "{shell}: {input:?}"
        );
    }
}

#[test]
fn strong_natural_language_matrix_is_consistent() {
    for (input, top_token) in [
        ("Who are you", "Who"),
        ("how file", "how"),
        ("please explain this", "please"),
        ("can you check this", "can"),
        ("is this safe", "is"),
        ("review this file", "review"),
        ("I need help", "I"),
        (
            "\u{5e2e}\u{6211}\u{770b}\u{770b}\u{5f53}\u{524d}\u{76ee}\u{5f55}",
            "\u{5e2e}\u{6211}\u{770b}\u{770b}\u{5f53}\u{524d}\u{76ee}\u{5f55}",
        ),
        (
            "\u{5e2e}\u{6211}\u{770b} ./\u{65e5}\u{5fd7}",
            "\u{5e2e}\u{6211}\u{770b}",
        ),
        (
            "\u{5e2e}\u{6211}\u{770b} ./*.log",
            "\u{5e2e}\u{6211}\u{770b}",
        ),
        ("review ./report.log", "review"),
    ] {
        assert_bash_zsh(input, top_token, "natural_language");
    }
}

#[test]
fn composed_natural_language_matrix_is_consistent() {
    for (input, top_token) in [
        ("Just do it", "Just"),
        ("please just check this", "please"),
        ("maybe explain this", "maybe"),
        ("simply tell me why", "simply"),
        ("go ahead", "go"),
        ("try again", "try"),
        ("keep going", "keep"),
        ("carry on", "carry"),
        ("never mind", "never"),
        ("forget it", "forget"),
        ("thank you", "thank"),
        ("good morning", "good"),
        ("do this", "do"),
        ("run the tests", "run"),
        ("find the problem", "find"),
        ("open this file", "open"),
        ("read the logs", "read"),
        ("create a report", "create"),
        ("update the config", "update"),
        ("restart the service", "restart"),
        ("I am stuck", "I"),
        ("I think this failed", "I"),
        ("we have a problem", "we"),
        ("you should retry", "you"),
        ("this is broken", "this"),
        ("that looks wrong", "that"),
        ("it failed again", "it"),
        ("there is an error", "there"),
        ("need some help", "need"),
        ("want more details", "want"),
        ("any ideas", "any"),
        ("nginx is down", "nginx"),
        ("the build failed", "the"),
        ("my command does not work", "my"),
        ("service not running", "service"),
        ("everything looks fine", "everything"),
        ("let us check", "let"),
        ("hello there", "hello"),
        ("yes please", "yes"),
        ("thanks again", "thanks"),
        ("not working", "not"),
        ("why?", "why"),
    ] {
        assert_bash_zsh(input, top_token, "natural_language");
    }
}

#[test]
fn command_veto_matrix_is_consistent() {
    for (input, top_token) in [
        (
            "./\u{4e2d}\u{6587}\u{811a}\u{672c}",
            "./\u{4e2d}\u{6587}\u{811a}\u{672c}",
        ),
        ("review --all", "review"),
        ("review FOO=bar", "review"),
        ("review this | cat", "review"),
        ("command review this", "command"),
        ("Who are you??", "Who"),
    ] {
        assert_bash_zsh(input, top_token, "command");
    }
    assert_bash_zsh("帮我看 \"$PATH\"", "帮我看", "natural_language");
}

#[test]
fn english_question_punctuation_reaches_the_classifier() {
    let dir = std::env::temp_dir().join(format!("cosh-question-{}", super::unique_suffix()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        for (input, top_token) in [
            // A `?` that closes a word inside the sentence is question
            // punctuation, not a glob for the classifier.
            (
                "What is 6 multiplied by 7? Reply with only the number.",
                "What",
            ),
            // Without a matching file the same shape is still a question.
            ("What is 7? Reply with only the number.", "What"),
            // Quoting the whole sentence must not change how it is judged.
            (
                "\"What is 6 multiplied by 7? Reply with only the number.\"",
                "What is 6 multiplied by 7? Reply with only the number.",
            ),
            ("\"Can you help\"", "Can you help"),
            // The sentence-final `?` and Han sentences keep their routing.
            ("What is that number multiplied by 2?", "What"),
            (
                "\u{8bf7}\u{95ee} 6 \u{4e58}\u{4ee5} 7 \u{7b49}\u{4e8e}\u{591a}\u{5c11}\u{ff1f}\u{53ea}\u{56de}\u{7b54}\u{6570}\u{5b57}",
                "\u{8bf7}\u{95ee}",
            ),
        ] {
            assert_eq!(
                classify_in(shell, input, top_token, &dir).as_deref(),
                Some("natural_language"),
                "{shell}: {input:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn question_punctuation_keeps_command_syntax_shell_owned() {
    let dir = std::env::temp_dir().join(format!("cosh-question-veto-{}", super::unique_suffix()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    // `7?` names this file, so the shell rewrites an unquoted `?` word before
    // the classifier reads the line: the bare form must stay native.
    std::fs::write(dir.join("70"), b"x").expect("write fixture");
    // Paired with the `"What is this"?` case below, this file is what the
    // shell's glob would rewrite that line to.
    std::fs::write(dir.join("What is this!"), b"x").expect("write fixture");
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        for (input, top_token) in [
            (
                "What is 6 multiplied by 7? Reply with only the number.",
                "What",
            ),
            ("What is 7? Reply with only the number.", "What"),
            // A `?` after the closing quote is unquoted, so the shell globs
            // it; stripping it must not turn the line into a quoted sentence
            // and let it escape the veto. The same holds when the glob really
            // matches a file here, and for the full-width `？`.
            ("\"Can you help\"?", "Can you help?"),
            ("\"What is 7?\"?", "What is 7?"),
            ("\"What is this\"?", "What is this?"),
            ("\"What is 7?\"\u{ff1f}", "What is 7?\u{ff1f}"),
            // Real command syntax stays with the shell.
            ("What is 6 | cat?", "What"),
            ("What is 6 > out?", "What"),
            ("What is $(id)?", "What"),
            ("What is `id`?", "What"),
            ("What is 6? | cat", "What"),
            // Quotes that do not wrap the whole line stay shell-owned too.
            ("\"What is 6?", "What is 6?"),
            ("What is \"6\" times 7?", "What"),
            ("What is 6? \"really\"", "What"),
            ("\"\"", "\"\""),
            // Two adjacent `?` read as a glob, not as punctuation.
            ("Who are you??", "Who"),
            ("\"Who are you??\"", "Who are you??"),
        ] {
            assert_eq!(
                classify_in(shell, input, top_token, &dir).as_deref(),
                Some("command"),
                "{shell}: {input:?}"
            );
        }
        for (input, top_token) in [
            // A `?` inside the quotes is never glob-expanded, so a matching
            // cwd file cannot rewrite the line: quoted questions stay
            // questions regardless of cwd.
            (
                "\"What is 6 multiplied by 7? Reply with only the number.\"",
                "What is 6 multiplied by 7? Reply with only the number.",
            ),
            ("\"What is 7?\"", "What is 7?"),
        ] {
            assert_eq!(
                classify_in(shell, input, top_token, &dir).as_deref(),
                Some("natural_language"),
                "{shell}: {input:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn question_mark_probe_keeps_the_caller_locale() {
    let Some(utf8) = utf8_locale() else {
        return;
    };
    // `?` is one character in a UTF-8 locale, so `7?` names the fixture and the
    // interactive shell rewrites the line; byte-wise under `C` it names
    // nothing. The veto's probe must predict both the way the caller's shell
    // does, or the classifier routes a line the shell already expanded.
    let dir = std::env::temp_dir().join(format!("cosh-question-locale-{}", super::unique_suffix()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("7中"), b"x").expect("write fixture");
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        for (locale, expected) in [(utf8, "command"), ("C", "natural_language")] {
            assert_eq!(
                classify_with_context_in(
                    shell,
                    "What is 7? Reply with only the number.",
                    "What",
                    ":",
                    Some(locale),
                    Some(&dir),
                )
                .as_deref(),
                Some(expected),
                "{shell}: the veto probe must glob `7?` the way {locale} does"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ambiguous_tool_inputs_remain_shell_owned() {
    for (input, top_token) in [
        ("ok", "ok"),
        ("hello", "hello"),
        ("thanks", "thanks"),
        ("continue", "continue"),
        ("just", "just"),
        ("just build", "just"),
        ("just cargo test", "just"),
        ("just terraform plan", "just"),
        ("maybe kubectl get pods", "maybe"),
        ("simply deploy", "simply"),
        ("go test", "go"),
        ("try build", "try"),
        ("nginx status", "nginx"),
        ("the build", "the"),
        ("terraform plan", "terraform"),
        ("kubectl get pods", "kubectl"),
        ("make deploy", "make"),
        (
            "\u{3042}\u{308a}\u{304c}\u{3068}\u{3046}",
            "\u{3042}\u{308a}\u{304c}\u{3068}\u{3046}",
        ),
        ("\u{1f642}", "\u{1f642}"),
    ] {
        assert_bash_zsh(input, top_token, "ambiguous");
    }
}

#[test]
fn invalid_utf8_is_unsafe() {
    let input = OsString::from_vec(vec![b'r', b'e', b'v', b'i', b'e', b'w', b' ', 0xff]);
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        assert_eq!(
            classify(shell, &input, "review").as_deref(),
            Some("unsafe"),
            "{shell}"
        );
    }
}

#[test]
fn han_core_boundaries_are_locale_independent() {
    for codepoint in [
        0x3400, 0x4dbf, 0x4e00, 0x9fff, 0xf900, 0xfaff, 0x20000, 0x323af,
    ] {
        let input = char::from_u32(codepoint).expect("valid Han codepoint");
        assert_bash_zsh(&input.to_string(), &input.to_string(), "natural_language");
    }
    for codepoint in [0x33ff, 0x4dc0, 0x10000, 0x323b0] {
        let input = char::from_u32(codepoint).expect("valid non-Han codepoint");
        assert_bash_zsh(&input.to_string(), &input.to_string(), "ambiguous");
    }
}

#[test]
fn routing_c1_classifier_han_tier_matrix() {
    for (input, top_token, expected) in [
        (
            "使用 git log --since=\"1 day ago\" --format=\"%h %s (%an, %ar)\" 总结",
            "使用",
            "natural_language",
        ),
        (
            "解释 --all FOO=bar ./*.log {a,b} ~/x",
            "解释",
            "natural_language",
        ),
        ("解释一下 (quoted) 的含义", "解释一下", "command"),
        (
            "解释一下 \"(quoted)\" 的含义",
            "解释一下",
            "natural_language",
        ),
        ("你还好吗？ 我想问问", "你还好吗？", "natural_language"),
        ("你还好吗?我想问问", "你还好吗?我想问问", "natural_language"),
        ("什么是mutex??", "什么是mutex??", "natural_language"),
        ("解释 ps aux | grep java", "解释", "command"),
        ("解释 true && touch x", "解释", "command"),
        ("解释 false || touch x", "解释", "command"),
        ("解释 \"$HOME\"", "解释", "natural_language"),
        ("解释 'a>b'", "解释", "natural_language"),
        ("解释 $HOME", "解释", "natural_language"),
        ("解释 \"${evil@P}\"", "解释", "command"),
        ("解释 \"${(e)evil}\"", "解释", "command"),
        ("解释 foo; bar", "解释", "command"),
        ("解释 input < file", "解释", "command"),
        ("解释 output > file", "解释", "command"),
        ("解释 $((1 + 1))", "解释", "command"),
        ("解释 <(printf x)", "解释", "command"),
        ("解释 `printf x`", "解释", "command"),
        ("解释 \\", "解释", "command"),
        ("解释 \"unterminated", "解释", "command"),
        ("解释\t内容", "解释", "command"),
    ] {
        assert_bash_zsh(input, top_token, expected);
    }
}

#[test]
fn routing_c1_classifier_validates_full_utf8_before_han() {
    let mut bytes = "解释".as_bytes().to_vec();
    bytes.push(0xff);
    let input = OsString::from_vec(bytes);
    for shell in ["bash", "zsh"] {
        if !shell_available(shell) {
            continue;
        }
        assert_eq!(
            classify(shell, &input, "解释").as_deref(),
            Some("unsafe"),
            "{shell}"
        );
    }
}
