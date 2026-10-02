// SPDX-License-Identifier: MIT
// Copyright (c) 2026 eunomia-bpf org.
//! Parsed form of the ActPlane taint DSL (docs/rule-language.md §2).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Endpoint,
    Exec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Exec,
    Read,
    Write,
    Unlink,
    Connect,
    Recv,
    Open,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub label: String,
    pub kind: Kind,
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub kind: Kind,
    pub pattern: String,
    pub arg: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    True,
    Label(String),
    Not(String),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cond {
    Target {
        negate: bool,
        pattern: String,
    },
    LineageIncludes {
        exec: String,
    },
    /// `after OP X [exits N] [since EV ("or" EV)*]`. Each EV is an
    /// (op, pattern, arg) event whose later occurrence makes the X gate stale.
    /// `since` empty = v1 latching semantics (gate fired ever). `exits N` is
    /// valid only for exec gates and stamps the gate after a matching process
    /// exits with status N.
    After {
        gate_op: Op,
        gate_pattern: String,
        gate_exit: Option<u8>,
        since: Vec<(Op, String, Option<String>)>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Clause {
    pub op: Op,
    pub target: Target,
    pub when: Expr,
    pub unless: Option<Cond>,
    /// Effect declared by the action verb that starts the clause
    /// (`notify`/`block`/`kill`).
    pub effect: Effect,
    /// Zero-based source clause index within the containing rule. This is
    /// metadata only, used to connect lowered kernel rules back to source text.
    pub source_index: usize,
}

/// Rule result. This is compiled into the kernel rule table and is the source
/// of truth for what happens when the rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum Effect {
    /// Report only (notify the agent); the operation proceeds.
    Notify = 0,
    /// Hard block (LSM -EPERM) when BPF LSM is available.
    #[default]
    Block = 1,
    /// Send SIGKILL to the current task.
    Kill = 2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub name: String,
    pub clauses: Vec<Clause>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Xform {
    pub endorse: bool,
    pub label: String,
    pub gate: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Policy {
    pub labels: Vec<String>,
    pub sources: Vec<Source>,
    pub rules: Vec<Rule>,
    pub xforms: Vec<Xform>,
}
