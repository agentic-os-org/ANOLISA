// SPDX-License-Identifier: MIT
// Copyright (c) 2026 eunomia-bpf org.
//! Hand-rolled parser for the taint DSL (docs/rule-language.md §2).

use super::ast::*;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Colon,
    Eq,
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
        } else if c == '#' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == '"' {
            i += 1;
            let start = i;
            while i < b.len() && b[i] != b'"' {
                i += 1;
            }
            if i >= b.len() {
                return Err("unterminated string".into());
            }
            out.push(Tok::Str(src[start..i].to_string()));
            i += 1;
        } else if c == ':' {
            out.push(Tok::Colon);
            i += 1;
        } else if c == '=' {
            out.push(Tok::Eq);
            i += 1;
        } else {
            let start = i;
            while i < b.len() {
                let d = b[i] as char;
                if d.is_whitespace() || d == '"' || d == ':' || d == '=' {
                    break;
                }
                i += 1;
            }
            out.push(Tok::Word(src[start..i].to_string()));
        }
    }
    Ok(out)
}

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        t
    }
    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Word(x)) if x == w)
    }
    fn word(&mut self) -> Result<String, String> {
        match self.next() {
            Some(Tok::Word(w)) => Ok(w),
            o => Err(format!("expected word, got {:?}", o)),
        }
    }
    fn string(&mut self) -> Result<String, String> {
        match self.next() {
            Some(Tok::Str(s)) => Ok(s),
            o => Err(format!("expected string, got {:?}", o)),
        }
    }
    fn eat(&mut self, w: &str) -> Result<(), String> {
        match self.next() {
            Some(Tok::Word(x)) if x == w => Ok(()),
            o => Err(format!("expected '{}', got {:?}", w, o)),
        }
    }
    fn kind(w: &str) -> Result<Kind, String> {
        match w {
            "file" => Ok(Kind::File),
            "endpoint" => Ok(Kind::Endpoint),
            "exec" => Ok(Kind::Exec),
            _ => Err(format!("unknown kind '{}'", w)),
        }
    }
    fn op(w: &str) -> Result<Op, String> {
        match w {
            "exec" => Ok(Op::Exec),
            "read" => Ok(Op::Read),
            "write" => Ok(Op::Write),
            "unlink" => Ok(Op::Unlink),
            "connect" => Ok(Op::Connect),
            "recv" => Ok(Op::Recv),
            "open" => Ok(Op::Open),
            _ => Err(format!("unknown op '{}'", w)),
        }
    }

    fn target(&mut self, op: Op) -> Result<Target, String> {
        let kind = if let Some(Tok::Word(w)) = self.peek() {
            if w == "file" || w == "endpoint" || w == "exec" {
                let w = self.word()?;
                P::kind(&w)?
            } else {
                return Err(format!("expected kind in target, got '{}'", w));
            }
        } else if op == Op::Exec {
            Kind::Exec
        } else {
            return Err("expected node kind in target".into());
        };
        let mut pattern = self.string()?;
        // Implicit basename matching: if the pattern contains no '/', treat it
        // as a basename match by prepending "**/".
        if kind == Kind::Exec && !pattern.contains('/') {
            pattern = format!("**/{}", pattern);
        }
        // Positional arguments: additional quoted strings after the target
        // pattern are treated as arguments (replaces the old `@arg` syntax).
        let arg = if matches!(self.peek(), Some(Tok::Str(_))) {
            Some(self.string()?)
        } else {
            None
        };
        Ok(Target { kind, pattern, arg })
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let mut lhs = self.term()?;
        loop {
            if self.is_word("and") {
                self.next();
                lhs = Expr::And(Box::new(lhs), Box::new(self.term()?));
            } else if self.is_word("or") {
                self.next();
                lhs = Expr::Or(Box::new(lhs), Box::new(self.term()?));
            } else {
                break;
            }
        }
        Ok(lhs)
    }
    fn term(&mut self) -> Result<Expr, String> {
        if self.is_word("not") {
            self.next();
            Ok(Expr::Not(self.word()?))
        } else if self.is_word("true") {
            self.next();
            Ok(Expr::True)
        } else {
            Ok(Expr::Label(self.word()?))
        }
    }
    fn cond(&mut self) -> Result<Cond, String> {
        let w = self.word()?;
        match w.as_str() {
            "target" => {
                let negate = self.is_word("not");
                if negate {
                    self.next();
                }
                Ok(Cond::Target {
                    negate,
                    pattern: self.string()?,
                })
            }
            "lineage-includes" => {
                self.eat("exec")?;
                Ok(Cond::LineageIncludes {
                    exec: self.string()?,
                })
            }
            "after" => {
                let gate_op = P::op(&self.word()?)?;
                let gate_pattern = self.string()?;
                let gate_exit = if self.is_word("exits") {
                    self.next();
                    if gate_op != Op::Exec {
                        return Err("`exits` is only valid on `after exec` gates".into());
                    }
                    let raw = self.word()?;
                    let code: u8 = raw
                        .parse()
                        .map_err(|_| format!("expected exit code 0..255, got '{raw}'"))?;
                    Some(code)
                } else {
                    None
                };
                let mut since = Vec::new();
                if self.is_word("since") {
                    self.next();
                    loop {
                        let op = P::op(&self.word()?)?;
                        let pat = self.string()?;
                        let arg = if matches!(self.peek(), Some(Tok::Str(_))) {
                            Some(self.string()?)
                        } else {
                            None
                        };
                        since.push((op, pat, arg));
                        if self.is_word("or") {
                            self.next();
                        } else {
                            break;
                        }
                    }
                }
                Ok(Cond::After {
                    gate_op,
                    gate_pattern,
                    gate_exit,
                    since,
                })
            }
            _ => Err(format!("unknown unless cond '{}'", w)),
        }
    }
    fn clause_effect(w: &str) -> Option<Effect> {
        match w {
            "notify" => Some(Effect::Notify),
            "block" => Some(Effect::Block),
            "kill" => Some(Effect::Kill),
            _ => None,
        }
    }
    fn clause(&mut self) -> Result<Clause, String> {
        let verb = self.word()?;
        let effect = P::clause_effect(&verb)
            .ok_or_else(|| format!("expected 'notify', 'block', or 'kill', got '{}'", verb))?;
        let op = P::op(&self.word()?)?;
        let target = self.target(op)?;
        let when = if self.is_word("if") {
            self.next();
            self.expr()?
        } else {
            Expr::True
        };
        let unless = if self.is_word("unless") {
            self.next();
            Some(self.cond()?)
        } else {
            None
        };
        Ok(Clause {
            op,
            target,
            when,
            unless,
            effect,
            source_index: 0,
        })
    }
}

pub fn parse(src: &str) -> Result<Policy, String> {
    let mut p = P { t: lex(src)?, i: 0 };
    let mut pol = Policy::default();
    while let Some(tok) = p.peek().cloned() {
        let kw = match tok {
            Tok::Word(w) => w,
            o => return Err(format!("expected declaration, got {:?}", o)),
        };
        match kw.as_str() {
            "label" => {
                return Err("the `label` keyword has been removed; use `source` instead (e.g. `source AGENT = exec \"**/your-agent\"`)".into());
            }
            "source" => {
                p.next();
                let label = p.word()?;
                match p.next() {
                    Some(Tok::Eq) => {}
                    o => return Err(format!("expected '=' in source, got {:?}", o)),
                }
                let kind = P::kind(&p.word()?)?;
                let pattern = p.string()?;
                pol.sources.push(Source {
                    label,
                    kind,
                    pattern,
                });
            }
            "declassify" | "endorse" => {
                let endorse = kw == "endorse";
                p.next();
                let label = p.word()?;
                p.eat("by")?;
                p.eat("exec")?;
                let gate = p.string()?;
                pol.xforms.push(Xform {
                    endorse,
                    label,
                    gate,
                });
            }
            "rule" => {
                p.next();
                let name = p.word()?;
                match p.next() {
                    Some(Tok::Colon) => {}
                    o => return Err(format!("expected ':' after rule name, got {:?}", o)),
                }
                let mut clauses = Vec::new();
                let mut reason = String::new();
                while let Some(Tok::Word(w)) = p.peek() {
                    if P::clause_effect(w).is_some() {
                        let mut clause = p.clause()?;
                        clause.source_index = clauses.len();
                        clauses.push(clause);
                    } else if w == "because" {
                        p.next();
                        reason = p.string()?;
                    } else {
                        break;
                    }
                }
                if pol.rules.iter().any(|rule| rule.name == name) {
                    return Err(format!("duplicate rule name `{name}`"));
                }
                pol.rules.push(Rule {
                    name,
                    clauses,
                    reason,
                });
            }
            other => return Err(format!("unknown declaration '{}'", other)),
        }
    }
    Ok(pol)
}
