//! Lexical shell projection: named write targets reach the deterministic gate,
//! anything the lexer cannot attribute is left to the model.
//!
//! The analysis never touches the filesystem and never claims completeness; a
//! command it reports as having no write targets is only "no target named".

use semantic_arbiter::contract::Operation;

mod lex;
use lex::lex;

/// One path a shell command names as a write destination.
#[derive(Debug, PartialEq)]
pub(crate) struct Target {
    pub operation: Operation,
    /// Path exactly as written after shell quoting is removed.
    pub path: String,
    /// The effect reaches everything below the path, not only the path itself.
    pub tree: bool,
    /// A preceding `cd` makes a relative path's base unknown.
    pub base_unknown: bool,
}

/// Lexical effects of one command line.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Analysis {
    pub targets: Vec<Target>,
    /// Some part may write somewhere the lexer cannot name.
    pub unresolved: bool,
}

#[derive(Debug)]
struct Word {
    text: String,
    /// Unquoted expansion, glob, brace or tilde: the runtime value differs from `text`.
    dynamic: bool,
    quoted: bool,
}

#[derive(Debug)]
enum Token {
    Word(Word),
    Op(&'static str),
}

/// How a simple command shifts the base directory for later relative paths.
enum DirChange {
    /// Leaves the base unchanged.
    None,
    /// Returns to the verified workspace root, so the base stays known.
    Root,
    /// Moves somewhere the lexer cannot verify; later relative paths are unknown.
    Unknown,
}

const SEPARATORS: &[&str] = &[";", "&", "&&", "||", "|", "|&"];
const OPERATORS: &[&str] = &[
    "<<<", "<<-", "&>>", ";;", "&&", "||", "|&", ">>", ">|", "<<", "<>", ">&", "<&", "&>", ";",
    "&", "|", "<", ">", "(", ")",
];

const READ_ONLY: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "pwd",
    "echo",
    "printf",
    "wc",
    "uniq",
    "diff",
    "cmp",
    "stat",
    "file",
    "which",
    "type",
    "whoami",
    "id",
    "date",
    "true",
    "false",
    "test",
    "[",
    "du",
    "df",
    "tree",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "cut",
    "tr",
    "jq",
    "nl",
    "od",
    "xxd",
    "hexdump",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "uname",
    "hostname",
    "ps",
    "printenv",
    "column",
    "less",
    "more",
];
const GIT_READ_ONLY: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "rev-parse",
    "ls-files",
    "blame",
    "grep",
    "describe",
    "shortlog",
    "cat-file",
    "ls-tree",
    "rev-list",
    "reflog",
    "whatchanged",
];
const WRAPPERS: &[&str] = &["sudo", "env", "command", "nohup", "time", "builtin", "exec"];
const FIND_ACTIONS: &[&str] = &[
    "-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprint0", "-fprintf", "-fls",
];

/// Analyzes one command line against the workspace root; malformed quoting
/// marks the whole line unresolved.
pub(crate) fn analyze(command: &str, workspace: &str) -> Analysis {
    let Some((tokens, substitution)) = lex(command) else {
        return Analysis {
            targets: vec![],
            unresolved: true,
        };
    };
    let mut analysis = Analysis {
        targets: vec![],
        unresolved: substitution,
    };
    let mut base_unknown = false;
    let mut segment: Vec<Token> = vec![];
    for token in tokens.into_iter().chain([Token::Op(";")]) {
        match token {
            Token::Op(op) if SEPARATORS.contains(&op) => {
                let words = redirections(std::mem::take(&mut segment), base_unknown, &mut analysis);
                match simple(&words, base_unknown, workspace, &mut analysis) {
                    DirChange::None => {}
                    DirChange::Root => base_unknown = false,
                    DirChange::Unknown => base_unknown = true,
                }
            }
            other => segment.push(other),
        }
    }
    analysis
}

/// Records redirection targets and returns the remaining argument words.
fn redirections(tokens: Vec<Token>, base_unknown: bool, analysis: &mut Analysis) -> Vec<Word> {
    let mut words = vec![];
    let mut tokens = tokens.into_iter();
    while let Some(token) = tokens.next() {
        let op = match token {
            Token::Word(word) => {
                words.push(word);
                continue;
            }
            Token::Op(op) => op,
        };
        let operand = match tokens.next() {
            Some(Token::Word(word)) if !matches!(op, "(" | ")" | ";;") => word,
            other => {
                analysis.unresolved = true;
                if let Some(Token::Word(word)) = other {
                    words.push(word);
                }
                continue;
            }
        };
        let writes = match op {
            ">" | ">>" | ">|" | "&>" | "&>>" | "<>" => true,
            ">&" => !(operand.text == "-" || operand.text.bytes().all(|b| b.is_ascii_digit())),
            _ => false,
        };
        if writes {
            push(analysis, Operation::Replace, &operand, false, base_unknown);
        }
    }
    words
}

/// Analyzes one simple command; reports how it shifts the base directory.
fn simple(
    words: &[Word],
    base_unknown: bool,
    workspace: &str,
    analysis: &mut Analysis,
) -> DirChange {
    let mut args = words;
    loop {
        while args.first().is_some_and(assignment) {
            args = &args[1..];
        }
        match args.first() {
            Some(word) if !word.dynamic && WRAPPERS.contains(&word.text.as_str()) => {
                if args.get(1).is_some_and(|next| next.text.starts_with('-')) {
                    analysis.unresolved = true;
                    return DirChange::None;
                }
                args = &args[1..];
            }
            _ => break,
        }
    }
    let Some((program, rest)) = args.split_first() else {
        return DirChange::None;
    };
    if program.dynamic {
        analysis.unresolved = true;
        return DirChange::None;
    }
    let name = program.text.rsplit('/').next().unwrap_or_default();
    let mut add =
        |operation, word: &Word, tree| push(analysis, operation, word, tree, base_unknown);
    match name {
        // Only a return to the verified root keeps the base known; an absolute
        // cd there succeeds from any cwd, so it also recovers after a prior cd.
        "cd" => {
            return match rest {
                [word] if !word.dynamic && is_root(&word.text, workspace) => DirChange::Root,
                _ => DirChange::Unknown,
            };
        }
        "pushd" | "popd" => return DirChange::Unknown,
        "rm" | "unlink" | "rmdir" => {
            let tree = name == "rm" && flagged(rest, &["-r", "-R", "--recursive"], "rR");
            operands(rest, &[]).for_each(|word| add(Operation::Delete, word, tree));
        }
        "touch" => {
            operands(rest, &["-d", "-t", "-r"]).for_each(|w| add(Operation::Replace, w, false))
        }
        "mkdir" => operands(rest, &["-m"]).for_each(|w| add(Operation::Replace, w, false)),
        "tee" => operands(rest, &[])
            .filter(|word| word.text != "-")
            .for_each(|word| add(Operation::Replace, word, false)),
        "truncate" => {
            operands(rest, &["-s", "-r", "--size", "--reference"])
                .for_each(|word| add(Operation::Replace, word, false));
        }
        "cp" | "mv" | "ln" => {
            transfer(name == "mv", rest, base_unknown, analysis);
        }
        "sed" => sed(rest, base_unknown, analysis),
        "sort" => sort(rest, base_unknown, analysis),
        "find" => {
            if rest
                .iter()
                .any(|word| FIND_ACTIONS.contains(&word.text.as_str()))
            {
                analysis.unresolved = true;
            }
        }
        "git" => git(rest, base_unknown, analysis),
        _ if READ_ONLY.contains(&name) => {}
        _ if is_version_query(rest) => {}
        _ => analysis.unresolved = true,
    }
    DirChange::None
}

fn transfer(moves: bool, rest: &[Word], base_unknown: bool, analysis: &mut Analysis) -> bool {
    if rest.iter().any(|word| {
        word.text == "-t" || word.text.starts_with("--target-directory") || word.text == "-T"
    }) {
        analysis.unresolved = true;
        return false;
    }
    let paths: Vec<_> = operands(rest, &["-S", "--suffix"]).collect();
    let Some((destination, sources)) = paths.split_last() else {
        return false;
    };
    if sources.is_empty() {
        analysis.unresolved = true;
        return false;
    }
    for source in sources {
        if moves {
            push(analysis, Operation::Delete, source, true, base_unknown);
        }
        // Without a filesystem lookup the destination may be a file or a
        // directory, so both possible written paths are reported.
        let leaf = source
            .text
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default();
        if !leaf.is_empty() && !source.dynamic {
            let nested = Word {
                text: format!("{}/{leaf}", destination.text.trim_end_matches('/')),
                dynamic: destination.dynamic,
                quoted: false,
            };
            push(analysis, Operation::Replace, &nested, false, base_unknown);
        }
    }
    if sources.len() == 1 && !destination.text.ends_with('/') {
        push(
            analysis,
            Operation::Replace,
            destination,
            false,
            base_unknown,
        );
    }
    false
}

fn sed(rest: &[Word], base_unknown: bool, analysis: &mut Analysis) {
    let mut in_place = false;
    let mut script = false;
    let mut files = vec![];
    let mut index = 0;
    while let Some(word) = rest.get(index) {
        index += 1;
        let text = word.text.as_str();
        if text == "-e" || text == "--expression" || text == "-f" || text == "--file" {
            script = true;
            index += 1;
        } else if text.starts_with("--in-place") {
            in_place = true;
        } else if text.starts_with("--expression=") || text.starts_with("--file=") {
            script = true;
        } else if text.len() > 1 && text.starts_with('-') && !text.starts_with("--") {
            let cluster = &text[1..];
            // GNU sed reads everything after `i` in a cluster as the backup suffix.
            let options = match cluster.find('i') {
                Some(position) => {
                    in_place = true;
                    &cluster[..position]
                }
                None => cluster,
            };
            if options.ends_with(['e', 'f']) {
                script = true;
                index += 1;
            }
        } else if !text.starts_with("--") {
            if script {
                files.push(word);
            } else {
                script = true;
            }
        }
    }
    if in_place {
        files
            .into_iter()
            .for_each(|word| push(analysis, Operation::Replace, word, false, base_unknown));
    }
}

fn sort(rest: &[Word], base_unknown: bool, analysis: &mut Analysis) {
    for (index, word) in rest.iter().enumerate() {
        if word.text == "-o" || word.text == "--output" {
            match rest.get(index + 1) {
                Some(path) => push(analysis, Operation::Replace, path, false, base_unknown),
                None => analysis.unresolved = true,
            }
        } else if let Some(path) = word.text.strip_prefix("--output=") {
            let word = Word {
                text: path.into(),
                dynamic: word.dynamic,
                quoted: word.quoted,
            };
            push(analysis, Operation::Replace, &word, false, base_unknown);
        } else if word.text.starts_with("-o") && !word.text.starts_with("--") {
            let word = Word {
                text: word.text[2..].into(),
                dynamic: word.dynamic,
                quoted: word.quoted,
            };
            push(analysis, Operation::Replace, &word, false, base_unknown);
        }
    }
}

fn git(rest: &[Word], base_unknown: bool, analysis: &mut Analysis) {
    let mut index = 0;
    let mut tree_relocated = false;
    while let Some(word) = rest.get(index) {
        match word.text.as_str() {
            "-c" => index += 2,
            "--no-pager" | "--no-optional-locks" | "-P" => index += 1,
            "-C" | "--git-dir" | "--work-tree" => {
                tree_relocated = true;
                index += 2;
            }
            "--version" | "--help" => return,
            text if text.starts_with("--git-dir=") || text.starts_with("--work-tree=") => {
                tree_relocated = true;
                index += 1;
            }
            _ if word.text.starts_with('-') => {
                analysis.unresolved = true;
                return;
            }
            _ => break,
        }
    }
    let Some((command, rest)) = rest.get(index..).and_then(|rest| rest.split_first()) else {
        return;
    };
    if command.dynamic {
        analysis.unresolved = true;
        return;
    }
    // Read-only subcommands name no write target, so a relocated tree cannot
    // misattribute anything; a mutating one cannot be attributed once the tree moves.
    if GIT_READ_ONLY.contains(&command.text.as_str()) {
        return;
    }
    if tree_relocated {
        analysis.unresolved = true;
        return;
    }
    let mut add =
        |operation, word: &Word, tree| push(analysis, operation, word, tree, base_unknown);
    match command.text.as_str() {
        "rm" => {
            if !flagged(rest, &["--cached"], "") {
                let tree = flagged(rest, &["-r"], "r");
                operands(rest, &[]).for_each(|word| add(Operation::Delete, word, tree));
            }
        }
        "mv" => {
            transfer(true, rest, base_unknown, analysis);
        }
        "checkout" => match rest.iter().position(|word| word.text == "--") {
            Some(split) => rest[split + 1..]
                .iter()
                .for_each(|word| add(Operation::Replace, word, true)),
            None => analysis.unresolved = true,
        },
        "restore" => {
            let staged = flagged(rest, &["--staged", "-S"], "");
            let worktree = flagged(rest, &["--worktree", "-W"], "");
            if !staged || worktree {
                operands(rest, &["-s", "--source"])
                    .for_each(|word| add(Operation::Replace, word, true));
            }
        }
        "reset" => {
            if flagged(rest, &["--hard", "--merge", "--keep"], "") {
                analysis.unresolved = true;
            }
        }
        "clean" => {
            if !flagged(rest, &["-n", "--dry-run"], "n") {
                analysis.unresolved = true;
            }
        }
        _ => analysis.unresolved = true,
    }
}

fn push(
    analysis: &mut Analysis,
    operation: Operation,
    word: &Word,
    tree: bool,
    base_unknown: bool,
) {
    if word.dynamic || word.text.is_empty() {
        analysis.unresolved = true;
        return;
    }
    if ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"].contains(&word.text.as_str())
        || word.text.starts_with("/dev/fd/")
    {
        return;
    }
    analysis.targets.push(Target {
        operation,
        path: word.text.clone(),
        tree,
        base_unknown,
    });
}

fn assignment(word: &Word) -> bool {
    word.text.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}

/// A lone version/help query prints and exits; no operand can name a write target.
fn is_version_query(rest: &[Word]) -> bool {
    match rest {
        [word] => {
            !word.dynamic && matches!(word.text.as_str(), "--version" | "--help" | "-V" | "-h")
        }
        _ => false,
    }
}

/// Whether a cd destination is lexically the workspace root itself.
///
/// `..` and relative forms stay unknown: their landing depends on filesystem
/// state the lexer never reads, so only an absolute path equal to the verified
/// root can keep the base known.
fn is_root(text: &str, workspace: &str) -> bool {
    if !text.starts_with('/') {
        return false;
    }
    let mut parts = vec![];
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => return false,
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/")) == workspace
}

/// Whether any option is present as a long form or inside a short cluster.
fn flagged(args: &[Word], long: &[&str], short: &str) -> bool {
    args.iter()
        .take_while(|word| word.text != "--")
        .any(|word| {
            long.contains(&word.text.as_str())
                || (!short.is_empty()
                    && word.text.len() > 1
                    && word.text.starts_with('-')
                    && !word.text.starts_with("--")
                    && word.text[1..].contains(|c| short.contains(c)))
        })
}

/// Non-option operands; options listed in `valued` consume the next word.
fn operands<'a>(args: &'a [Word], valued: &'a [&str]) -> impl Iterator<Item = &'a Word> {
    let mut options = true;
    let mut skip = false;
    args.iter().filter(move |word| {
        if skip {
            skip = false;
            return false;
        }
        if options && word.text == "--" {
            options = false;
            return false;
        }
        if options && word.text.len() > 1 && word.text.starts_with('-') && !word.quoted {
            skip = valued.contains(&word.text.as_str());
            return false;
        }
        true
    })
}

#[cfg(test)]
mod tests;
