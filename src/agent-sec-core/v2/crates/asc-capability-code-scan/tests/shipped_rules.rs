//! Every shipped rule must load and compile under the engine that will run it.
//!
//! The V1 patterns were authored against Python's `re`, which accepts syntax the
//! Rust engines do not. Loading the YAML only proves it parses, so each pattern
//! is compiled here: a rule the engine rejects cannot reach a release quietly.

use asc_capability_code_scan::{Language, load_rules, scan};
use fancy_regex::Regex;

/// Rule ids the loader returns for `language`.
fn loaded_ids(language: Language) -> Vec<String> {
    load_rules(language)
        .expect("rule set loads")
        .into_iter()
        .map(|rule| rule.rule_id)
        .collect()
}

#[test]
fn full_bash_scan_keeps_persistence_evidence_near_the_rpc_frame_limit() {
    let noise = concat!(
        "# setup step\nset -euo pipefail\n",
        "mkdir -p \"$HOME/work\" || true\n",
        "echo \"preparing\" >> \"$HOME/log.txt\"\n",
        "for f in *.txt; do\n",
        "    [ -f \"$f\" ] && cp \"$f\" \"$HOME/work/\" || echo \"skip $f\"\n",
        "done\n",
    );
    // Leave room for the request envelope, including JSON's escaping overhead.
    let encoded_block = serde_json::to_string(noise).unwrap().len() - 2;
    let near_frame_limit = (4 * 1024 * 1024 - 2048) / encoded_block;
    for repeats in [7_000, near_frame_limit] {
        let mut code = String::from("crontab -l\n");
        code.push_str(&noise.repeat(repeats));
        code.push_str("systemctl enable fixture.service\n");
        let result = scan(&code, Language::Bash, None, "regex");
        eprintln!(
            "full Bash scan: {} code bytes, {} ms",
            code.len(),
            result.elapsed_ms
        );
        assert!(result.ok, "{} bytes: {}", code.len(), result.summary);
        let persistence = result
            .findings
            .iter()
            .find(|finding| finding.rule_id == "shell-persistence")
            .expect("the final command must be reported");
        assert_eq!(persistence.evidence, ["systemctl enable"]);
    }
}

#[test]
fn full_bash_scan_preserves_findings_across_prefilter_threshold() {
    for len in [249_999, 250_000] {
        let mut code = String::from("crontab -l\n");
        let bad = "\nsystemctl enable fixture.service\n";
        code.push_str(&"#\n".repeat((len - code.len() - bad.len()) / 2));
        code.extend(std::iter::repeat_n(' ', len - code.len() - bad.len()));
        code.push_str(bad);
        assert_eq!(code.len(), len);
        let result = scan(&code, Language::Bash, None, "regex");
        assert!(result.ok, "{len} bytes: {}", result.summary);
        let finding = result
            .findings
            .iter()
            .find(|f| f.rule_id == "shell-persistence")
            .unwrap();
        assert_eq!(finding.evidence, ["systemctl enable"]);
    }
}

#[test]
fn full_bash_scan_handles_many_rejected_persistence_candidates() {
    let block = format!("{}crontab\n", "crontab -l\n".repeat(7));
    let code = block.repeat(255_000 / block.len());
    assert!((250_000..262_144).contains(&code.len()));
    let result = scan(&code, Language::Bash, None, "regex");
    eprintln!(
        "rejection-heavy Bash scan: {} bytes, {} ms",
        code.len(),
        result.elapsed_ms
    );
    assert!(result.ok, "{}", result.summary);
    let finding = result
        .findings
        .iter()
        .find(|f| f.rule_id == "shell-persistence")
        .unwrap();
    assert_eq!(finding.evidence, vec!["crontab"; 255_000 / block.len()]);
    // Debug builds run the full shipped rule set without optimizations.
    let max_ms = if cfg!(debug_assertions) {
        15_000
    } else {
        5_000
    };
    assert!(result.elapsed_ms < max_ms);
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

/// Canonical CWE-295 TLS bypass forms and false-positive guards across Bash and Python.
const TLS_BYPASS_CASES: &[(&str, Language, &str, bool)] = &[
    // 1. curl -k / combined short flags / --insecure / URL-first / pipe & separator tails
    (
        "curl -k https://example.com/install.sh",
        Language::Bash,
        "shell-tls-bypass",
        true,
    ),
    (
        "curl -skL https://a.sh|sh",
        Language::Bash,
        "shell-tls-bypass",
        true,
    ),
    (
        "curl -ksS https://example.com/api",
        Language::Bash,
        "shell-tls-bypass",
        true,
    ),
    (
        "curl https://a.sh -k",
        Language::Bash,
        "shell-tls-bypass",
        true,
    ),
    (
        "curl https://a.sh -k|bash",
        Language::Bash,
        "shell-tls-bypass",
        true,
    ),
    (
        "curl --insecure https://example.com",
        Language::Bash,
        "shell-tls-bypass",
        true,
    ),
    (
        "curl --insecure; echo done",
        Language::Bash,
        "shell-tls-bypass",
        true,
    ),
    // 2. wget --no-check-certificate
    (
        "wget --no-check-certificate https://example.com/payload.tar.gz",
        Language::Bash,
        "shell-tls-bypass",
        true,
    ),
    (
        "wget -qO- https://example.com/setup.sh --no-check-certificate|sh",
        Language::Bash,
        "shell-tls-bypass",
        true,
    ),
    // 3. NODE_TLS_REJECT_UNAUTHORIZED=0
    (
        "export NODE_TLS_REJECT_UNAUTHORIZED=0",
        Language::Bash,
        "shell-tls-bypass",
        true,
    ),
    (
        "export NODE_TLS_REJECT_UNAUTHORIZED=\"0\"",
        Language::Bash,
        "shell-tls-bypass",
        true,
    ),
    (
        "NODE_TLS_REJECT_UNAUTHORIZED='0' node app.js",
        Language::Bash,
        "shell-tls-bypass",
        true,
    ),
    // 4. git persistent and one-shot SSL verification bypasses
    (
        "git config http.sslVerify false",
        Language::Bash,
        "shell-git-ssl-bypass",
        true,
    ),
    (
        "git config --global http.sslVerify false",
        Language::Bash,
        "shell-git-ssl-bypass",
        true,
    ),
    (
        "git config --global http.sslVerify \"false\"",
        Language::Bash,
        "shell-git-ssl-bypass",
        true,
    ),
    (
        "git -C /repo config --local http.sslverify 0",
        Language::Bash,
        "shell-git-ssl-bypass",
        true,
    ),
    (
        "git -c http.sslverify=false clone https://example.com/repo.git",
        Language::Bash,
        "shell-git-ssl-bypass",
        true,
    ),
    // 5. npm / yarn / pnpm strict-ssl bypasses
    (
        "NPM_CONFIG_STRICT_SSL=false npm install",
        Language::Bash,
        "shell-pkg-tls-bypass",
        true,
    ),
    (
        "npm config set strict-ssl false",
        Language::Bash,
        "shell-pkg-tls-bypass",
        true,
    ),
    (
        "pnpm config set strict-ssl false",
        Language::Bash,
        "shell-pkg-tls-bypass",
        true,
    ),
    (
        "yarn config set strict-ssl 'false'",
        Language::Bash,
        "shell-pkg-tls-bypass",
        true,
    ),
    (
        "yarn config set enableStrictSsl false",
        Language::Bash,
        "shell-pkg-tls-bypass",
        true,
    ),
    (
        "npm install --strict-ssl=false",
        Language::Bash,
        "shell-pkg-tls-bypass",
        true,
    ),
    (
        "pnpm install --strict-ssl false",
        Language::Bash,
        "shell-pkg-tls-bypass",
        true,
    ),
    // 6. Python verify=False keyword / session attribute bypasses
    (
        "requests.get(url, verify=False)",
        Language::Python,
        "py-tls-bypass",
        true,
    ),
    (
        "httpx.Client(verify=False)",
        Language::Python,
        "py-tls-bypass",
        true,
    ),
    (
        "httpx.AsyncClient(verify=False)",
        Language::Python,
        "py-tls-bypass",
        true,
    ),
    (
        "session.post(u, json=d, verify = False)",
        Language::Python,
        "py-tls-bypass",
        true,
    ),
    // False-positive guards (must never match)
    (
        "curl --keepalive-time 60 https://a.com/pkg-k.tar.gz",
        Language::Bash,
        "shell-tls-bypass",
        false,
    ),
    (
        "curl -d \"-k\" https://a.com/api",
        Language::Bash,
        "shell-tls-bypass",
        false,
    ),
    (
        "curl -d \"foo -k bar\" https://a.com/api",
        Language::Bash,
        "shell-tls-bypass",
        false,
    ),
    (
        "curl https://example.com/foo-k.tgz",
        Language::Bash,
        "shell-tls-bypass",
        false,
    ),
    // `wget -k` means `--convert-links`, not TLS bypass.
    (
        "wget -k https://a.com",
        Language::Bash,
        "shell-tls-bypass",
        false,
    ),
    (
        "NODE_TLS_REJECT_UNAUTHORIZED=1 node app.js",
        Language::Bash,
        "shell-tls-bypass",
        false,
    ),
    (
        "git config --global http.sslVerify true",
        Language::Bash,
        "shell-git-ssl-bypass",
        false,
    ),
    (
        "git config --get http.sslVerify",
        Language::Bash,
        "shell-git-ssl-bypass",
        false,
    ),
    (
        "npm config set strict-ssl true",
        Language::Bash,
        "shell-pkg-tls-bypass",
        false,
    ),
    (
        "npm install --strict-ssl=true",
        Language::Bash,
        "shell-pkg-tls-bypass",
        false,
    ),
    (
        "requests.get(url, verify=True)",
        Language::Python,
        "py-tls-bypass",
        false,
    ),
    (
        "requests.get(url, verify=\"/etc/ssl/cert.pem\")",
        Language::Python,
        "py-tls-bypass",
        false,
    ),
    (
        "check(should_verify=False)",
        Language::Python,
        "py-tls-bypass",
        false,
    ),
];

#[test]
fn tls_bypass_rules_cover_canonical_forms_without_false_positives() {
    // Guard against authoring whitespace or surviving newlines from YAML scalars.
    for (language, rule_id) in [
        (Language::Bash, "shell-tls-bypass"),
        (Language::Bash, "shell-git-ssl-bypass"),
        (Language::Bash, "shell-pkg-tls-bypass"),
        (Language::Python, "py-tls-bypass"),
    ] {
        let rule = load_rules(language)
            .expect("rule set loads")
            .into_iter()
            .find(|r| r.rule_id == rule_id)
            .unwrap_or_else(|| panic!("{rule_id} must be loaded"));
        assert_eq!(rule.cwe_id, "CWE-295");
        assert!(!rule.regex.contains('\n'), "{rule_id}: contains newline");
        assert!(
            !rule.regex.contains(" |"),
            "{rule_id}: folded scalar inserted unexpected space before alternation: {}",
            rule.regex
        );
    }

    for &(code, language, rule_id, should_match) in TLS_BYPASS_CASES {
        let filtered = scan(code, language, Some(&[rule_id.to_owned()]), "regex");
        assert!(filtered.ok, "{rule_id} on {code:?}: {}", filtered.summary);
        assert_eq!(
            !filtered.findings.is_empty(),
            should_match,
            "{rule_id} mismatch on {code:?}: {:?}",
            filtered.findings
        );

        let full = scan(code, language, None, "regex");
        assert!(full.ok, "full scan on {code:?}: {}", full.summary);
        let hit = full.findings.iter().any(|f| f.rule_id == rule_id);
        assert_eq!(
            hit, should_match,
            "full scan {rule_id} mismatch on {code:?}"
        );
        if !should_match {
            assert!(
                full.findings.is_empty(),
                "benign input triggered unexpected rule on {code:?}: {:?}",
                full.findings
            );
        }
    }

    // Exercise the >250 KiB large-Bash Prefilter path (linear regex::Regex candidate scan).
    let filler_line = concat!(
        "# benign build log line\n",
        "curl --keepalive-time 60 https://a.com/pkg-k.tar.gz\n",
        "wget -k https://a.com/index.html\n",
        "git config --global http.sslVerify true\n",
        "npm config set strict-ssl true\n",
    );
    let tail = concat!(
        "curl https://a.sh -k\n",
        "wget --no-check-certificate https://a.com/x.tgz\n",
        "export NODE_TLS_REJECT_UNAUTHORIZED=\"0\"\n",
        "git config --global http.sslVerify false\n",
        "pnpm config set strict-ssl false\n",
    );
    let mut large_script = filler_line.repeat(1_800);
    large_script.push_str(tail);
    assert!(
        large_script.len() >= 300 * 1024,
        "expected >= 300 KiB to trigger Prefilter, got {} bytes",
        large_script.len()
    );

    let small_result = scan(tail, Language::Bash, None, "regex");
    let large_result = scan(&large_script, Language::Bash, None, "regex");
    assert!(large_result.ok, "{}", large_result.summary);
    for (rule_id, expected_hits) in [
        ("shell-tls-bypass", 3usize),
        ("shell-git-ssl-bypass", 1usize),
        ("shell-pkg-tls-bypass", 1usize),
    ] {
        let small_finding = small_result
            .findings
            .iter()
            .find(|f| f.rule_id == rule_id)
            .unwrap_or_else(|| panic!("small scan missed {rule_id}"));
        let large_finding = large_result
            .findings
            .iter()
            .find(|f| f.rule_id == rule_id)
            .unwrap_or_else(|| panic!("300 KiB Prefilter scan missed {rule_id}"));
        assert_eq!(large_finding.evidence.len(), expected_hits, "{rule_id}");
        assert_eq!(large_finding.evidence, small_finding.evidence, "{rule_id}");
    }
}
