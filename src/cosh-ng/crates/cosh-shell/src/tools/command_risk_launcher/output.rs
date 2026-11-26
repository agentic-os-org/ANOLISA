use super::{basename, scan_launcher_options, ChainStep, LAUNCHERS};

pub(in crate::tools) fn launcher_runs_login_shell(tokens: &[String]) -> bool {
    let mut index = 0;
    while let Some(program) = tokens.get(index) {
        let Some((_, spec)) = LAUNCHERS
            .iter()
            .find(|(name, _)| *name == basename(program))
        else {
            break;
        };
        let ChainStep::Next(next) = scan_launcher_options(tokens, index + 1, spec) else {
            break;
        };
        if basename(program) == "sudo" {
            let mut option = index + 1;
            while option < next {
                let token = &tokens[option];
                if token == "--" {
                    break;
                }
                let name = token.split('=').next().unwrap_or(token);
                if spec.value_flags.contains(&name) {
                    option += if token.contains('=') { 1 } else { 2 };
                    continue;
                }
                if spec
                    .value_flags
                    .iter()
                    .any(|flag| token.starts_with(flag) && token.len() > flag.len())
                {
                    option += 1;
                    continue;
                }
                if matches!(name, "--login" | "--shell")
                    || token.strip_prefix('-').is_some_and(|flags| {
                        !flags.starts_with('-')
                            && flags.chars().any(|flag| matches!(flag, 'i' | 's'))
                    })
                {
                    return true;
                }
                option += 1;
            }
        }
        index = next;
    }
    false
}

pub(in crate::tools) fn env_outputs_values(args: &[String]) -> bool {
    let Some((_, spec)) = LAUNCHERS.iter().find(|(name, _)| *name == "env") else {
        return false;
    };
    let mut tokens = args.to_vec();
    tokens.push("__output_probe__".to_string());
    matches!(scan_launcher_options(&tokens, 0, spec), ChainStep::Next(next) if next == args.len())
}
