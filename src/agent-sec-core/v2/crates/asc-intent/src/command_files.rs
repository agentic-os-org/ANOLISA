//! Static data operands of simple Bash commands. Unsupported syntax stays on the model path.

use std::path::{Component, Path};

use crate::policy_validation::spawn_rule;
use crate::{CommandInput, IntentSpec};

#[derive(Default)]
struct Word {
    text: String,
    glob: bool,
}

/// Names common external utilities without consulting the daemon's PATH or shell environment.
/// Other programs and shell constructs stay on the model path.
pub(crate) fn path_program(input: &CommandInput) -> Option<&'static str> {
    match words(&input.command)?.first()?.text.as_str() {
        "cat" => Some("cat"),
        "grep" => Some("grep"),
        "ls" => Some("ls"),
        "curl" => Some("curl"),
        "tar" => Some("tar"),
        _ => None,
    }
}

/// Recognizes only complete commands, so parsing a prefix never hides another shell operation.
pub(crate) fn file_request(input: &CommandInput) -> Option<(String, Vec<String>)> {
    let words = words(&input.command)?;
    let (program, args) = words.split_first()?;
    let mut spec = IntentSpec::default();
    let scope = match program.text.as_str() {
        "cat" | "/bin/cat" | "/usr/bin/cat" => {
            let (inputs, output) = match args {
                [inputs @ .., redirect, output] if redirect.text == ">" => (inputs, Some(output)),
                inputs => (inputs, None),
            };
            if !inputs.iter().any(|input| input.glob) {
                return None;
            }
            for input_word in inputs {
                let source = path(&input.cwd, &input_word.text)?;
                if input_word.glob {
                    let (dir, name) = source.rsplit_once('/')?;
                    if dir.is_empty() || dir.contains(['*', '?']) || !name.contains(['*', '?']) {
                        return None;
                    }
                    spec.allow_read.push(format!("{dir}/*"));
                } else {
                    spec.allow_read.push(source);
                }
            }
            if let Some(output) = output {
                if output.glob {
                    return None;
                }
                spec.allow_write.push(path(&input.cwd, &output.text)?);
            }
            "Bash expands the pattern at execution. The requested read scope covers its containing directory."
        }
        "tar" | "/bin/tar" | "/usr/bin/tar" => {
            archive(args, &input.cwd, &mut spec)?;
            "Archive input trees are read-only. Creation requests the output directory where specified; PDP must authorize the requested scope."
        }
        _ => return None,
    };
    let mut caveats = vec![scope.to_owned()];
    let policy = if program.text.starts_with('/') {
        spec.allow_exec.push(program.text.clone());
        spec.render_cedar().ok()?
    } else {
        caveats.push(format!(
            "PATH determines the file.exec target for {} at execution.",
            program.text
        ));
        format!(
            "{}\n{}",
            spec.render_cedar().ok()?,
            spawn_rule(&program.text)
        )
    };
    Some((policy, caveats))
}

/// GNU tar resolves the archive path against the invocation cwd. Positional -C applies only to
/// following input operands. Unrecognized flags may change that contract, so defer the whole call.
fn archive(args: &[Word], cwd: &str, spec: &mut IntentSpec) -> Option<()> {
    if args.iter().any(|word| word.glob || word.text == ">") {
        return None;
    }
    let mut args = args.iter().map(|word| word.text.as_str());
    let mut base = None;
    let mut output = None;
    let mut create = false;
    let mut options = true;
    while let Some(arg) = args.next() {
        if options && arg == "--" {
            options = false;
            continue;
        }
        if options && arg.starts_with('-') {
            let file = match arg {
                "--create" | "-c" | "-cz" | "-zc" => {
                    create = true;
                    continue;
                }
                "--gzip" | "-z" => continue,
                "-C" | "--directory" => {
                    base = Some(path(base.as_deref().unwrap_or(cwd), args.next()?)?);
                    continue;
                }
                "-cf" | "-czf" | "-zcf" => {
                    create = true;
                    args.next()?
                }
                "-f" | "--file" => args.next()?,
                _ => arg.strip_prefix("--file=")?,
            };
            if output.is_some()
                || file.contains(':')
                || file.ends_with('/')
                || file.rsplit('/').next() == Some(".")
            {
                return None;
            }
            output = Some(path(cwd, file)?);
        } else if arg == "." {
            let directory = base.as_deref()?;
            spec.allow_read
                .push(format!("{}/*", directory.trim_end_matches('/')));
        } else {
            // A named operand may be a file. Its spelling cannot justify a subtree grant.
            return None;
        }
    }
    let output = output?;
    if !create || spec.allow_read.is_empty() {
        return None;
    }
    if Path::new(cwd).starts_with(&output) {
        return None;
    }
    let parent = Path::new(&output).parent()?.to_str()?;
    // A directory write must not cover the working directory or any of its ancestors.
    let write_scope = if Path::new(cwd).starts_with(parent) {
        output
    } else {
        format!("{parent}/*")
    };
    spec.allow_write.push(write_scope);
    Some(())
}

fn path(cwd: &str, value: &str) -> Option<String> {
    if value.is_empty() || value.starts_with('-') || value == ">" {
        return None;
    }
    let full = Path::new(cwd).join(value);
    // Removing a parent component could cross a symbolic link; there is no filesystem lookup here.
    if full.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    full.components()
        .collect::<std::path::PathBuf>()
        .to_str()
        .map(str::to_owned)
}

/// Tokenizes literal words, quoted/escaped whitespace, basename globs and stdout redirection.
/// Expansion, compound commands, descriptor syntax and mixed literal/pattern stars are deferred.
fn words(command: &str) -> Option<Vec<Word>> {
    let mut words = Vec::new();
    let mut current = Word::default();
    let mut active = false;
    let mut quote = None;
    let mut chars = command.chars();
    while let Some(ch) = chars.next() {
        if ch.is_control() && ch != '\t' {
            return None;
        }
        if quote == Some(ch) {
            quote = None;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            let literal = chars.next()?;
            if !matches!(literal, ' ' | '\'' | '"' | '\\')
                || quote == Some('"') && matches!(literal, ' ' | '\'')
            {
                return None;
            }
            current.text.push(literal);
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if quote.is_none() && matches!(ch, ' ' | '\t' | '>') {
            if ch == '>' && active && current.text.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            if active {
                words.push(std::mem::take(&mut current));
                active = false;
            }
            if ch == '>' {
                words.push(Word {
                    text: ">".into(),
                    glob: false,
                });
            }
            continue;
        } else {
            if matches!(
                ch,
                '$' | '`'
                    | '~'
                    | '#'
                    | '&'
                    | '|'
                    | ';'
                    | '('
                    | ')'
                    | '<'
                    | '{'
                    | '}'
                    | '!'
                    | '>'
                    | '['
                    | ']'
            ) {
                return None;
            }
            if matches!(ch, '*' | '?') {
                if quote.is_some() {
                    return None;
                }
                current.glob = true;
            }
            current.text.push(ch);
        }
        active = true;
    }
    if quote.is_some() {
        return None;
    }
    if active {
        words.push(current);
    }
    Some(words)
}
