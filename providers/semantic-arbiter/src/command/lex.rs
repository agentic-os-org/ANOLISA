//! Quote-, heredoc- and operator-aware splitting of one command line.

use super::{Token, Word, OPERATORS};

/// Splits a command line into words and operators.
///
/// Returns `None` for unterminated quoting and reports whether a command or
/// process substitution appears outside single quotes.
pub(super) fn lex(input: &str) -> Option<(Vec<Token>, bool)> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = vec![];
    let mut substitution = false;
    let mut heredocs: Vec<(String, bool, bool)> = vec![];
    let mut word: Option<Word> = None;
    let mut index = 0;
    fn finish(word: &mut Option<Word>, tokens: &mut Vec<Token>) {
        if let Some(word) = word.take() {
            tokens.push(Token::Word(word));
        }
    }
    fn current(word: &mut Option<Word>) -> &mut Word {
        word.get_or_insert_with(|| Word {
            text: String::new(),
            dynamic: false,
            quoted: false,
        })
    }
    while index < chars.len() {
        let c = chars[index];
        match c {
            ' ' | '\t' => {
                finish(&mut word, &mut tokens);
                index += 1;
            }
            '\n' => {
                finish(&mut word, &mut tokens);
                tokens.push(Token::Op(";"));
                index += 1;
                for (delimiter, strip, quoted) in heredocs.drain(..) {
                    loop {
                        if index >= chars.len() {
                            break;
                        }
                        let end = chars[index..]
                            .iter()
                            .position(|&c| c == '\n')
                            .map_or(chars.len(), |n| index + n);
                        let line: String = chars[index..end].iter().collect();
                        index = (end + 1).min(chars.len());
                        let line = if strip {
                            line.trim_start_matches('\t')
                        } else {
                            &line
                        };
                        if line == delimiter {
                            break;
                        }
                        if !quoted && (line.contains("$(") || line.contains('`')) {
                            substitution = true;
                        }
                    }
                }
            }
            '#' if word.is_none() => {
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
            }
            '\'' => {
                let end = chars[index + 1..].iter().position(|&c| c == '\'')? + index + 1;
                let target = current(&mut word);
                target.text.extend(&chars[index + 1..end]);
                target.quoted = true;
                index = end + 1;
            }
            '"' => {
                index += 1;
                let target = current(&mut word);
                target.quoted = true;
                loop {
                    let c = *chars.get(index)?;
                    index += 1;
                    match c {
                        '"' => break,
                        '\\' => {
                            let next = *chars.get(index)?;
                            index += 1;
                            if !"\"\\$`\n".contains(next) {
                                target.text.push('\\');
                            }
                            if next != '\n' {
                                target.text.push(next);
                            }
                        }
                        '`' => substitution = true,
                        '$' => {
                            if chars.get(index) == Some(&'(') {
                                substitution = true;
                            }
                            target.dynamic = true;
                            target.text.push('$');
                        }
                        other => target.text.push(other),
                    }
                }
            }
            '\\' => {
                index += 1;
                match chars.get(index) {
                    Some('\n') => {}
                    Some(&next) => {
                        let target = current(&mut word);
                        target.text.push(next);
                        target.quoted = true;
                    }
                    None => return None,
                }
                index += 1;
            }
            '`' => {
                substitution = true;
                current(&mut word).dynamic = true;
                index += 1;
            }
            '$' => {
                if chars.get(index + 1) == Some(&'(') {
                    substitution = true;
                }
                let target = current(&mut word);
                target.dynamic = true;
                target.text.push('$');
                index += 1;
            }
            '<' | '>' if chars.get(index + 1) == Some(&'(') => {
                substitution = true;
                finish(&mut word, &mut tokens);
                index += 1;
            }
            _ if "<>&|;()".contains(c) => {
                // A bare numeric word glued to a redirection is a descriptor, not an argument.
                if "<>".contains(c)
                    && word
                        .as_ref()
                        .is_some_and(|w| !w.quoted && w.text.bytes().all(|b| b.is_ascii_digit()))
                {
                    word = None;
                }
                finish(&mut word, &mut tokens);
                let rest: String = chars[index..chars.len().min(index + 3)].iter().collect();
                let op = *OPERATORS.iter().find(|op| rest.starts_with(**op))?;
                index += op.len();
                tokens.push(Token::Op(op));
                if matches!(op, "<<" | "<<-") {
                    while chars.get(index).is_some_and(|c| *c == ' ' || *c == '\t') {
                        index += 1;
                    }
                    let start = index;
                    while chars
                        .get(index)
                        .is_some_and(|c| !" \t\n;&|<>()".contains(*c))
                    {
                        index += 1;
                    }
                    let raw: String = chars[start..index].iter().collect();
                    let quoted = raw.contains(['\'', '"', '\\']);
                    let delimiter: String = raw.chars().filter(|c| !"'\"\\".contains(*c)).collect();
                    if delimiter.is_empty() {
                        return None;
                    }
                    tokens.push(Token::Word(Word {
                        text: delimiter.clone(),
                        dynamic: false,
                        quoted,
                    }));
                    heredocs.push((delimiter, op == "<<-", quoted));
                }
            }
            _ => {
                let start = word.is_none();
                let target = current(&mut word);
                if "*?[{".contains(c) || (c == '~' && start) {
                    target.dynamic = true;
                }
                target.text.push(c);
                index += 1;
            }
        }
    }
    finish(&mut word, &mut tokens);
    Some((tokens, substitution))
}
