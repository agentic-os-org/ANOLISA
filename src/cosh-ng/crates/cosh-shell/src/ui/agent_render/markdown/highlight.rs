use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

/// Lexical token classes used for code block syntax highlighting.
///
/// The tokenizer is intentionally line-based and lexical-only: it colors
/// keywords, strings, comments, numbers and call sites for a curated
/// language set. Anything it cannot classify stays unstyled, and unknown
/// languages fall back to raw text, so the worst failure mode is a token
/// without color — never corrupted output (issue #1751).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Keyword,
    StringLit,
    Comment,
    Number,
    Call,
    Plain,
}

struct Token {
    text: String,
    kind: TokenKind,
}

struct LanguageProfile {
    keywords: &'static [&'static str],
    line_comments: &'static [&'static str],
    string_delimiters: &'static [char],
    case_insensitive_keywords: bool,
}

const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "case", "class",
    "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if",
    "import", "in", "is", "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return",
    "self", "try", "while", "with", "yield",
];

const BASH_KEYWORDS: &[&str] = &[
    "alias", "break", "case", "continue", "declare", "do", "done", "elif", "else", "esac", "eval",
    "exec", "exit", "export", "fi", "for", "function", "if", "in", "local", "readonly", "return",
    "select", "set", "shift", "source", "then", "time", "trap", "unset", "until", "while",
];

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

const JS_KEYWORDS: &[&str] = &[
    "abstract",
    "any",
    "async",
    "await",
    "boolean",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "keyof",
    "let",
    "namespace",
    "never",
    "new",
    "null",
    "number",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "static",
    "string",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "unknown",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "false",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "nil",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "true",
    "type",
    "var",
];

const C_KEYWORDS: &[&str] = &[
    "auto", "bool", "break", "case", "char", "const", "continue", "default", "do", "double",
    "else", "enum", "extern", "false", "float", "for", "goto", "if", "inline", "int", "long",
    "register", "restrict", "return", "short", "signed", "sizeof", "static", "struct", "switch",
    "true", "typedef", "union", "unsigned", "void", "volatile", "while",
];

// C++ keeps its own superset so C code blocks do not color C++-only
// identifiers such as `class` or `namespace`.
const CPP_KEYWORDS: &[&str] = &[
    "auto",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "constexpr",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "explicit",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "nullptr",
    "operator",
    "private",
    "protected",
    "public",
    "register",
    "restrict",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "template",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
];

const JSON_KEYWORDS: &[&str] = &["false", "null", "true"];

const YAML_KEYWORDS: &[&str] = &["false", "no", "null", "off", "on", "true", "yes"];

const TOML_KEYWORDS: &[&str] = &["false", "true"];

const SQL_KEYWORDS: &[&str] = &[
    "all",
    "alter",
    "and",
    "as",
    "between",
    "by",
    "create",
    "default",
    "delete",
    "distinct",
    "drop",
    "exists",
    "foreign",
    "from",
    "group",
    "having",
    "in",
    "index",
    "inner",
    "insert",
    "into",
    "is",
    "join",
    "key",
    "left",
    "like",
    "limit",
    "not",
    "null",
    "offset",
    "on",
    "or",
    "order",
    "outer",
    "primary",
    "references",
    "right",
    "select",
    "set",
    "table",
    "union",
    "update",
    "values",
    "view",
    "where",
];

const HASH_COMMENT: &[&str] = &["#"];
const SLASH_COMMENT: &[&str] = &["//"];
const SQL_COMMENT: &[&str] = &["--"];
const NO_COMMENT: &[&str] = &[];

const QUOTES: &[char] = &['"', '\''];
const QUOTES_WITH_BACKTICK: &[char] = &['"', '\'', '`'];
const DOUBLE_QUOTE_ONLY: &[char] = &['"'];

fn profile_for(language: &str) -> Option<&'static LanguageProfile> {
    static PYTHON: LanguageProfile = LanguageProfile {
        keywords: PYTHON_KEYWORDS,
        line_comments: HASH_COMMENT,
        string_delimiters: QUOTES,
        case_insensitive_keywords: false,
    };
    static BASH: LanguageProfile = LanguageProfile {
        keywords: BASH_KEYWORDS,
        line_comments: HASH_COMMENT,
        string_delimiters: QUOTES_WITH_BACKTICK,
        case_insensitive_keywords: false,
    };
    static RUST: LanguageProfile = LanguageProfile {
        keywords: RUST_KEYWORDS,
        line_comments: SLASH_COMMENT,
        // Single quotes are skipped: they collide with lifetimes ('a).
        string_delimiters: DOUBLE_QUOTE_ONLY,
        case_insensitive_keywords: false,
    };
    static JS: LanguageProfile = LanguageProfile {
        keywords: JS_KEYWORDS,
        line_comments: SLASH_COMMENT,
        string_delimiters: QUOTES_WITH_BACKTICK,
        case_insensitive_keywords: false,
    };
    static GO: LanguageProfile = LanguageProfile {
        keywords: GO_KEYWORDS,
        line_comments: SLASH_COMMENT,
        string_delimiters: QUOTES_WITH_BACKTICK,
        case_insensitive_keywords: false,
    };
    static C: LanguageProfile = LanguageProfile {
        keywords: C_KEYWORDS,
        line_comments: SLASH_COMMENT,
        string_delimiters: QUOTES,
        case_insensitive_keywords: false,
    };
    static CPP: LanguageProfile = LanguageProfile {
        keywords: CPP_KEYWORDS,
        line_comments: SLASH_COMMENT,
        string_delimiters: QUOTES,
        case_insensitive_keywords: false,
    };
    static JSON: LanguageProfile = LanguageProfile {
        keywords: JSON_KEYWORDS,
        line_comments: NO_COMMENT,
        string_delimiters: DOUBLE_QUOTE_ONLY,
        case_insensitive_keywords: false,
    };
    static YAML: LanguageProfile = LanguageProfile {
        keywords: YAML_KEYWORDS,
        line_comments: HASH_COMMENT,
        string_delimiters: QUOTES,
        case_insensitive_keywords: false,
    };
    static TOML: LanguageProfile = LanguageProfile {
        keywords: TOML_KEYWORDS,
        line_comments: HASH_COMMENT,
        string_delimiters: QUOTES,
        case_insensitive_keywords: false,
    };
    static SQL: LanguageProfile = LanguageProfile {
        keywords: SQL_KEYWORDS,
        line_comments: SQL_COMMENT,
        string_delimiters: QUOTES,
        case_insensitive_keywords: true,
    };

    match language.trim().to_ascii_lowercase().as_str() {
        "python" | "python3" | "py" => Some(&PYTHON),
        "bash" | "sh" | "shell" | "zsh" => Some(&BASH),
        "rust" | "rs" => Some(&RUST),
        "javascript" | "js" | "jsx" | "typescript" | "ts" | "tsx" => Some(&JS),
        "go" | "golang" => Some(&GO),
        "c" | "h" => Some(&C),
        "cpp" | "c++" | "cc" | "cxx" | "hpp" => Some(&CPP),
        "json" => Some(&JSON),
        "yaml" | "yml" => Some(&YAML),
        "toml" => Some(&TOML),
        "sql" => Some(&SQL),
        _ => None,
    }
}

/// Convert one pre-wrapped code line into styled spans. Unknown languages
/// return the line as a single raw span (current behavior preserved).
pub(super) fn styled_code_spans(language: &str, line: &str) -> Vec<Span<'static>> {
    let Some(profile) = profile_for(language) else {
        return vec![Span::raw(line.to_string())];
    };
    tokenize(profile, line)
        .into_iter()
        .map(|token| match style_for_token(token.kind) {
            Some(style) => Span::styled(token.text, style),
            None => Span::raw(token.text),
        })
        .collect()
}

fn style_for_token(kind: TokenKind) -> Option<Style> {
    match kind {
        TokenKind::Keyword => Some(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        TokenKind::StringLit => Some(Style::default().fg(Color::Green)),
        TokenKind::Comment => Some(Style::default().fg(Color::DarkGray)),
        TokenKind::Number => Some(Style::default().fg(Color::Magenta)),
        TokenKind::Call => Some(Style::default().fg(Color::Yellow)),
        TokenKind::Plain => None,
    }
}

fn tokenize(profile: &LanguageProfile, line: &str) -> Vec<Token> {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut plain_run = String::new();
    let mut idx = 0;

    while idx < chars.len() {
        if comment_starts_at(profile, &chars, idx) {
            flush_plain(&mut tokens, &mut plain_run);
            tokens.push(Token {
                text: chars[idx..].iter().collect(),
                kind: TokenKind::Comment,
            });
            break;
        }

        let ch = chars[idx];
        if profile.string_delimiters.contains(&ch) {
            flush_plain(&mut tokens, &mut plain_run);
            let end = string_end(&chars, idx, ch);
            tokens.push(Token {
                text: chars[idx..end].iter().collect(),
                kind: TokenKind::StringLit,
            });
            idx = end;
            continue;
        }

        if ch.is_ascii_digit() {
            flush_plain(&mut tokens, &mut plain_run);
            let end = number_end(&chars, idx);
            tokens.push(Token {
                text: chars[idx..end].iter().collect(),
                kind: TokenKind::Number,
            });
            idx = end;
            continue;
        }

        if ch == '_' || ch.is_alphabetic() {
            flush_plain(&mut tokens, &mut plain_run);
            let end = identifier_end(&chars, idx);
            let text: String = chars[idx..end].iter().collect();
            let kind = identifier_kind(profile, &chars, end, &text);
            tokens.push(Token { text, kind });
            idx = end;
            continue;
        }

        plain_run.push(ch);
        idx += 1;
    }

    flush_plain(&mut tokens, &mut plain_run);
    tokens
}

fn flush_plain(tokens: &mut Vec<Token>, plain_run: &mut String) {
    if plain_run.is_empty() {
        return;
    }
    tokens.push(Token {
        text: std::mem::take(plain_run),
        kind: TokenKind::Plain,
    });
}

fn comment_starts_at(profile: &LanguageProfile, chars: &[char], idx: usize) -> bool {
    profile.line_comments.iter().any(|marker| {
        marker
            .chars()
            .enumerate()
            .all(|(offset, expected)| chars.get(idx + offset) == Some(&expected))
    })
}

fn string_end(chars: &[char], start: usize, delimiter: char) -> usize {
    let mut idx = start + 1;
    while idx < chars.len() {
        if chars[idx] == '\\' {
            idx += 2;
            continue;
        }
        if chars[idx] == delimiter {
            return idx + 1;
        }
        idx += 1;
    }
    chars.len()
}

fn number_end(chars: &[char], start: usize) -> usize {
    let mut idx = start + 1;
    while idx < chars.len() {
        let ch = chars[idx];
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
            idx += 1;
        } else {
            break;
        }
    }
    idx
}

fn identifier_end(chars: &[char], start: usize) -> usize {
    let mut idx = start + 1;
    while idx < chars.len() {
        let ch = chars[idx];
        if ch == '_' || ch.is_alphanumeric() {
            idx += 1;
        } else {
            break;
        }
    }
    idx
}

fn identifier_kind(profile: &LanguageProfile, chars: &[char], end: usize, text: &str) -> TokenKind {
    let is_keyword = if profile.case_insensitive_keywords {
        let lowered = text.to_ascii_lowercase();
        profile.keywords.contains(&lowered.as_str())
    } else {
        profile.keywords.contains(&text)
    };
    if is_keyword {
        return TokenKind::Keyword;
    }
    if chars.get(end) == Some(&'(') {
        return TokenKind::Call;
    }
    TokenKind::Plain
}
