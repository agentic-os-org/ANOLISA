// SPDX-License-Identifier: MIT
// Copyright (c) 2026 eunomia-bpf org.
//! Lower a parsed Policy to the kernel ABI (struct taint_config, see
//! bpf/taint.h): assign label/gate bits, compile boolean exprs to req/forbid
//! masks (via DNF), and lower globs to the kernel's exact/prefix/suffix/any
//! match kinds.

use super::ast::*;
use std::collections::{BTreeSet, HashMap};
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};

// must match bpf/taint.h
const PAT: usize = 64;
const ARG: usize = 24;
// Must match bpf/taint.h MAX_TAINT_* exactly (ABI). Sized for 100+ rules/policy.
const MAX_UPDATES: usize = 320;
const MAX_RULES: usize = 128;
const MAX_GATES: usize = 64;
const MAX_INVALS: usize = 64;

const M_EXACT: u8 = 0;
const M_PREFIX: u8 = 1;
const M_SUFFIX: u8 = 2;
const M_ANY: u8 = 3;
const M_CONTAINS: u8 = 4;
const MAX_CONTAINS_LITERAL: usize = 16; // mirrors TAINT_SUF_MAX in bpf/taint.h
const OP_EXEC: u8 = 0;
const OP_OPEN: u8 = 1;
const OP_WRITE: u8 = 2;
const OP_CONNECT: u8 = 3;
const OP_RECV: u8 = 4;
const C_NONE: u8 = 0;
const C_LINEAGE: u8 = 1;
const C_AFTER: u8 = 2;
const C_TARGET: u8 = 3;
const EFFECT_NOTIFY: u8 = 0;
const EFFECT_BLOCK: u8 = 1;
const EFFECT_KILL: u8 = 2;
const GATE_IMMEDIATE: i32 = -1;

#[repr(C)]
#[derive(Clone, Copy)]
struct CUpdate {
    op: u8,
    m: u8,
    target: [u8; PAT],
    arg: [u8; ARG],
    add: u64,
    del: u64,
    gates: u64,
    invals: u64,
    ipv4: u32,
    ipv4_mask: u32,
    gate_exit_code: i32,
    domain_id: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CRule {
    op: u8,
    m: u8,
    cond_kind: u8,
    cond_neg: u8,
    cond_match: u8,
    effect: u8,
    target: [u8; PAT],
    arg: [u8; ARG],
    cond_pat: [u8; PAT],
    req: u64,
    forbid: u64,
    gate: u64,
    rule_id: u32,
    ipv4: u32,
    ipv4_mask: u32,
    cond_ipv4: u32,
    cond_ipv4_mask: u32,
    gate_idx: u32,
    domain_id: u32,
    since_mask: u64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CConfig {
    n_updates: u32,
    n_rules: u32,
    updates: [CUpdate; MAX_UPDATES],
    rules: [CRule; MAX_RULES],
}

/// AgentSight ABI guard: exported so agentsight-enforcer can cross-check this
/// compiler's CConfig blob size against the engine's EXPECTED_CONFIG_BLOB_SIZE.
pub const COMPILED_CONFIG_BLOB_SIZE: usize = std::mem::size_of::<CConfig>();

fn set_pat(dst: &mut [u8], s: &str) {
    let b = s.as_bytes();
    let n = b.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&b[..n]);
    dst[n] = 0;
}

/// (match, literal) lowering for exec-side patterns (matched on comm).
fn lower_exec(pat: &str) -> (u8, String) {
    if pat == "*" || pat == "**" || pat == "**/*" {
        return (M_ANY, String::new());
    }
    let base = pat.rsplit('/').next().unwrap_or(pat);
    if let Some(stripped) = base.strip_suffix('*') {
        (M_PREFIX, stripped.to_string())
    } else {
        (M_EXACT, base.to_string())
    }
}

fn shorten_contains_literal(lit: &str) -> String {
    if lit.len() <= MAX_CONTAINS_LITERAL {
        return lit.to_string();
    }
    let trimmed = lit.trim_start_matches('/');
    if trimmed.len() <= MAX_CONTAINS_LITERAL {
        return trimmed.to_string();
    }
    for (idx, _) in trimmed.match_indices('/') {
        let candidate = &trimmed[idx + 1..];
        if !candidate.is_empty() && candidate.len() <= MAX_CONTAINS_LITERAL {
            return candidate.to_string();
        }
    }
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if !last.is_empty() && last.len() <= MAX_CONTAINS_LITERAL {
        return last.to_string();
    }
    let start = trimmed.len().saturating_sub(MAX_CONTAINS_LITERAL);
    trimmed[start..].to_string()
}

fn shorten_repo_relative_exact_literal(path: &str) -> String {
    if path.len() <= MAX_CONTAINS_LITERAL {
        return path.to_string();
    }
    for (idx, _) in path.match_indices('/') {
        let candidate = &path[idx + 1..];
        if candidate.contains('/') && candidate.len() <= MAX_CONTAINS_LITERAL {
            return candidate.to_string();
        }
    }
    if let Some((parent, _base)) = path.rsplit_once('/') {
        return shorten_contains_literal(&format!("{}/", parent));
    }
    shorten_contains_literal(path)
}

/// (match, literal) lowering for path patterns.
fn lower_path(pat: &str) -> (u8, String) {
    if pat == "*" || pat == "**" || pat == "**/*" {
        return (M_ANY, String::new());
    }
    let repo_relative = !pat.starts_with('/');
    // **/middle/** → contains "/middle/" (substring search)
    if let Some(inner) = pat.strip_prefix("**/").and_then(|r| r.strip_suffix("/**"))
        && !inner.contains('*')
    {
        return (M_CONTAINS, shorten_contains_literal(&format!("/{inner}/")));
    }
    // **/middle/* → contains "/middle/" (files directly inside)
    if let Some(inner) = pat.strip_prefix("**/").and_then(|r| r.strip_suffix("/*"))
        && !inner.contains('*')
    {
        return (M_CONTAINS, shorten_contains_literal(&format!("/{inner}/")));
    }
    if let Some(inner) = pat.strip_prefix("**/") {
        if let Some(suffix) = inner.strip_prefix('*') {
            return (M_SUFFIX, suffix.to_string());
        }
        if !inner.contains('*') {
            return (M_SUFFIX, format!("/{inner}"));
        }
        return (M_CONTAINS, shorten_contains_literal(inner));
    }
    if let Some(p) = pat.strip_suffix("/**") {
        if repo_relative {
            if !p.contains('*') {
                return (M_CONTAINS, shorten_contains_literal(&format!("{}/", p)));
            }
        } else {
            return (M_PREFIX, format!("{}/", p));
        }
    }
    if let Some(p) = pat.strip_suffix("**") {
        if repo_relative {
            if !p.contains('*') {
                return (M_CONTAINS, shorten_contains_literal(p));
            }
        } else {
            return (M_PREFIX, p.to_string());
        }
    }
    if let Some(p) = pat.strip_suffix("/*") {
        if repo_relative {
            if !p.contains('*') {
                return (M_CONTAINS, shorten_contains_literal(&format!("{}/", p)));
            }
        } else {
            return (M_PREFIX, format!("{}/", p));
        }
    }
    if let Some(p) = pat.strip_prefix('*') {
        if repo_relative {
            return (M_CONTAINS, shorten_contains_literal(p));
        }
        return (M_SUFFIX, p.to_string());
    }
    if let Some(idx) = pat.find('*') {
        if repo_relative {
            return (M_CONTAINS, shorten_contains_literal(&pat[..idx]));
        }
        return (M_PREFIX, pat[..idx].to_string());
    }
    if repo_relative && pat.contains('/') {
        return (M_CONTAINS, shorten_repo_relative_exact_literal(pat));
    }
    if repo_relative {
        return (M_CONTAINS, shorten_contains_literal(pat));
    }
    (M_EXACT, pat.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_relative_paths_match_absolute_runtime_paths() {
        assert_eq!(
            lower_path("pyproject.toml"),
            (M_CONTAINS, "pyproject.toml".into())
        );
        assert_eq!(
            lower_path("src/google/adk/agents/config_schemas/AgentConfig.json"),
            (M_CONTAINS, "config_schemas/".into())
        );
        assert_eq!(
            lower_path("ui/src/i18n/locales/en.ts"),
            (M_CONTAINS, "locales/en.ts".into())
        );
        assert_eq!(
            lower_path("codex-rs/app-server-protocol/schema/typescript/v2/**"),
            (M_CONTAINS, "typescript/v2/".into())
        );
        assert_eq!(
            lower_path("packages/oh-my-opencode-*/bin/**"),
            (M_CONTAINS, "oh-my-opencode-".into())
        );
        assert_eq!(
            lower_path("src/browser_harness/**"),
            (M_CONTAINS, "browser_harness/".into())
        );
        assert_eq!(
            lower_path("ui/src/i18n/locales/*.ts"),
            (M_CONTAINS, "i18n/locales/".into())
        );
        assert_eq!(lower_path("**/*.js"), (M_SUFFIX, ".js".into()));
        assert_eq!(lower_path("**/sec.env"), (M_SUFFIX, "/sec.env".into()));
        assert_eq!(lower_path("**/*"), (M_ANY, String::new()));
    }

    #[test]
    fn absolute_paths_keep_absolute_semantics() {
        assert_eq!(
            lower_path("/tmp/guarded/**"),
            (M_PREFIX, "/tmp/guarded/".into())
        );
        assert_eq!(
            lower_path("/tmp/guarded/file.txt"),
            (M_EXACT, "/tmp/guarded/file.txt".into())
        );
    }

    #[test]
    fn exec_wildcard_patterns_match_any_comm() {
        assert_eq!(lower_exec("*"), (M_ANY, String::new()));
        assert_eq!(lower_exec("**"), (M_ANY, String::new()));
        assert_eq!(lower_exec("**/*"), (M_ANY, String::new()));
    }

    #[test]
    fn endpoint_sources_lower_to_connect_and_recv_updates() {
        let pol = crate::dsl::parse::parse(r#"source NET = endpoint "127.0.0.1""#)
            .expect("parse endpoint source");
        let compiled = compile(&pol).expect("compile endpoint source");
        let cfg: CConfig =
            unsafe { std::ptr::read_unaligned(compiled.bytes.as_ptr() as *const CConfig) };
        assert_eq!(cfg.n_updates, 2);
        let ops = [cfg.updates[0].op, cfg.updates[1].op];
        assert!(ops.contains(&OP_CONNECT), "missing connect update: {ops:?}");
        assert!(ops.contains(&OP_RECV), "missing recv update: {ops:?}");
    }

    #[test]
    fn hostname_endpoint_sources_resolve_to_ipv4_updates() {
        let pol = crate::dsl::parse::parse(r#"source NET = endpoint "localhost""#)
            .expect("parse endpoint source");
        let compiled = compile(&pol).expect("compile endpoint source");
        assert_eq!(
            compiled.endpoint_resolutions.get("localhost"),
            Some(&vec!["127.0.0.1".to_string()])
        );

        let cfg: CConfig =
            unsafe { std::ptr::read_unaligned(compiled.bytes.as_ptr() as *const CConfig) };
        let (localhost, mask) = lower_ipv4("127.0.0.1");
        assert_eq!(cfg.n_updates, 2);
        for update in &cfg.updates[..cfg.n_updates as usize] {
            assert_eq!(update.ipv4, localhost);
            assert_eq!(update.ipv4_mask, mask);
        }
    }

    #[test]
    fn hostname_endpoint_rule_resolves_to_ipv4_matcher() {
        let pol = crate::dsl::parse::parse(
            r#"
            rule local:
              notify connect endpoint "localhost" if true
              because "local host"
            "#,
        )
        .expect("parse endpoint rule");
        let compiled = compile(&pol).expect("compile endpoint rule");
        let cfg: CConfig =
            unsafe { std::ptr::read_unaligned(compiled.bytes.as_ptr() as *const CConfig) };
        let (localhost, mask) = lower_ipv4("127.0.0.1");
        assert_eq!(cfg.n_rules, 1);
        assert_eq!(cfg.rules[0].ipv4, localhost);
        assert_eq!(cfg.rules[0].ipv4_mask, mask);
    }

    #[test]
    fn wildcard_hostnames_are_not_resolved_as_exact_hosts() {
        assert_eq!(hostname_candidate("*.internal"), None);
        assert_eq!(hostname_candidate("api.internal"), Some("api.internal"));
    }

    #[test]
    fn file_path_at_abi_limit_compiles() {
        // 63 bytes: exactly PAT - 1, fits with NUL terminator in [u8; 64]
        let path = format!("/{}", "a".repeat(62)); // "/" + 62 × 'a' = 63 bytes
        assert_eq!(path.len(), 63);
        let dsl = format!(
            "source T = file \"{}\"\nrule r:\n  notify open file \"{}\" if T\n  because \"test\"\n",
            path, path
        );
        assert!(
            crate::dsl::compile_str(&dsl).is_ok(),
            "63-byte path should compile successfully"
        );
    }

    #[test]
    fn file_path_exceeding_abi_limit_rejected() {
        // 64 bytes: exactly PAT, too long for [u8; PAT] with NUL terminator
        let path = format!("/{}", "a".repeat(63)); // "/" + 63 × 'a' = 64 bytes
        assert_eq!(path.len(), 64);
        let dsl = format!(
            "source T = file \"{}\"\nrule r:\n  notify open file \"{}\" if T\n  because \"test\"\n",
            path, path
        );
        let err = match crate::dsl::compile_str(&dsl) {
            Ok(_) => panic!("64-byte path must be rejected at compile time"),
            Err(err) => err,
        };
        assert!(
            err.contains("byte") && err.contains("ABI"),
            "error should mention byte ABI limit: {err}"
        );
    }
}

fn ipv4_to_kernel(addr: Ipv4Addr) -> u32 {
    let octets = addr.octets();
    (octets[0] as u32)
        | ((octets[1] as u32) << 8)
        | ((octets[2] as u32) << 16)
        | ((octets[3] as u32) << 24)
}

fn kernel_ipv4_to_string(ip: u32) -> String {
    format!(
        "{}.{}.{}.{}",
        ip & 0xff,
        (ip >> 8) & 0xff,
        (ip >> 16) & 0xff,
        (ip >> 24) & 0xff
    )
}

fn looks_like_ipv4_prefix(pat: &str) -> bool {
    if pat == "*" {
        return true;
    }
    let body = pat.trim_end_matches('.');
    !body.is_empty()
        && body
            .split('.')
            .all(|tok| !tok.is_empty() && tok.bytes().all(|b| b.is_ascii_digit()))
}

/// Lower an IPv4 prefix/host pattern to (net, mask) in the same byte order as
/// the kernel's `sin_addr.s_addr` (octet k at bit 8*k). "*" -> match-any (0,0).
/// "10.0.0." -> /24, "10.0.0.5" -> /32.
fn lower_numeric_ipv4(pat: &str) -> Option<(u32, u32)> {
    if pat == "*" {
        return Some((0, 0));
    }
    let body = pat.strip_suffix('.').unwrap_or(pat);
    let mut net: u32 = 0;
    let mut mask: u32 = 0;
    let mut k = 0u32;
    for tok in body.split('.') {
        if k >= 4 {
            break;
        }
        match tok.parse::<u8>() {
            Ok(o) => {
                net |= (o as u32) << (8 * k);
                mask |= 0xffu32 << (8 * k);
                k += 1;
            }
            Err(_) => return None,
        }
    }
    if k == 0 { None } else { Some((net, mask)) }
}

#[cfg(test)]
fn lower_ipv4(pat: &str) -> (u32, u32) {
    lower_numeric_ipv4(pat).unwrap_or((0, u32::MAX))
}

fn hostname_candidate(pat: &str) -> Option<&str> {
    if pat == "*" || pat.contains('*') || pat.contains(':') || looks_like_ipv4_prefix(pat) {
        return None;
    }
    let host = pat.trim_end_matches('.');
    if host.is_empty() || host.contains('/') {
        return None;
    }
    if host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        Some(host)
    } else {
        None
    }
}

fn resolve_hostname_ipv4s(host: &str) -> Vec<u32> {
    if host.eq_ignore_ascii_case("localhost") {
        return vec![ipv4_to_kernel(Ipv4Addr::new(127, 0, 0, 1))];
    }
    let Ok(addrs) = (host, 0).to_socket_addrs() else {
        return Vec::new();
    };
    let mut out = BTreeSet::new();
    for addr in addrs {
        if let SocketAddr::V4(v4) = addr {
            out.insert(ipv4_to_kernel(*v4.ip()));
        }
    }
    out.into_iter().collect()
}

/// Gate allocation table: `(kernel op, match kind, literal, exit code)` maps to
/// `(latching bit, gate slot index)`.
type GateBits = HashMap<(u8, u8, String, Option<u8>), (u64, u32)>;

struct Ctx {
    labels: HashMap<String, u64>,
    used_labels: u64,
    updates: Vec<CUpdate>,
    gate_bits: GateBits,
    next_gate: u32,
    inval_slots: HashMap<(u8, u8, String, String), u32>,
    next_inval: u32,
    endpoint_cache: HashMap<String, Vec<(u32, u32)>>,
    endpoint_resolutions: HashMap<String, Vec<String>>,
}
impl Ctx {
    fn endpoint_matches(&mut self, pat: &str) -> Vec<(u32, u32)> {
        if let Some(matches) = self.endpoint_cache.get(pat) {
            return matches.clone();
        }
        let matches = if let Some(numeric) = lower_numeric_ipv4(pat) {
            vec![numeric]
        } else if let Some(host) = hostname_candidate(pat) {
            let addrs = resolve_hostname_ipv4s(host);
            self.endpoint_resolutions.insert(
                pat.to_string(),
                addrs
                    .iter()
                    .map(|addr| kernel_ipv4_to_string(*addr))
                    .collect(),
            );
            if addrs.is_empty() {
                vec![(0, u32::MAX)]
            } else {
                addrs.into_iter().map(|addr| (addr, u32::MAX)).collect()
            }
        } else {
            vec![(0, u32::MAX)]
        };
        self.endpoint_cache.insert(pat.to_string(), matches.clone());
        matches
    }

    fn endpoint_condition_match(&mut self, pat: &str, negate: bool) -> (u32, u32) {
        let matches = self.endpoint_matches(pat);
        if matches.len() == 1 {
            return matches[0];
        }
        // `unless target PAT` should fail closed when a hostname expands to
        // several A records but the current ABI can store only one condition
        // address. For `target not PAT`, use match-any before negation so the
        // condition is false for every endpoint, and the rule still applies.
        if negate { (0, 0) } else { (0, u32::MAX) }
    }

    fn add_update(&mut self, spec: UpdateSpec<'_>) -> Result<(), String> {
        for u in &mut self.updates {
            if u.op == spec.op
                && u.m == spec.m
                && u.ipv4 == spec.ipv4
                && u.ipv4_mask == spec.ipv4_mask
                && u.gate_exit_code == spec.gate_exit_code
                && pat_eq(&u.target, spec.target)
                && arg_eq(&u.arg, spec.arg)
            {
                u.add |= spec.add;
                u.del |= spec.del;
                u.gates |= spec.gates;
                u.invals |= spec.invals;
                return Ok(());
            }
        }
        if self.updates.len() >= MAX_UPDATES {
            return Err(format!(
                "too many event updates ({} > {})",
                self.updates.len() + 1,
                MAX_UPDATES
            ));
        }
        let mut u = CUpdate {
            op: spec.op,
            m: spec.m,
            target: [0; PAT],
            arg: [0; ARG],
            add: spec.add,
            del: spec.del,
            gates: spec.gates,
            invals: spec.invals,
            ipv4: spec.ipv4,
            ipv4_mask: spec.ipv4_mask,
            gate_exit_code: spec.gate_exit_code,
            domain_id: 0,
        };
        set_pat(&mut u.target, spec.target);
        if !spec.arg.is_empty() {
            set_pat(&mut u.arg, spec.arg);
        }
        self.updates.push(u);
        Ok(())
    }

    fn label_bit(&mut self, name: &str) -> Result<u64, String> {
        if let Some(b) = self.labels.get(name) {
            return Ok(*b);
        }
        let bit_idx = (0..64)
            .find(|idx| self.used_labels & (1u64 << idx) == 0)
            .ok_or_else(|| "too many labels (max 64)".to_string())?;
        let b = 1u64 << bit_idx;
        self.used_labels |= b;
        self.labels.insert(name.to_string(), b);
        Ok(b)
    }
    /// Returns (gate bit, gate slot index). The index is what the engine uses to
    /// look up the gate's epoch for staleness; the bit is the v1 latching mask.
    fn gate_bit(
        &mut self,
        gate_op: Op,
        pat: &str,
        gate_exit: Option<u8>,
    ) -> Result<(u64, u32), String> {
        let (low_op, m, lit) = match gate_op {
            Op::Exec => {
                let (m, l) = lower_exec(pat);
                (OP_EXEC, m, l)
            }
            Op::Read | Op::Open => {
                let (m, l) = lower_path(pat);
                (OP_OPEN, m, l)
            }
            Op::Write | Op::Unlink => {
                let (m, l) = lower_path(pat);
                (OP_WRITE, m, l)
            }
            other => {
                return Err(format!(
                    "`after {}` is not supported as a gate (use exec/read/write)",
                    op_name(other)
                ));
            }
        };
        if gate_exit.is_some() && low_op != OP_EXEC {
            return Err("`exits` is only valid on `after exec` gates".into());
        }
        let key = (low_op, m, lit.clone(), gate_exit);
        if let Some(b) = self.gate_bits.get(&key) {
            return Ok(*b);
        }
        if self.next_gate >= 64 || self.next_gate as usize >= MAX_GATES {
            return Err("too many gates".into());
        }
        let idx = self.next_gate;
        let b = 1u64 << idx;
        self.next_gate += 1;
        self.add_update(UpdateSpec {
            op: low_op,
            m,
            target: &lit,
            arg: "",
            add: 0,
            del: 0,
            gates: b,
            invals: 0,
            ipv4: 0,
            ipv4_mask: 0,
            gate_exit_code: gate_exit.map(i32::from).unwrap_or(GATE_IMMEDIATE),
        })?;
        self.gate_bits.insert(key, (b, idx));
        Ok((b, idx))
    }
    /// Allocate (or reuse) a `since` invalidator slot, returning its bit in the
    /// rule's `since_mask`. `op` is the lowered taint_op; the pattern is matched
    /// like a sink target (exec on comm, others on path).
    fn inval_slot(
        &mut self,
        op: u8,
        kind: Kind,
        pat: &str,
        arg: Option<&str>,
    ) -> Result<u64, String> {
        let (m, lit) = if op == OP_EXEC {
            lower_exec(pat)
        } else {
            lower_target(op, kind, pat)
        };
        let arg_s = arg.unwrap_or("");
        let key = (op, m, lit.clone(), arg_s.to_string());
        if let Some(i) = self.inval_slots.get(&key) {
            return Ok(1u64 << *i);
        }
        if self.next_inval >= 64 || self.next_inval as usize >= MAX_INVALS {
            return Err("too many `since` invalidators (max 64)".into());
        }
        let idx = self.next_inval;
        self.next_inval += 1;
        let bit = 1u64 << idx;
        self.add_update(UpdateSpec {
            op,
            m,
            target: &lit,
            arg: arg_s,
            add: 0,
            del: 0,
            gates: 0,
            invals: bit,
            ipv4: 0,
            ipv4_mask: 0,
            gate_exit_code: GATE_IMMEDIATE,
        })?;
        self.inval_slots.insert(key, idx);
        Ok(bit)
    }
}

struct UpdateSpec<'a> {
    op: u8,
    m: u8,
    target: &'a str,
    arg: &'a str,
    add: u64,
    del: u64,
    gates: u64,
    invals: u64,
    ipv4: u32,
    ipv4_mask: u32,
    gate_exit_code: i32,
}

fn pat_eq(buf: &[u8; PAT], s: &str) -> bool {
    let mut pat = [0u8; PAT];
    set_pat(&mut pat, s);
    *buf == pat
}

fn arg_eq(buf: &[u8; ARG], s: &str) -> bool {
    let mut a = [0u8; ARG];
    let b = s.as_bytes();
    let n = b.len().min(ARG);
    a[..n].copy_from_slice(&b[..n]);
    *buf == a
}

/// expr -> disjunction of (req_mask, forbid_mask)
fn dnf(e: &Expr, ctx: &mut Ctx) -> Result<Vec<(u64, u64)>, String> {
    Ok(match e {
        Expr::True => vec![(0, 0)],
        Expr::Label(l) => vec![(ctx.label_bit(l)?, 0)],
        Expr::Not(l) => vec![(0, ctx.label_bit(l)?)],
        Expr::Or(a, b) => {
            let mut v = dnf(a, ctx)?;
            v.extend(dnf(b, ctx)?);
            v
        }
        Expr::And(a, b) => {
            let (da, db) = (dnf(a, ctx)?, dnf(b, ctx)?);
            let mut v = Vec::new();
            for (ra, fa) in &da {
                for (rb, fb) in &db {
                    v.push((ra | rb, fa | fb));
                }
            }
            v
        }
    })
}

/// Human-readable verb for a DSL op, used in the feedback payload.
fn op_name(op: Op) -> &'static str {
    match op {
        Op::Exec => "exec",
        Op::Read => "read",
        Op::Open => "open",
        Op::Write => "write",
        Op::Unlink => "unlink",
        Op::Connect => "connect",
        Op::Recv => "recv",
    }
}

fn op_lowers(op: Op) -> Result<&'static [u8], String> {
    match op {
        Op::Exec => Ok(&[OP_EXEC]),
        Op::Read => Ok(&[OP_OPEN]),
        Op::Open => Ok(&[OP_OPEN]),
        Op::Write | Op::Unlink => Ok(&[OP_WRITE]),
        Op::Connect => Ok(&[OP_CONNECT]),
        Op::Recv => Ok(&[OP_RECV]),
    }
}

/// Lower a `since` event op to the single taint_op the engine stamps on. Only
/// read/write/exec can invalidate a gate.
fn inval_op(op: Op) -> Result<u8, String> {
    match op {
        Op::Read | Op::Open => Ok(OP_OPEN),
        Op::Write | Op::Unlink => Ok(OP_WRITE),
        Op::Exec => Ok(OP_EXEC),
        other => Err(format!(
            "`since {}` is not a valid invalidator (use exec/read/write/open/unlink)",
            op_name(other)
        )),
    }
}

fn lower_target(op: u8, kind: Kind, pat: &str) -> (u8, String) {
    let _ = kind;
    match op {
        OP_EXEC => lower_exec(pat),
        OP_CONNECT | OP_RECV => (M_ANY, String::new()),
        _ => lower_path(pat),
    }
}

fn lower_effect(effect: Effect) -> u8 {
    match effect {
        Effect::Notify => EFFECT_NOTIFY,
        Effect::Block => EFFECT_BLOCK,
        Effect::Kill => EFFECT_KILL,
    }
}

/// Per-lowered-rule metadata, indexed by `rule_id`, kept Rust-side for building
/// the corrective-feedback payload (docs/feedback-design.md §6).
#[derive(Clone)]
pub struct RuleMeta {
    pub name: String,
    pub reason: String,
    pub effect: Effect,
    /// Operations represented by this lowered rule. This is usually a single
    /// DSL op, kept as a list for compatibility with existing feedback code.
    pub ops: Vec<String>,
    pub clause_op: String,
    pub kernel_op: String,
    pub target_kind: Kind,
    pub target_pattern: String,
    pub target_arg: Option<String>,
    pub clause_source_index: usize,
    pub source: Option<RuleSourceMeta>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleSourceMeta {
    pub source_ref: String,
    pub binding_mode: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
    pub clause_start_line: Option<usize>,
    pub clause_end_line: Option<usize>,
    pub clause_text: Option<String>,
}

pub struct Compiled {
    pub bytes: Vec<u8>,
    pub reasons: Vec<String>, // indexed by lowered rule_id
    pub meta: Vec<RuleMeta>,  // indexed by lowered rule_id
    pub labels: HashMap<String, u64>,
    /// Exact hostname endpoint patterns that were resolved at compile time.
    /// Non-empty values are the IPv4 A records expanded into kernel matchers;
    /// an empty value means resolution was attempted but yielded no IPv4.
    pub endpoint_resolutions: HashMap<String, Vec<String>>,
}

fn collect_label_names(pol: &Policy) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    for s in &pol.sources {
        names.insert(s.label.clone());
    }
    for x in &pol.xforms {
        names.insert(x.label.clone());
    }
    for r in &pol.rules {
        for cl in &r.clauses {
            collect_expr_labels(&cl.when, &mut names);
        }
    }
    names.into_iter().collect()
}

fn validate_label_bindings(labels: &HashMap<String, u64>) -> Result<u64, String> {
    let mut used = 0u64;
    for (name, bit) in labels {
        if name.is_empty() {
            return Err("label names must not be empty".into());
        }
        if *bit == 0 || bit.count_ones() != 1 {
            return Err(format!("label `{name}` has invalid bit mask 0x{bit:x}"));
        }
        if used & *bit != 0 {
            return Err(format!("label bit 0x{bit:x} is assigned more than once"));
        }
        used |= *bit;
    }
    Ok(used)
}

fn collect_expr_labels(expr: &Expr, out: &mut std::collections::BTreeSet<String>) {
    match expr {
        Expr::Label(l) | Expr::Not(l) => {
            out.insert(l.clone());
        }
        Expr::And(a, b) | Expr::Or(a, b) => {
            collect_expr_labels(a, out);
            collect_expr_labels(b, out);
        }
        Expr::True => {}
    }
}

pub fn compile(pol: &Policy) -> Result<Compiled, String> {
    compile_with_labels(pol, &HashMap::new())
}

pub fn compile_with_labels(
    pol: &Policy,
    existing_labels: &HashMap<String, u64>,
) -> Result<Compiled, String> {
    let sorted_labels = collect_label_names(pol);
    let pre_labels = existing_labels.clone();
    let used_labels = validate_label_bindings(&pre_labels)?;

    let mut ctx = Ctx {
        used_labels,
        labels: pre_labels,
        updates: Vec::new(),
        gate_bits: HashMap::new(),
        next_gate: 0,
        inval_slots: HashMap::new(),
        next_inval: 0,
        endpoint_cache: HashMap::new(),
        endpoint_resolutions: HashMap::new(),
    };
    for name in &sorted_labels {
        ctx.label_bit(name)?;
    }
    let mut rules: Vec<CRule> = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    let mut meta: Vec<RuleMeta> = Vec::new();

    for s in &pol.sources {
        let bit = ctx.label_bit(&s.label)?;
        let (op, m, lit, ipv4, ipv4_mask) = match s.kind {
            Kind::Exec => {
                let (m, lit) = lower_exec(&s.pattern);
                (OP_EXEC, m, lit, 0, 0)
            }
            Kind::File => {
                let (m, lit) = lower_path(&s.pattern);
                if lit.len() >= PAT {
                    return Err(format!(
                        "source '{}': file path pattern is {} bytes, exceeds the {} byte ABI limit (PAT={})",
                        s.label,
                        lit.len(),
                        PAT - 1,
                        PAT
                    ));
                }
                (OP_OPEN, m, lit, 0, 0)
            }
            Kind::Endpoint => {
                let endpoints = ctx.endpoint_matches(&s.pattern);
                for (n, mk) in endpoints {
                    for op in [OP_CONNECT, OP_RECV] {
                        ctx.add_update(UpdateSpec {
                            op,
                            m: M_ANY,
                            target: "",
                            arg: "",
                            add: bit,
                            del: 0,
                            gates: 0,
                            invals: 0,
                            ipv4: n,
                            ipv4_mask: mk,
                            gate_exit_code: GATE_IMMEDIATE,
                        })?;
                    }
                }
                continue;
            }
        };
        ctx.add_update(UpdateSpec {
            op,
            m,
            target: &lit,
            arg: "",
            add: bit,
            del: 0,
            gates: 0,
            invals: 0,
            ipv4,
            ipv4_mask,
            gate_exit_code: GATE_IMMEDIATE,
        })?;
    }
    for x in &pol.xforms {
        let bit = ctx.label_bit(&x.label)?;
        let (m, lit) = lower_exec(&x.gate);
        ctx.add_update(UpdateSpec {
            op: OP_EXEC,
            m,
            target: &lit,
            arg: "",
            add: if x.endorse { bit } else { 0 },
            del: if x.endorse { 0 } else { bit },
            gates: 0,
            invals: 0,
            ipv4: 0,
            ipv4_mask: 0,
            gate_exit_code: GATE_IMMEDIATE,
        })?;
    }
    for rule in &pol.rules {
        for cl in &rule.clauses {
            for op in op_lowers(cl.op)? {
                let op = *op;
                let target_matches = if op == OP_CONNECT || op == OP_RECV {
                    ctx.endpoint_matches(&cl.target.pattern)
                        .into_iter()
                        .map(|(ipv4, ipv4_mask)| (M_ANY, String::new(), ipv4, ipv4_mask))
                        .collect::<Vec<_>>()
                } else {
                    let (tm, tlit) = lower_target(op, cl.target.kind, &cl.target.pattern);
                    if (op == OP_OPEN || op == OP_WRITE) && tlit.len() >= PAT {
                        return Err(format!(
                            "rule '{}': target file path pattern is {} bytes, exceeds the {} byte ABI limit (PAT={})",
                            rule.name,
                            tlit.len(),
                            PAT - 1,
                            PAT
                        ));
                    }
                    vec![(tm, tlit, 0, 0)]
                };
                for (tm, tlit, ipv4, ipv4_mask) in target_matches {
                    // condition
                    let (mut ck, mut cneg, mut cm, mut clit, mut gate) =
                        (C_NONE, 0u8, M_EXACT, String::new(), 0u64);
                    let (mut cipv4, mut cipv4_mask) = (0u32, 0u32);
                    let mut gate_idx = 0u32;
                    let mut since_mask = 0u64;
                    match &cl.unless {
                        None => {}
                        Some(Cond::Target { negate, pattern }) => {
                            ck = C_TARGET;
                            cneg = *negate as u8;
                            if op == OP_CONNECT || op == OP_RECV {
                                let (n, mk) = ctx.endpoint_condition_match(pattern, *negate);
                                cipv4 = n;
                                cipv4_mask = mk;
                            } else {
                                let (m, l) = lower_target(op, cl.target.kind, pattern);
                                if (op == OP_OPEN || op == OP_WRITE) && l.len() >= PAT {
                                    return Err(format!(
                                        "rule '{}': condition target file path pattern is {} bytes, exceeds the {} byte ABI limit (PAT={})",
                                        rule.name,
                                        l.len(),
                                        PAT - 1,
                                        PAT
                                    ));
                                }
                                cm = m;
                                clit = l;
                            }
                        }
                        Some(Cond::LineageIncludes { exec }) => {
                            ck = C_LINEAGE;
                            let (b, _idx) = ctx.gate_bit(Op::Exec, exec, None)?;
                            gate = b;
                        }
                        Some(Cond::After {
                            gate_op,
                            gate_pattern,
                            gate_exit,
                            since,
                        }) => {
                            ck = C_AFTER;
                            let (b, idx) = ctx.gate_bit(*gate_op, gate_pattern, *gate_exit)?;
                            gate = b;
                            gate_idx = idx;
                            for (op, pat, arg) in since {
                                let iop = inval_op(*op)?;
                                since_mask |=
                                    ctx.inval_slot(iop, cl.target.kind, pat, arg.as_deref())?;
                            }
                        }
                    }
                    for (req, forbid) in dnf(&cl.when, &mut ctx)? {
                        let rule_id = meta.len() as u32;
                        reasons.push(rule.reason.clone());
                        meta.push(RuleMeta {
                            name: rule.name.clone(),
                            reason: rule.reason.clone(),
                            effect: cl.effect,
                            ops: vec![op_name(cl.op).to_string()],
                            clause_op: op_name(cl.op).to_string(),
                            kernel_op: kernel_op_name(op).to_string(),
                            target_kind: cl.target.kind,
                            target_pattern: cl.target.pattern.clone(),
                            target_arg: cl.target.arg.clone(),
                            clause_source_index: cl.source_index,
                            source: None,
                        });
                        let mut cr = CRule {
                            op,
                            m: tm,
                            cond_kind: ck,
                            cond_neg: cneg,
                            cond_match: cm,
                            effect: lower_effect(cl.effect),
                            target: [0; PAT],
                            arg: [0; ARG],
                            cond_pat: [0; PAT],
                            req,
                            forbid,
                            gate,
                            rule_id,
                            ipv4,
                            ipv4_mask,
                            cond_ipv4: cipv4,
                            cond_ipv4_mask: cipv4_mask,
                            gate_idx,
                            domain_id: 0,
                            since_mask,
                        };
                        set_pat(&mut cr.target, &tlit);
                        if let Some(a) = &cl.target.arg {
                            set_pat(&mut cr.arg, a);
                        }
                        set_pat(&mut cr.cond_pat, &clit);
                        rules.push(cr);
                    }
                }
            }
        }
    }

    if rules.len() > MAX_RULES {
        return Err(format!(
            "too many compiled rules ({} > {})",
            rules.len(),
            MAX_RULES
        ));
    }

    // build the repr(C) config
    let mut cfg: CConfig = unsafe { std::mem::zeroed() };
    cfg.n_updates = ctx.updates.len() as u32;
    cfg.n_rules = rules.len() as u32;
    for (i, u) in ctx.updates.iter().enumerate() {
        cfg.updates[i] = *u;
    }
    for (i, r) in rules.iter().enumerate() {
        cfg.rules[i] = *r;
    }

    let bytes = unsafe {
        std::slice::from_raw_parts(
            &cfg as *const CConfig as *const u8,
            std::mem::size_of::<CConfig>(),
        )
    }
    .to_vec();
    Ok(Compiled {
        bytes,
        reasons,
        meta,
        labels: ctx.labels,
        endpoint_resolutions: ctx.endpoint_resolutions,
    })
}

fn kernel_op_name(op: u8) -> &'static str {
    match op {
        OP_EXEC => "exec",
        OP_OPEN => "read",
        OP_WRITE => "write",
        OP_CONNECT => "connect",
        OP_RECV => "recv",
        _ => "op",
    }
}
