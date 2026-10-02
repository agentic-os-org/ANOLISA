//! Bounded CLI input and terminal-safe output helpers.

use super::*;

fn open_readonly_nofollow(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    options.open(path)
}

pub(super) fn read_prompt(path: Option<&PathBuf>) -> Result<String, CliError> {
    let mut input: Box<dyn Read> = match path {
        Some(path) => {
            let file = open_readonly_nofollow(path).map_err(CliError::Input)?;
            if !file.metadata().map_err(CliError::Input)?.is_file() {
                return Err(CliError::PromptNotRegular(path.clone()));
            }
            Box::new(file)
        }
        None => {
            if io::stdin().is_terminal() {
                eprintln!("Enter prompt, then press Ctrl-D:");
            }
            Box::new(io::stdin())
        }
    };
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take((MAX_PROMPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(CliError::Input)?;
    if bytes.len() > MAX_PROMPT_BYTES {
        return Err(CliError::PromptTooLarge);
    }
    let prompt = String::from_utf8(bytes)
        .map_err(|error| CliError::Input(io::Error::new(io::ErrorKind::InvalidData, error)))?;
    if prompt.trim().is_empty() {
        return Err(CliError::EmptyPrompt);
    }
    Ok(prompt)
}

pub(super) fn read_intent(path: Option<&PathBuf>) -> Result<String, CliError> {
    let mut input: Box<dyn Read> = match path {
        Some(path) => {
            let file = open_readonly_nofollow(path).map_err(CliError::IntentInput)?;
            if !file.metadata().map_err(CliError::IntentInput)?.is_file() {
                return Err(CliError::IntentNotRegular(path.clone()));
            }
            Box::new(file)
        }
        None => {
            if io::stdin().is_terminal() {
                eprintln!("Enter Task intent, then press Ctrl-D:");
            }
            Box::new(io::stdin())
        }
    };
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take((MAX_PROMPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(CliError::IntentInput)?;
    if bytes.len() > MAX_PROMPT_BYTES {
        return Err(CliError::IntentTooLarge);
    }
    let intent = String::from_utf8(bytes).map_err(|error| {
        CliError::IntentInput(io::Error::new(io::ErrorKind::InvalidData, error))
    })?;
    if intent.trim().is_empty() {
        return Err(CliError::EmptyIntent);
    }
    Ok(intent)
}

pub(super) fn terminal_safe(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '\n' | '\t' => vec![character],
            character if character.is_control() => character.escape_default().collect(),
            character => vec![character],
        })
        .collect()
}
