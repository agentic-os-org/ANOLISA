#![allow(dead_code)]

pub(crate) use crate::evidence::model::{OutputExcerptDirection, OutputExcerptRequest};
use crate::evidence::output_policy::parse_terminal_output_id;

const MAX_REQUEST_LINES: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedCoshRequest {
    pub(crate) request: CoshRequest,
    pub(crate) ignored_multiple_request_blocks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoshRequest {
    History,
    Output(OutputExcerptRequest),
}

pub(crate) fn parse_first_cosh_request(text: &str) -> Option<ParsedCoshRequest> {
    let mut parsed = Vec::new();
    let mut lines = text.lines();

    while let Some(line) = lines.next() {
        if line.trim() != "```cosh-request" {
            continue;
        }

        let mut body = Vec::new();
        for body_line in lines.by_ref() {
            if body_line.trim() == "```" {
                if let Some(request) = parse_request_body(&body) {
                    parsed.push(request);
                }
                break;
            }
            body.push(body_line);
        }
    }

    let request = parsed.into_iter().next()?;
    Some(ParsedCoshRequest {
        request,
        ignored_multiple_request_blocks: parse_valid_request_count_after_first(text) > 1,
    })
}

fn parse_valid_request_count_after_first(text: &str) -> usize {
    let mut count = 0;
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "```cosh-request" {
            continue;
        }
        let mut body = Vec::new();
        for body_line in lines.by_ref() {
            if body_line.trim() == "```" {
                if parse_request_body(&body).is_some() {
                    count += 1;
                }
                break;
            }
            body.push(body_line);
        }
    }
    count
}

fn parse_request_body(lines: &[&str]) -> Option<CoshRequest> {
    let non_empty = lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    match non_empty.as_slice() {
        ["history"] => Some(CoshRequest::History),
        [output_line] => parse_output_request(output_line, None).map(CoshRequest::Output),
        [output_line, lines_line] => {
            parse_output_request(output_line, Some(lines_line)).map(CoshRequest::Output)
        }
        _ => None,
    }
}

fn parse_output_request(
    output_line: &str,
    lines_line: Option<&str>,
) -> Option<OutputExcerptRequest> {
    let tokens = output_line.split_whitespace().collect::<Vec<_>>();
    let [verb, output_id, rest @ ..] = tokens.as_slice() else {
        return None;
    };
    if *verb != "output" || parse_terminal_output_id(output_id).is_none() {
        return None;
    }
    let direction = match rest {
        [] => OutputExcerptDirection::Tail,
        ["head"] => OutputExcerptDirection::Head,
        ["tail"] => OutputExcerptDirection::Tail,
        _ => return None,
    };
    let lines = match lines_line {
        Some(line) => Some(parse_lines_request(line)?),
        None => None,
    };
    Some(OutputExcerptRequest {
        output_id: (*output_id).to_string(),
        direction,
        lines,
    })
}

fn parse_lines_request(line: &str) -> Option<usize> {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    let ["lines", value] = tokens.as_slice() else {
        return None;
    };
    let parsed = value.parse::<usize>().ok()?;
    (parsed > 0).then_some(parsed.min(MAX_REQUEST_LINES))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_history_request_only_inside_fence() {
        let parsed = parse_first_cosh_request(
            "Please inspect history\n```cosh-request\nhistory\n```\nthanks",
        )
        .expect("history request");

        assert_eq!(parsed.request, CoshRequest::History);
        assert!(!parsed.ignored_multiple_request_blocks);
        assert!(parse_first_cosh_request("history").is_none());
    }

    #[test]
    fn rejects_history_with_arguments() {
        assert!(parse_first_cosh_request("```cosh-request\nhistory recent\n```").is_none());
    }

    #[test]
    fn parses_output_tail_with_bounded_lines() {
        let parsed = parse_first_cosh_request(
            "```cosh-request\noutput terminal-output://raw-1/cmd-2 tail\nlines 999\n```",
        )
        .expect("output request");

        assert_eq!(
            parsed.request,
            CoshRequest::Output(OutputExcerptRequest {
                output_id: "terminal-output://raw-1/cmd-2".to_string(),
                direction: OutputExcerptDirection::Tail,
                lines: Some(MAX_REQUEST_LINES),
            })
        );
    }

    #[test]
    fn parses_output_head_and_default_tail() {
        let head = parse_first_cosh_request(
            "```cosh-request\noutput terminal-output://raw-1/cmd-2 head\n```",
        )
        .expect("head request");
        assert!(matches!(
            head.request,
            CoshRequest::Output(OutputExcerptRequest {
                direction: OutputExcerptDirection::Head,
                ..
            })
        ));

        let tail =
            parse_first_cosh_request("```cosh-request\noutput terminal-output://raw-1/cmd-2\n```")
                .expect("tail request");
        assert!(matches!(
            tail.request,
            CoshRequest::Output(OutputExcerptRequest {
                direction: OutputExcerptDirection::Tail,
                ..
            })
        ));
    }

    #[test]
    fn rejects_natural_language_and_complex_output_requests() {
        for input in [
            "please read terminal-output://raw-1/cmd-2",
            "```cosh-request\nread terminal-output://raw-1/cmd-2\n```",
            "```cosh-request\noutput /tmp/output-ref tail\n```",
            "```cosh-request\noutput terminal-output://raw-1 tail\n```",
            "```cosh-request\noutput terminal-output:///cmd-2 tail\n```",
            "```cosh-request\noutput terminal-output://raw-1/cmd-2/extra tail\n```",
            "```cosh-request\noutput terminal-output://raw-1/cmd-2 tail extra\n```",
            "```cosh-request\noutput terminal-output://raw-1/cmd-2 tail\nlines 0\n```",
            "```cosh-request\noutput terminal-output://raw-1/cmd-2 tail\nlines 10\nextra true\n```",
        ] {
            assert!(parse_first_cosh_request(input).is_none(), "{input}");
        }
    }

    #[test]
    fn returns_first_valid_request_and_flags_multiple() {
        let parsed = parse_first_cosh_request(
            "```cosh-request\nhistory\n```\ntext\n```cosh-request\noutput terminal-output://raw-1/cmd-2 tail\n```",
        )
        .expect("first request");

        assert_eq!(parsed.request, CoshRequest::History);
        assert!(parsed.ignored_multiple_request_blocks);
    }
}
