//! Literal path ranges and target-owned exclusions; no filesystem inspection.

use asc_agentsight_protocol::{
    FileProtectionExclusion, FileProtectionExclusionKind, FileProtectionTarget,
    FileProtectionTargetKind,
};
use cedar_policy::pst::{BinaryOp, Expr, Literal, PatternElem, Var};

use super::{Result, expressions::Branch, protections};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Range {
    Exact(String),
    Tree(String),
}

impl Range {
    pub(super) fn path(&self) -> &str {
        match self {
            Self::Exact(path) | Self::Tree(path) => path,
        }
    }

    pub(super) fn contains(&self, other: &Self) -> bool {
        self == other
            || matches!(self, Self::Tree(_))
                && (self.path() == other.path() || descendant(self.path(), other.path()))
    }
}

pub(super) fn descendant(parent: &str, path: &str) -> bool {
    if parent == "/" {
        return path != "/";
    }
    path.strip_prefix(parent)
        .is_some_and(|rest| rest.starts_with('/'))
}

pub(super) fn validate_path(path: &str) -> Result<()> {
    if !path.starts_with('/')
        || path.contains(['\0', '*', '?', '[', ']', '{', '}', '\\'])
        || path.contains("//")
        || path != "/" && path.ends_with('/')
        || path.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err("UNSUPPORTED_PATH");
    }
    Ok(())
}

pub(super) fn path_field(expr: &Expr) -> bool {
    matches!(expr, Expr::GetAttr { expr, attr } if attr == "path" && matches!(expr.as_ref(), Expr::Var(Var::Resource)))
}

fn exact(expr: &Expr) -> Option<&str> {
    let Expr::BinaryOp {
        op: BinaryOp::Eq,
        left,
        right,
    } = expr
    else {
        return None;
    };
    match (left.as_ref(), right.as_ref()) {
        (field, Expr::Literal(Literal::String(path)))
        | (Expr::Literal(Literal::String(path)), field)
            if path_field(field) =>
        {
            Some(path)
        }
        _ => None,
    }
}

// A lone trailing wildcard excludes the root. Only root equality OR its
// descendant pattern denotes the directory_tree contract.
pub(super) fn tree(expr: &Expr) -> Option<String> {
    let Expr::BinaryOp {
        op: BinaryOp::Or,
        left,
        right,
    } = expr
    else {
        return None;
    };
    for (root, children) in [
        (left.as_ref(), right.as_ref()),
        (right.as_ref(), left.as_ref()),
    ] {
        let Some(root) = exact(root) else {
            continue;
        };
        let Expr::Like { expr, pattern } = children else {
            continue;
        };
        if !path_field(expr) || !matches!(pattern.last(), Some(PatternElem::Wildcard)) {
            continue;
        }
        let prefix: Option<String> = pattern[..pattern.len() - 1]
            .iter()
            .map(|part| match part {
                PatternElem::Char(ch) => Some(*ch),
                PatternElem::Wildcard => None,
            })
            .collect();
        let expected = if root == "/" {
            "/".into()
        } else {
            format!("{root}/")
        };
        if prefix.as_deref() == Some(&expected) {
            return Some(root.into());
        }
    }
    None
}

pub(super) fn target(branch: Branch) -> Result<Option<FileProtectionTarget>> {
    if branch.operations == 0 {
        return Ok(None);
    }
    let range = branch.range.ok_or("UNBOUNDED_PATH_SCOPE")?;
    let mut exclusions: Vec<FileProtectionExclusion> = Vec::new();
    for excluded in branch.excluded {
        if excluded.contains(&range) {
            return Ok(None);
        }
        if !range.contains(&excluded) {
            continue;
        }
        // Excluding only a directory root cannot be expressed as a strict
        // descendant exclusion without also dropping the protected root.
        if excluded.path() == range.path() {
            return Err("UNSUPPORTED_ROOT_EXCLUSION");
        }
        let kind = match excluded {
            Range::Exact(_) => FileProtectionExclusionKind::ExactEntry,
            Range::Tree(_) => FileProtectionExclusionKind::DirectorySubtree,
        };
        if exclusions
            .iter()
            .any(|item| item.path == excluded.path() && item.kind != kind)
        {
            return Err("CONFLICTING_EXCLUSIONS");
        }
        exclusions.push(FileProtectionExclusion {
            path: excluded.path().into(),
            kind,
        });
    }
    let kind = match range {
        Range::Exact(_) => FileProtectionTargetKind::ExactFile,
        Range::Tree(_) => FileProtectionTargetKind::DirectoryTree,
    };
    Ok(Some(FileProtectionTarget {
        path: range.path().into(),
        kind,
        protections: protections(branch.operations),
        exclusions,
    }))
}
