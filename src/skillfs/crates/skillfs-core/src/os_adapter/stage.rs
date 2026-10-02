//! The compiled OS-adapter transform stage and its `TransformStage` impl.
//!
//! The stage is built once at mount startup (rules parsed, validated, and
//! specialized for the resolved target). Its `apply` runs only in-memory string
//! substitution — no YAML parsing, OS detection, or I/O on the per-read path.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::transform::TransformStage;

use super::detect::{OsTarget, TargetSelector, resolve_target};
use super::error::OsAdapterError;
use super::rules;

/// The built-in Ubuntu/Alinux rule catalog, embedded from the repository asset
/// so the default adapter works in source builds, RPMs, and containers without
/// depending on a separate on-disk file.
const BUILTIN_RULES: &[u8] = include_bytes!("../../assets/ubuntu-alinux.yaml");

/// The compiled OS-adapter stage: an ordered list of substitutions specialized
/// for one resolved [`OsTarget`].
#[derive(Debug)]
pub struct OsAdapterStage {
    target: OsTarget,
    /// Ordered substitutions; applied by a single-pass, longest-match scan per
    /// read (see the `TransformStage::apply` impl).
    rules: Vec<rules::CompiledRule>,
    /// Source literals matched-and-preserved during the scan (ineligible rules:
    /// never / identity / direction-disallowed), so a shorter eligible rule
    /// cannot rewrite inside a span an ineligible rule claims.
    protects: Vec<rules::CompiledProtection>,
    /// Content-free digest of the rule artifact bytes, for diagnostics.
    digest: String,
    /// Number of rules parsed from the artifact (before direction filtering).
    total_rules: usize,
}

impl OsAdapterStage {
    /// Load, validate, and compile the rule artifact at `rules_path` for the
    /// given `selector`.
    ///
    /// # Errors
    ///
    /// Returns [`OsAdapterError`] when the file is missing/unreadable, the YAML
    /// is malformed, a rule has an invalid `direction`/`auto_apply`/`match`
    /// value or empty pattern, the resolved target has duplicate/ambiguous
    /// source patterns, or `target_os = auto` cannot map `/etc/os-release` to a
    /// supported target.
    pub fn load(rules_path: &Path, selector: TargetSelector) -> Result<Self, OsAdapterError> {
        let bytes = std::fs::read(rules_path).map_err(|source| OsAdapterError::ReadRules {
            path: rules_path.to_path_buf(),
            source,
        })?;
        let target = resolve_target(selector)?;
        Self::from_bytes(&bytes, target, rules_path)
    }

    /// Load, validate, and compile the built-in Ubuntu/Alinux catalog embedded
    /// in the binary, for the given `selector`.
    ///
    /// Used when the OS adapter is enabled without an explicit `rules_path`. The
    /// catalog is compiled from the embedded artifact exactly once at mount
    /// startup; the per-read path is unaffected.
    ///
    /// # Errors
    ///
    /// Returns [`OsAdapterError`] when `target_os = auto` cannot map
    /// `/etc/os-release` to a supported target, or (should the embedded artifact
    /// ever regress) when it fails to compile. Never panics.
    pub fn load_default(selector: TargetSelector) -> Result<Self, OsAdapterError> {
        let target = resolve_target(selector)?;
        Self::from_bytes(
            BUILTIN_RULES,
            target,
            Path::new("<built-in ubuntu-alinux.yaml>"),
        )
    }

    /// Compile a stage from raw artifact `bytes` for an already-resolved
    /// `target`. Split out from [`Self::load`] so target detection and file I/O
    /// stay testable independently.
    pub(crate) fn from_bytes(
        bytes: &[u8],
        target: OsTarget,
        path: &Path,
    ) -> Result<Self, OsAdapterError> {
        let compiled = rules::compile(bytes, target, path)?;
        Ok(Self {
            target,
            rules: compiled.rules,
            protects: compiled.protects,
            digest: digest_hex(bytes),
            total_rules: compiled.total_rules,
        })
    }

    /// The resolved target OS this stage converts content toward.
    pub fn target(&self) -> OsTarget {
        self.target
    }

    /// Stable, content-free SHA-256 digest (hex) of the rule artifact bytes.
    ///
    /// Suitable for initialization diagnostics and, being algorithm-stable
    /// across Rust versions and hosts, as a future cache-identity key.
    pub fn rule_digest(&self) -> &str {
        &self.digest
    }

    /// Number of rules parsed from the artifact (before direction filtering).
    pub fn total_rules(&self) -> usize {
        self.total_rules
    }

    /// Number of substitutions active for the resolved target.
    pub fn active_rules(&self) -> usize {
        self.rules.len()
    }
}

impl TransformStage for OsAdapterStage {
    fn name(&self) -> &'static str {
        "os_adapter"
    }

    fn apply(&self, input: &str) -> String {
        // Single left-to-right pass over the *original* input. At each position
        // pick the longest matching source pattern (most specific wins), emit
        // its replacement, and skip past the matched span so neither the
        // replacement text nor already-scanned input is rewritten again.
        //
        // This keeps every rule a 1:1 map to its declared target and prevents
        // cascading rewrites through overlapping patterns — e.g. `apache2`
        // never rewrites the inside of `apache2-utils`, and `cron` never
        // re-hits the `crond` produced by a more specific rule. A naive
        // sequential `replace` per rule would corrupt both. Distinct sources of
        // equal length cannot both prefix the same position, so the longest
        // match is unambiguous; compile-time dedup already rejects equal
        // sources.
        //
        // A token-mode candidate enters longest-match selection only after its
        // ASCII-alphanumeric source edges satisfy their boundaries. Literal
        // candidates retain the historical substring behavior.
        //
        // Ineligible patterns (never / identity / direction-disallowed) also
        // compete in the scan as *protection* matches: when one is the longest
        // match it is emitted verbatim and skipped, so a shorter eligible rule
        // cannot rewrite inside a span an ineligible rule claims (e.g. the
        // `never` path `/etc/init.d/apache2` is not corrupted by `apache2`).
        let mut out = String::with_capacity(input.len());
        let mut i = 0;
        while i < input.len() {
            let rest = &input[i..];
            // Longest matching source at this position, and its replacement:
            // `Some(to)` substitutes, `None` protects (emit the source as-is).
            let mut best_from: Option<&str> = None;
            let mut best_to: Option<&str> = None;
            for rule in &self.rules {
                if rule.match_mode.matches(input, i, &rule.source)
                    && best_from.is_none_or(|best| rule.source.len() > best.len())
                {
                    best_from = Some(&rule.source);
                    best_to = Some(&rule.target);
                }
            }
            for protection in &self.protects {
                if protection.match_mode.matches(input, i, &protection.source)
                    && best_from.is_none_or(|best| protection.source.len() > best.len())
                {
                    best_from = Some(&protection.source);
                    best_to = None;
                }
            }
            match best_from {
                Some(from) => {
                    out.push_str(best_to.unwrap_or(from));
                    i += from.len();
                }
                None => {
                    // No pattern matches here: copy one whole UTF-8 scalar so
                    // `i` stays on a char boundary for the next iteration. The
                    // loop guard `i < input.len()` makes `rest` non-empty, so
                    // `chars().next()` is always `Some`.
                    let Some(ch) = rest.chars().next() else {
                        unreachable!("rest is non-empty while i < input.len()")
                    };
                    let len = ch.len_utf8();
                    out.push_str(&rest[..len]);
                    i += len;
                }
            }
        }
        out
    }
}

/// Stable, content-free SHA-256 digest (hex) of the rule artifact bytes.
///
/// Unlike the standard-library hasher, SHA-256 is algorithm-stable across Rust
/// versions and hosts, so the digest can identify a specific rule artifact for
/// diagnostics and future persistent-cache keys. It never carries Skill content.
fn digest_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut hex = String::with_capacity(out.len() * 2);
    for byte in out {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn stage_from(yaml: &str, target: OsTarget) -> Result<OsAdapterStage, OsAdapterError> {
        OsAdapterStage::from_bytes(yaml.as_bytes(), target, Path::new("test-rules.yaml"))
    }

    const SAMPLE: &str = r#"
- ubuntu: "sudo apt-get install -y "
  alinux: "sudo dnf install -y "
  direction: bidirectional
  auto_apply: always
  confidence: high
- ubuntu: "libssl-dev"
  alinux: "openssl-devel"
  direction: bidirectional
  auto_apply: always
- ubuntu: "apache2.service"
  alinux: "httpd.service"
  direction: bidirectional
  auto_apply: always
- ubuntu: "/etc/apt/sources.list.d/"
  alinux: "/etc/yum.repos.d/"
  direction: bidirectional
  auto_apply: always
- ubuntu: "build-essential"
  alinux: '@"Development Tools"'
  direction: ubuntu_to_alinux_only
  auto_apply: always
- ubuntu: "ufw"
  alinux: "firewalld"
  direction: ubuntu_to_alinux_only
  auto_apply: never
"#;

    #[test]
    fn ubuntu_to_alinux_converts_representative_rules() {
        let stage = stage_from(SAMPLE, OsTarget::Alinux).unwrap();
        let input = "Run `sudo apt-get install -y libssl-dev` then enable apache2.service.\n\
                     Config lives in /etc/apt/sources.list.d/. Install build-essential.\n";
        let out = stage.apply(input);
        assert!(out.contains("sudo dnf install -y "));
        assert!(out.contains("openssl-devel"));
        assert!(out.contains("httpd.service"));
        assert!(out.contains("/etc/yum.repos.d/"));
        assert!(out.contains("@\"Development Tools\""));
        assert!(!out.contains("apt-get install"));
        assert!(!out.contains("libssl-dev"));
    }

    #[test]
    fn alinux_to_ubuntu_reverses_bidirectional_only() {
        let stage = stage_from(SAMPLE, OsTarget::Ubuntu).unwrap();
        let input =
            "sudo dnf install -y openssl-devel; unit httpd.service; @\"Development Tools\"\n";
        let out = stage.apply(input);
        assert!(out.contains("sudo apt-get install -y "));
        assert!(out.contains("libssl-dev"));
        assert!(out.contains("apache2.service"));
        // ubuntu_to_alinux_only rule does NOT reverse: the Alinux literal stays.
        assert!(out.contains("@\"Development Tools\""));
        assert!(!out.contains("build-essential"));
    }

    #[test]
    fn auto_apply_never_is_loaded_but_not_applied() {
        let stage = stage_from(SAMPLE, OsTarget::Alinux).unwrap();
        assert_eq!(stage.apply("enable ufw now"), "enable ufw now");
        assert_eq!(stage.total_rules(), 6);
    }

    #[test]
    fn token_match_mode_requires_ascii_alphanumeric_boundaries() {
        let yaml = "- ubuntu: cron\n  alinux: cronie\n  direction: bidirectional\n  match: token\n  auto_apply: always\n";
        let alinux = stage_from(yaml, OsTarget::Alinux).unwrap();
        for (input, expected) in [
            ("cron", "cronie"),
            ("cron ", "cronie "),
            ("cron\n", "cronie\n"),
            ("cron,", "cronie,"),
            ("(cron)", "(cronie)"),
        ] {
            assert_eq!(alinux.apply(input), expected, "input: {input:?}");
        }
        for input in ["crontab", "cronutils", "cron2", "micron"] {
            assert_eq!(alinux.apply(input), input, "input: {input:?}");
        }

        let ubuntu = stage_from(yaml, OsTarget::Ubuntu).unwrap();
        for (input, expected) in [
            ("cronie", "cron"),
            ("cronie ", "cron "),
            ("cronie\n", "cron\n"),
            ("cronie!", "cron!"),
        ] {
            assert_eq!(ubuntu.apply(input), expected, "input: {input:?}");
        }
        for input in ["cronietab", "cronieutils", "cronie2", "microcronie"] {
            assert_eq!(ubuntu.apply(input), input, "input: {input:?}");
        }
    }

    #[test]
    fn omitted_match_mode_preserves_literal_prefix_matching() {
        let yaml =
            "- ubuntu: apt\n  alinux: dnf\n  direction: bidirectional\n  auto_apply: always\n";
        let stage = stage_from(yaml, OsTarget::Alinux).unwrap();
        assert_eq!(stage.apply("aptitude"), "dnfitude");
    }

    #[test]
    fn mixed_modes_keep_protection_when_token_substitution_misses() {
        let yaml = "- ubuntu: cron\n  alinux: cronie\n  direction: bidirectional\n  match: token\n  auto_apply: always\n- ubuntu: cron\n  alinux: legacy-cron\n  direction: ubuntu_to_alinux_only\n  match: literal\n  auto_apply: never\n- ubuntu: cr\n  alinux: xx\n  direction: ubuntu_to_alinux_only\n  auto_apply: always\n";
        let stage = stage_from(yaml, OsTarget::Alinux).unwrap();

        // Both four-byte candidates match, so the eligible token substitution
        // wins the equal-length tie.
        assert_eq!(stage.apply("cron"), "cronie");
        // The token boundary fails, but the coexisting literal protection still
        // claims the full source and blocks the shorter `cr` substitution.
        assert_eq!(stage.apply("crontab"), "crontab");
        // The shorter substitution remains active outside the protected source.
        assert_eq!(stage.apply("crab"), "xxab");
    }

    // -----------------------------------------------------------------------
    // Built-in catalog (bundled default)
    // -----------------------------------------------------------------------

    #[test]
    fn builtin_catalog_has_expected_rule_and_eligibility_counts() {
        // Structural contract of the bundled asset: exactly 311 rules, every
        // rule carries an explicit auto_apply, split 257 always / 54 never.
        let value: serde_yaml::Value = serde_yaml::from_slice(BUILTIN_RULES).unwrap();
        let seq = value.as_sequence().expect("top-level YAML sequence");
        assert_eq!(seq.len(), 311, "exactly 311 rules");
        let (mut always, mut never) = (0usize, 0usize);
        for rule in seq {
            let aa = rule
                .get("auto_apply")
                .and_then(|v| v.as_str())
                .expect("every rule must declare auto_apply explicitly");
            match aa {
                "always" => always += 1,
                "never" => never += 1,
                other => panic!("unexpected auto_apply value: {other}"),
            }
        }
        assert_eq!(always, 257, "eligible rules are auto_apply: always");
        assert_eq!(never, 54, "protected rules are auto_apply: never");
    }

    #[test]
    fn builtin_catalog_loads_for_explicit_alinux() {
        let stage = OsAdapterStage::load_default(TargetSelector::Alinux).unwrap();
        assert_eq!(stage.target(), OsTarget::Alinux);
        // 311 rules parsed before direction/eligibility filtering.
        assert_eq!(stage.total_rules(), 311);
        // Non-identity active substitutions when converting toward Alinux.
        assert_eq!(stage.active_rules(), 223);
    }

    #[test]
    fn builtin_catalog_loads_for_explicit_ubuntu() {
        let stage = OsAdapterStage::load_default(TargetSelector::Ubuntu).unwrap();
        assert_eq!(stage.target(), OsTarget::Ubuntu);
        assert_eq!(stage.total_rules(), 311);
        assert_eq!(stage.active_rules(), 192);
    }

    #[test]
    fn builtin_cron_rules_enforce_token_boundaries() {
        let alinux = OsAdapterStage::load_default(TargetSelector::Alinux).unwrap();
        for (input, expected) in [
            ("cron", "cronie"),
            ("cron ", "cronie "),
            ("cron\n", "cronie\n"),
            ("cron,", "cronie,"),
        ] {
            assert_eq!(alinux.apply(input), expected, "input: {input:?}");
        }
        for input in ["crontab -e", "cronutils", "cron2", "micron"] {
            assert_eq!(alinux.apply(input), input, "input: {input:?}");
        }
        for (input, expected) in [
            ("sudo apt-get install -y cron", "sudo dnf install -y cronie"),
            ("apt-get install -y cron", "dnf install -y cronie"),
            ("sudo apt install -y cron", "sudo dnf install -y cronie"),
            ("apt install -y cron", "dnf install -y cronie"),
            ("apt-get install cron", "dnf install -y cronie"),
            ("apt install cron", "dnf install -y cronie"),
        ] {
            assert_eq!(alinux.apply(input), expected, "input: {input}");
        }
        for (input, expected) in [
            ("apt-get install -y crontab", "dnf install -y crontab"),
            ("apt install -y cronutils", "dnf install -y cronutils"),
            ("apt-get install cron2", "dnf install -y cron2"),
        ] {
            assert_eq!(alinux.apply(input), expected, "input: {input}");
        }
        assert_eq!(alinux.apply("cron.service"), "crond.service");
        assert_eq!(
            alinux.apply("systemctl restart cron"),
            "systemctl restart crond"
        );
        assert_eq!(alinux.apply("service cron"), "service crond");
        assert_eq!(
            alinux.apply("systemctl restart crontab"),
            "systemctl restart crontab"
        );
        assert_eq!(alinux.apply("service cronutils"), "service cronutils");

        let ubuntu = OsAdapterStage::load_default(TargetSelector::Ubuntu).unwrap();
        for (input, expected) in [
            ("cronie", "cron"),
            ("cronie ", "cron "),
            ("cronie\n", "cron\n"),
            ("cronie!", "cron!"),
        ] {
            assert_eq!(ubuntu.apply(input), expected, "input: {input:?}");
        }
        for input in ["cronietab", "cronieutils", "cronie2", "microcronie"] {
            assert_eq!(ubuntu.apply(input), input, "input: {input:?}");
        }
        for (input, expected) in [
            ("sudo dnf install -y cronie", "sudo apt-get install -y cron"),
            ("dnf install -y cronie", "apt-get install -y cron"),
        ] {
            assert_eq!(ubuntu.apply(input), expected, "input: {input}");
        }
        assert_eq!(
            ubuntu.apply("dnf install -y cronietab"),
            "apt-get install -y cronietab"
        );
        assert_eq!(ubuntu.apply("crond.service"), "cron.service");
        assert_eq!(
            ubuntu.apply("systemctl restart crond"),
            "systemctl restart cron"
        );
        assert_eq!(ubuntu.apply("service crond"), "service cron");
        assert_eq!(
            ubuntu.apply("systemctl restart crondtab"),
            "systemctl restart crondtab"
        );
    }

    #[test]
    fn builtin_catalog_transforms_high_confidence_rules_both_directions() {
        // A representative high-confidence rule (apt-get install) converts in
        // both directions using the bundled catalog.
        let to_alinux = OsAdapterStage::load_default(TargetSelector::Alinux).unwrap();
        let out = to_alinux.apply("run sudo apt-get install -y nginx\n");
        assert!(out.contains("sudo dnf install -y "), "{out}");
        assert!(!out.contains("apt-get install"), "{out}");

        let to_ubuntu = OsAdapterStage::load_default(TargetSelector::Ubuntu).unwrap();
        let back = to_ubuntu.apply("run sudo dnf install -y nginx\n");
        assert!(back.contains("sudo apt-get install -y "), "{back}");
    }

    #[test]
    fn builtin_apt_shorthand_is_forward_only_without_reverse_ambiguity() {
        // The reclassified `apt update` shorthand still rewrites Ubuntu ->
        // Alinux, but the reverse target uses the canonical apt-get/apt-cache
        // form, so the catalog loads for Ubuntu without a reverse conflict.
        let to_alinux = OsAdapterStage::load_default(TargetSelector::Alinux).unwrap();
        assert!(to_alinux.apply("apt update\n").contains("dnf check-update"));
        // Loading toward Ubuntu already succeeded above with 192 active rules,
        // proving no ambiguity was introduced by the shorthand rules.
        OsAdapterStage::load_default(TargetSelector::Ubuntu).unwrap();
    }

    #[test]
    fn builtin_medium_and_low_rules_are_present_but_inert() {
        // Representative low (ufw -> firewalld) and medium (apt-get dist-upgrade
        // -> dnf distro-sync) rules ship in the catalog as auto_apply: never, so
        // their target literals are never produced by the compiled stage.
        let value: serde_yaml::Value = serde_yaml::from_slice(BUILTIN_RULES).unwrap();
        let seq = value.as_sequence().unwrap();
        let has = |ubuntu: &str, aa: &str| {
            seq.iter().any(|r| {
                r.get("ubuntu").and_then(|v| v.as_str()) == Some(ubuntu)
                    && r.get("auto_apply").and_then(|v| v.as_str()) == Some(aa)
            })
        };
        assert!(
            has("ufw", "never"),
            "low-confidence ufw rule present as never"
        );
        assert!(
            has("apt-get dist-upgrade", "never"),
            "medium-confidence dist-upgrade rule present as never"
        );

        // Neither never-rule contributes a substitution: their target literals
        // never appear in the output.
        let stage = OsAdapterStage::load_default(TargetSelector::Alinux).unwrap();
        assert!(!stage.apply("please enable ufw now").contains("firewalld"));
        assert!(
            !stage
                .apply("run apt-get dist-upgrade")
                .contains("dnf distro-sync")
        );
    }

    #[test]
    fn builtin_overlapping_patterns_do_not_cascade() {
        // Regression: sequential per-rule replacement corrupted overlapping
        // patterns. Single-pass longest-match keeps each rule's declared target.
        let alinux = OsAdapterStage::load_default(TargetSelector::Alinux).unwrap();
        // `apache2` must not rewrite the inside of `apache2-utils`.
        assert_eq!(alinux.apply("apache2-utils"), "httpd-tools");
        // `cron` must not rewrite the inside of `cron.service`.
        assert_eq!(alinux.apply("cron.service"), "crond.service");
        // The command form wins over the bare `cron` verb, and `crond` in the
        // result is never re-hit by the `cron` rule.
        assert_eq!(
            alinux.apply("systemctl restart cron"),
            "systemctl restart crond"
        );
        // The specific config path wins over its `/etc/apache2/` components.
        assert_eq!(
            alinux.apply("/etc/apache2/apache2.conf"),
            "/etc/httpd/conf/httpd.conf"
        );
        // A bare, non-overlapping match still converts.
        assert_eq!(alinux.apply("apache2"), "httpd");

        // Reverse: the specific package name is not corrupted by the
        // `httpd` -> `apache2` rule firing inside it.
        let ubuntu = OsAdapterStage::load_default(TargetSelector::Ubuntu).unwrap();
        assert_eq!(ubuntu.apply("libmicrohttpd-devel"), "libmicrohttpd-dev");
    }

    #[test]
    fn builtin_catalog_full_audit_no_penetration() {
        // Catalog-wide guard enumerated from the *raw* artifact (not the
        // already-filtered `stage.rules`), so it also covers never / identity /
        // direction-disallowed rules. For each raw rule, applying the full
        // compiled stage to its source must yield:
        //   - the declared target, when the rule is an eligible substitution;
        //   - the source unchanged, otherwise (protected — no shorter eligible
        //     rule may penetrate it).
        // The old implementation dropped ineligible rules, so shorter rules
        // penetrated them (e.g. `/etc/init.d/apache2` -> `/etc/init.d/httpd`).
        use std::collections::HashMap;
        let value: serde_yaml::Value = serde_yaml::from_slice(BUILTIN_RULES).unwrap();
        let seq = value.as_sequence().unwrap();
        let field = |rule: &serde_yaml::Value, key: &str| {
            rule.get(key).and_then(|v| v.as_str()).unwrap().to_string()
        };
        for (target, selector) in [
            (OsTarget::Alinux, TargetSelector::Alinux),
            (OsTarget::Ubuntu, TargetSelector::Ubuntu),
        ] {
            let stage = OsAdapterStage::load_default(selector).unwrap();
            let sided = |rule: &serde_yaml::Value| {
                let ubuntu = field(rule, "ubuntu");
                let alinux = field(rule, "alinux");
                match target {
                    OsTarget::Alinux => (ubuntu, alinux),
                    OsTarget::Ubuntu => (alinux, ubuntu),
                }
            };
            let allows = |direction: &str| {
                direction == "bidirectional"
                    || (direction == "ubuntu_to_alinux_only" && target == OsTarget::Alinux)
                    || (direction == "alinux_to_ubuntu_only" && target == OsTarget::Ubuntu)
            };
            // Eligible substitutions first; when source and mode both match, a
            // substitution wins the equal-length protection tie (mirrors the
            // scan's precedence).
            let mut subs: HashMap<String, String> = HashMap::new();
            for rule in seq {
                let (source, dest) = sided(rule);
                if field(rule, "auto_apply") == "always"
                    && allows(&field(rule, "direction"))
                    && source != dest
                {
                    subs.insert(source, dest);
                }
            }
            // Every raw source must transform to its substitution target, or be
            // preserved verbatim when it has no eligible substitution.
            for rule in seq {
                let (source, _) = sided(rule);
                let expected = subs.get(&source).cloned().unwrap_or_else(|| source.clone());
                assert_eq!(
                    stage.apply(&source),
                    expected,
                    "target {}: source {source:?} must map to {expected:?}",
                    stage.target().as_str()
                );
            }
        }
    }

    #[test]
    fn builtin_never_and_identity_rules_are_protected() {
        // Regression: ineligible rules were dropped at compile time, so shorter
        // eligible rules penetrated them. They now protect their whole span.
        let alinux = OsAdapterStage::load_default(TargetSelector::Alinux).unwrap();
        // `never` path must not be rewritten by the shorter `apache2` rule.
        assert_eq!(alinux.apply("/etc/init.d/apache2"), "/etc/init.d/apache2");

        let ubuntu = OsAdapterStage::load_default(TargetSelector::Ubuntu).unwrap();
        // Identity `postgresql-contrib` must not be split by the shorter
        // `postgresql` -> `postgresql-client` reverse rule.
        assert_eq!(ubuntu.apply("postgresql-contrib"), "postgresql-contrib");
        // The shorter reverse rule still fires on its own bare source.
        assert_eq!(ubuntu.apply("postgresql"), "postgresql-client");
    }

    #[test]
    fn apply_does_not_rewrite_replacement_output() {
        // A minimal fixture proving non-cascading: `ab` -> `bc` must not then
        // match a `bc` -> `x` rule on the produced output.
        let yaml = "- ubuntu: ab\n  alinux: bc\n  direction: bidirectional\n  auto_apply: always\n\
                    - ubuntu: bc\n  alinux: x\n  direction: ubuntu_to_alinux_only\n  auto_apply: always\n";
        let stage = stage_from(yaml, OsTarget::Alinux).unwrap();
        // `ab` -> `bc` (not `x`); the literal `bc` in input -> `x`.
        assert_eq!(stage.apply("ab bc"), "bc x");
    }

    #[test]
    fn builtin_digest_is_stable_across_targets() {
        // Digest is over the raw artifact bytes, independent of resolved target.
        let a = OsAdapterStage::load_default(TargetSelector::Alinux).unwrap();
        let u = OsAdapterStage::load_default(TargetSelector::Ubuntu).unwrap();
        assert_eq!(a.rule_digest(), u.rule_digest());
        assert_eq!(a.rule_digest().len(), 64);
    }

    #[test]
    fn digest_is_stable_sha256_and_content_free() {
        let a = stage_from(SAMPLE, OsTarget::Alinux).unwrap();
        let b = stage_from(SAMPLE, OsTarget::Alinux).unwrap();
        // Deterministic across builds and independent of the resolved target.
        assert_eq!(a.rule_digest(), b.rule_digest());
        // SHA-256 hex is 64 lowercase hex chars and never embeds rule content.
        assert_eq!(a.rule_digest().len(), 64);
        assert!(a.rule_digest().bytes().all(|c| c.is_ascii_hexdigit()));
        assert!(!a.rule_digest().contains("apt"));

        // Pin the exact SHA-256 of the artifact bytes so a hashing-algorithm
        // regression (e.g. reverting to a non-stable hasher) is caught. The
        // digest is over the raw artifact, independent of the resolved target.
        let one = "- ubuntu: a\n  alinux: b\n  direction: bidirectional\n  auto_apply: always\n";
        let expected: String = Sha256::digest(one.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            stage_from(one, OsTarget::Alinux).unwrap().rule_digest(),
            expected
        );
    }
}
