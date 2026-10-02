//! Conservative templates used only by the anchored Generic Log fallback.

pub(crate) fn mask(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut digits = false;
    for character in line.chars() {
        if character.is_ascii_digit() {
            if !digits {
                output.push('0');
                digits = true;
            }
        } else {
            digits = false;
            output.push(character);
        }
    }
    output
}

pub(crate) fn generic_progress_template(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.bytes().any(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    [
        "progress",
        "processing",
        "building",
        "compiling",
        "downloading",
        "running",
        "checking",
        "completed",
    ]
    .iter()
    .any(|signal| lower.contains(signal))
    .then(|| mask(trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("../tests/template_tests.rs");
}
