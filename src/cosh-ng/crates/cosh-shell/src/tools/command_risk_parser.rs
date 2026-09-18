use super::command_risk::CommandShape;

/// Non-persistent output-suppression sink allowlist (issue #1667
/// implementation boundaries): a `[N]>` / `[N]>>` redirection is treated
/// as output suppression instead of a filesystem write only when the
/// target is an unquoted, non-expanded literal from this table, or a
/// whole-word quoted form of such a literal (`2>'/dev/null'`,
/// `2>"/dev/null"`, issue #1752 — quotes around a sink entry cannot
/// introduce expansion because the entries contain no `$`, backtick or
/// backslash). The fd words `[N]>&1` / `[N]>&2` (duplication onto the
/// conventional output targets) and `[N]>&-` (close) are exempted by
/// policy as descriptor operations, not filesystem writes; see the fd
/// word probe in `parse_command` for the policy rationale (issue #2054,
/// spec `shell-fd-dup-redirection-risk`). Every other form (regular
/// files, expanded or partially quoted targets, other numeric targets,
/// `&>`, `>&file`, bash's move form `[N]>&M-`) keeps the fail-closed
/// RedirectionWrite high-risk path. Extending this table requires
/// revisiting the issue #1667 boundaries and the decision-matrix tests
/// in `command_risk_tests.rs`.
const SAFE_OUTPUT_SINKS: &[&str] = &["/dev/null"];

/// Output suppression plus routing that cannot be reproduced by a stderr-null spawn.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct NullRedirections {
    count: usize,
    other_routing: bool,
}

impl NullRedirections {
    pub(super) fn is_empty(self) -> bool {
        self.count == 0
    }

    pub(super) fn is_stderr_only(self) -> bool {
        !self.is_empty() && !self.other_routing
    }

    fn record_sink(&mut self, stderr: bool) {
        self.count += 1;
        self.other_routing |= !stderr;
    }
}

/// Segment separator kind recorded at each `&&`/`||`/`;`/newline break.
/// A single `&` also records a mark (background list separator) but the
/// shape escalates to Complex, so its connector is never consumed by the
/// compound path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegmentConnector {
    Seq,
    And,
    Or,
}

#[derive(Debug, Clone)]
pub(super) struct ParsedCommand {
    pub(super) shape: CommandShape,
    pub(super) stages: Vec<Vec<String>>,
    /// Quote-aware expansion markers that a direct argv spawn cannot reproduce.
    pub(super) requires_shell_expansion: bool,
    pub(super) null_redirections: NullRedirections,
    /// Command segments split at `&&`, `||`, `;`, and newlines; each
    /// segment holds its own pipeline stages. Only populated when the
    /// command contains segment separators (used by the stripped-compound
    /// aggregation path).
    pub(super) segments: Vec<Vec<Vec<String>>>,
    /// Connector kind for every recorded segment mark, in source order.
    /// When no empty segment was swallowed, entry `i` is the connector
    /// between `segments[i]` and `segments[i + 1]`; consumers must verify
    /// `segment_connectors.len() == segments.len() - 1` before relying on
    /// that pairing (a trailing separator leaves a dangling mark).
    pub(super) segment_connectors: Vec<SegmentConnector>,
}

pub(super) fn parse_command(command: &str) -> ParsedCommand {
    if command.is_empty() {
        return ParsedCommand {
            shape: CommandShape::Empty,
            stages: Vec::new(),
            requires_shell_expansion: false,
            null_redirections: NullRedirections::default(),
            segments: Vec::new(),
            segment_connectors: Vec::new(),
        };
    }
    if command.contains('\0') {
        return ParsedCommand {
            shape: CommandShape::Unparseable,
            stages: Vec::new(),
            requires_shell_expansion: false,
            null_redirections: NullRedirections::default(),
            segments: Vec::new(),
            segment_connectors: Vec::new(),
        };
    }

    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut stages: Vec<Vec<String>> = Vec::new();
    let mut shape = CommandShape::Simple;
    let mut quote: Option<char> = None;
    let mut dangling_escape = false;
    let mut requires_shell_expansion = false;
    let mut null_redirections = NullRedirections::default();
    let mut amp_redirect_guard = false;
    // Segment breaks recorded as (stage index, token offset, connector)
    // at each `&&`/`||`/`;`/newline, resolved into `segments` after
    // parsing.
    let mut segment_marks: Vec<(usize, usize, SegmentConnector)> = Vec::new();
    // Tracks whether the current token buffer contains quoted or escaped
    // content; such tokens are ordinary arguments and never fd prefixes.
    let mut token_quoted = false;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        if let Some(quote_ch) = quote {
            if ch == quote_ch {
                quote = None;
            } else {
                requires_shell_expansion |= quote_ch == '"' && ch == '!';
                token.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                token_quoted = true;
            }
            ' ' | '\t' => push_token(&mut tokens, &mut token, &mut token_quoted),
            '\n' | ';' => {
                push_token(&mut tokens, &mut token, &mut token_quoted);
                segment_marks.push((stages.len(), tokens.len(), SegmentConnector::Seq));
                shape = max_shape(shape, CommandShape::Sequence);
            }
            '|' => {
                push_token(&mut tokens, &mut token, &mut token_quoted);
                if chars.peek().is_some_and(|next| *next == '|') {
                    chars.next();
                    segment_marks.push((stages.len(), tokens.len(), SegmentConnector::Or));
                    shape = max_shape(shape, CommandShape::AndOrList);
                } else {
                    stages.push(std::mem::take(&mut tokens));
                    shape = max_shape(shape, CommandShape::Pipeline);
                }
            }
            '&' => {
                push_token(&mut tokens, &mut token, &mut token_quoted);
                if chars.peek().is_some_and(|next| *next == '&') {
                    chars.next();
                    segment_marks.push((stages.len(), tokens.len(), SegmentConnector::And));
                    shape = max_shape(shape, CommandShape::AndOrList);
                } else {
                    shape = max_shape(shape, CommandShape::Complex);
                    // `&>` redirects both stdout and stderr; it is not a
                    // `[N]>` form, keep the existing high-risk path.
                    amp_redirect_guard = chars.peek().is_some_and(|next| *next == '>');
                    // A single `&` is the background list separator, so
                    // record a segment mark like `;`/newline/`&&`/`||`
                    // (issue #1785 review). A terminal `&` leaves an
                    // empty tail that the segment builder drops, so
                    // `ls &` still yields a single segment.
                    if !amp_redirect_guard {
                        // Shape already escalated to Complex, so this
                        // connector is never consumed by the compound
                        // path; the kind is irrelevant, pick Seq.
                        segment_marks.push((stages.len(), tokens.len(), SegmentConnector::Seq));
                    }
                }
            }
            '>' => {
                let guarded = amp_redirect_guard;
                amp_redirect_guard = false;
                // Shell only treats an unquoted, unescaped whole-numeric
                // token adjacent to `>` as an IO_NUMBER fd prefix (the `2`
                // in `2>`). Any other pending token is an ordinary word
                // that belongs to the command, and the redirection itself
                // uses the default stdout fd (`ls>/dev/null`,
                // `echo "2">/dev/null`).
                let fd_candidate = !token.is_empty()
                    && !token_quoted
                    && token.bytes().all(|byte| byte.is_ascii_digit());
                if !fd_candidate {
                    push_token(&mut tokens, &mut token, &mut token_quoted);
                }
                // Fd word probe (issue #2054, spec
                // `shell-fd-dup-redirection-risk`). Policy: exempt only
                // the conventional output targets — `[N]>&1` / `[N]>&2`
                // duplication and `[N]>&-` close. Rebinding fd 1/2 to a
                // file (`exec 2>out`) is possible in the persistent
                // foreground shell, but that rebinding command is itself
                // fail-closed High (user-approved first), and once
                // rebound the file receives output from commands with no
                // redirection syntax at all, so this lexical classifier
                // cannot defend that state either way. Auxiliary fds
                // (`>&3`, `2>&10`) stay fail-closed: their bindings are
                // only reachable through explicit fd syntax, where
                // lexical rejection is effective. The source prefix must
                // be a single digit (zsh treats only a lone digit as an
                // fd; `tee 10>&1` passes `10` as an argument), and
                // bash's move form `[N]>&M-` is excluded (zsh parses it
                // as a redirection to a file named `M-`). Non-consuming
                // and fail-closed: every rejected form falls through
                // byte-for-byte to the pre-existing path. Full trade-off
                // record: spec `shell-fd-dup-redirection-risk`
                // design.md (matrix + invariant I0).
                let single_digit_prefix = !fd_candidate || token.len() == 1;
                if !guarded && single_digit_prefix && chars.peek().is_some_and(|next| *next == '&')
                {
                    let mut dup_lookahead = chars.clone();
                    dup_lookahead.next();
                    let mut dup_consumed = 1usize;
                    let mut target_digit: Option<char> = None;
                    if dup_lookahead
                        .peek()
                        .is_some_and(|next| next.is_ascii_digit())
                    {
                        target_digit = dup_lookahead.next();
                        dup_consumed += 1;
                    }
                    // Only fd 1 and fd 2 are safe duplication targets; a
                    // second digit (`2>&10`) or any other digit (`>&3`)
                    // rejects the elision.
                    let has_digit = matches!(target_digit, Some('1') | Some('2'))
                        && !dup_lookahead
                            .peek()
                            .is_some_and(|next| next.is_ascii_digit());
                    let mut has_dash = false;
                    if target_digit.is_none()
                        && dup_lookahead.peek().is_some_and(|next| *next == '-')
                    {
                        dup_lookahead.next();
                        dup_consumed += 1;
                        has_dash = true;
                    }
                    // Keep the boundary check explicit next to the parsed span.
                    #[allow(clippy::unnecessary_map_or)]
                    let boundary_ok = dup_lookahead.peek().map_or(true, |next| {
                        // Word boundary = whitespace or a POSIX operator
                        // character. `{`/`}` are NOT operators in the
                        // redirection-word position: both bash and zsh
                        // parse `: >&1{` as a redirection to a file named
                        // `1{`, so they must reject
                        // the elision and fail closed.
                        matches!(
                            next,
                            ' ' | '\t' | '\n' | ';' | '|' | '&' | '<' | '>' | '(' | ')'
                        )
                    });
                    if (has_digit || has_dash) && boundary_ok {
                        // Descriptor operations depend on ordering and bindings;
                        // a stderr-null argv execution must never erase them.
                        null_redirections.other_routing = true;
                        for _ in 0..dup_consumed {
                            chars.next();
                        }
                        // Close forms (`[N]>&-`) suppress the stream from
                        // the user's point of view only when they close an
                        // output stream: the bare default (stdout), fd 1,
                        // or fd 2. Closing stdin or an auxiliary fd
                        // (`0>&-`, `3>&-`) leaves the visible output
                        // untouched (verified in bash and zsh), so those
                        // stay annotation-free like duplications.
                        let closes_output_stream =
                            has_dash && (!fd_candidate || token == "1" || token == "2");
                        if fd_candidate {
                            // The IO_NUMBER prefix (the `2` in `2>&1`)
                            // belongs to the redirection syntax, not argv.
                            token.clear();
                            token_quoted = false;
                        }
                        // Output-closing forms join the issue #1667
                        // null-sink channel (`output-suppressed` reason +
                        // auto-allow fallback).
                        if closes_output_stream {
                            null_redirections.record_sink(false);
                        }
                        continue;
                    }
                }
                // Non-consuming lookahead: on rejection fall back to a path
                // that is byte-for-byte identical to the pre-fix behavior.
                let mut lookahead = chars.clone();
                let mut consumed = 0usize;
                if lookahead.peek().is_some_and(|next| *next == '>') {
                    lookahead.next();
                    consumed += 1;
                }
                while lookahead
                    .peek()
                    .is_some_and(|next| *next == ' ' || *next == '\t')
                {
                    lookahead.next();
                    consumed += 1;
                }
                // Issue #1752: a target whose whole word is single- or
                // double-quoted (`2>'/dev/null'`, `2>"/dev/null"`) still
                // undergoes quote removal to the literal sink path —
                // quotes cannot introduce expansion for a SAFE_OUTPUT_SINK
                // entry, whose bytes contain no `$`, backtick or backslash
                // — so it joins the null-suppression channel like the
                // unquoted form. Only this exact whole-word form is
                // exempted: the closing quote must end the word (a suffix
                // like `'/dev/null'x` concatenates into a different path,
                // and `{`/`}` are not word boundaries in the
                // redirection-word position) and the dequoted content must
                // match the allowlist byte-for-byte, so expanded
                // (`"$F"`), suffixed, and non-sink quoted targets keep the
                // fail-closed path below byte-for-byte.
                if !guarded {
                    if let Some(&quote_ch) = lookahead.peek() {
                        if matches!(quote_ch, '\'' | '"') {
                            let mut quoted_scan = lookahead.clone();
                            let mut quoted_consumed = 0usize;
                            quoted_scan.next();
                            quoted_consumed += 1;
                            let mut quoted_target = String::new();
                            let mut quote_closed = false;
                            while let Some(&next) = quoted_scan.peek() {
                                if next == quote_ch {
                                    quoted_scan.next();
                                    quoted_consumed += 1;
                                    quote_closed = true;
                                    break;
                                }
                                if matches!(
                                    next,
                                    ' ' | '\t'
                                        | '\n'
                                        | ';'
                                        | '|'
                                        | '&'
                                        | '<'
                                        | '>'
                                        | '('
                                        | ')'
                                        | '{'
                                        | '}'
                                ) {
                                    break;
                                }
                                quoted_target.push(next);
                                quoted_scan.next();
                                quoted_consumed += 1;
                            }
                            // Keep the workspace Rust 1.74 MSRV;
                            // `Option::is_none_or` is newer.
                            #[allow(clippy::unnecessary_map_or)]
                            let word_ends = quote_closed
                                && quoted_scan.peek().map_or(true, |next| {
                                    matches!(
                                        next,
                                        ' ' | '\t' | '\n' | ';' | '|' | '&' | '<' | '>' | '(' | ')'
                                    )
                                });
                            if word_ends && SAFE_OUTPUT_SINKS.contains(&quoted_target.as_str()) {
                                null_redirections.record_sink(fd_candidate && token == "2");
                                if fd_candidate {
                                    token.clear();
                                    token_quoted = false;
                                }
                                for _ in 0..consumed + quoted_consumed {
                                    chars.next();
                                }
                                continue;
                            }
                        }
                    }
                }
                let mut target = String::new();
                let mut literal = true;
                while let Some(&next) = lookahead.peek() {
                    if matches!(
                        next,
                        ' ' | '\t' | '\n' | ';' | '|' | '&' | '<' | '>' | '(' | ')' | '{' | '}'
                    ) {
                        break;
                    }
                    if matches!(next, '\'' | '"' | '`' | '$' | '\\') {
                        literal = false;
                        break;
                    }
                    target.push(next);
                    lookahead.next();
                    consumed += 1;
                }
                if !guarded && literal && SAFE_OUTPUT_SINKS.contains(&target.as_str()) {
                    null_redirections.record_sink(fd_candidate && token == "2");
                    if fd_candidate {
                        token.clear();
                        token_quoted = false;
                    }
                    for _ in 0..consumed {
                        chars.next();
                    }
                } else {
                    if fd_candidate {
                        push_token(&mut tokens, &mut token, &mut token_quoted);
                    }
                    if chars.peek().is_some_and(|next| *next == '>') {
                        chars.next();
                    }
                    shape = max_shape(shape, CommandShape::RedirectionWrite);
                }
            }
            '<' => {
                push_token(&mut tokens, &mut token, &mut token_quoted);
                shape = max_shape(shape, CommandShape::RedirectionRead);
            }
            '`' => {
                push_token(&mut tokens, &mut token, &mut token_quoted);
                shape = max_shape(shape, CommandShape::CommandSubstitution);
            }
            '$' if chars.peek().is_some_and(|next| *next == '(') => {
                push_token(&mut tokens, &mut token, &mut token_quoted);
                chars.next();
                shape = max_shape(shape, CommandShape::CommandSubstitution);
            }
            '(' | ')' | '{' | '}' => {
                push_token(&mut tokens, &mut token, &mut token_quoted);
                shape = max_shape(shape, CommandShape::Complex);
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    token.push(next);
                    token_quoted = true;
                } else {
                    // Bash and zsh disagree at EOF; never execute a truncated argv.
                    dangling_escape = true;
                }
            }
            '*' | '?' | '[' | '~' | '!' | '=' | '^' => {
                // ponytail: leave expansion to the shell until its context is modeled.
                requires_shell_expansion = true;
                token.push(ch);
            }
            _ => token.push(ch),
        }
    }

    if quote.is_some() || dangling_escape {
        return ParsedCommand {
            shape: CommandShape::Unparseable,
            stages: Vec::new(),
            requires_shell_expansion: false,
            null_redirections: NullRedirections::default(),
            segments: Vec::new(),
            segment_connectors: Vec::new(),
        };
    }
    push_token(&mut tokens, &mut token, &mut token_quoted);
    if !tokens.is_empty() {
        stages.push(tokens);
    }
    if matches!(shape, CommandShape::Simple)
        && stages.first().is_some_and(|tokens| {
            tokens
                .iter()
                .take_while(|token| is_env_assignment(token))
                .count()
                > 0
        })
    {
        shape = CommandShape::EnvSimple;
    }

    ParsedCommand {
        shape,
        segments: split_segments(&stages, &segment_marks),
        segment_connectors: segment_marks
            .iter()
            .map(|&(_, _, connector)| connector)
            .collect(),
        stages,
        requires_shell_expansion,
        null_redirections,
    }
}

/// Resolves the recorded segment marks into per-segment pipeline stages.
/// Consecutive stages between two marks belong to the same segment; a mark
/// splits the stage it points into at the recorded token offset.
fn split_segments(
    stages: &[Vec<String>],
    marks: &[(usize, usize, SegmentConnector)],
) -> Vec<Vec<Vec<String>>> {
    if marks.is_empty() {
        return Vec::new();
    }
    let mut segments: Vec<Vec<Vec<String>>> = Vec::new();
    let mut current: Vec<Vec<String>> = Vec::new();
    let mut mark_iter = marks.iter().peekable();
    for (stage_index, stage) in stages.iter().enumerate() {
        let mut start = 0usize;
        while let Some(&&(mark_stage, mark_offset, _)) = mark_iter.peek() {
            if mark_stage != stage_index {
                break;
            }
            mark_iter.next();
            let part = &stage[start..mark_offset.min(stage.len())];
            if !part.is_empty() {
                current.push(part.to_vec());
            }
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
            start = mark_offset.min(stage.len());
        }
        let rest = &stage[start..];
        if !rest.is_empty() {
            current.push(rest.to_vec());
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

pub(super) fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !name.bytes().next().unwrap_or_default().is_ascii_digit()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn push_token(tokens: &mut Vec<String>, token: &mut String, token_quoted: &mut bool) {
    // An explicitly quoted empty string (`''` / `""`) is a real argv entry
    // in every shell (`ls ''` passes an empty argument and fails); dropping
    // it would break the assessed-argv == executed-argv invariant.
    if !token.is_empty() || *token_quoted {
        tokens.push(std::mem::take(token));
    }
    *token_quoted = false;
}

fn max_shape(current: CommandShape, next: CommandShape) -> CommandShape {
    use CommandShape::*;
    let rank = |shape| match shape {
        Empty => 0,
        Simple | EnvSimple => 1,
        Pipeline => 2,
        AndOrList | Sequence | RedirectionRead => 3,
        Complex => 4,
        RedirectionWrite => 5,
        CommandSubstitution => 6,
        Unparseable => 7,
    };
    if rank(next) > rank(current) {
        next
    } else {
        current
    }
}
