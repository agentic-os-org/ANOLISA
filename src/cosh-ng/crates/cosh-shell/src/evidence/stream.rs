use crate::evidence::request::{parse_first_cosh_request, ParsedCoshRequest};

const REQUEST_MARKER: &str = "```cosh-request";
const CLOSING_FENCE: &str = "```";
const MAX_BUFFERED_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Debug, Default)]
pub(crate) struct CoshRequestStreamFilter {
    pending: String,
}

#[derive(Debug, Default)]
pub(crate) struct FilteredCoshRequestDelta {
    pub(crate) visible_text: String,
    pub(crate) requests: Vec<ParsedCoshRequest>,
    pub(crate) audit_records: Vec<CoshRequestAuditRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoshRequestAuditRecord {
    pub(crate) raw_block: String,
    pub(crate) outcome: CoshRequestAuditOutcome,
    pub(crate) reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoshRequestAuditOutcome {
    Parsed,
    Invalid,
}

impl CoshRequestStreamFilter {
    pub(crate) fn filter_delta(&mut self, delta: &str) -> FilteredCoshRequestDelta {
        self.pending.push_str(delta);
        self.drain_pending(false)
    }

    pub(crate) fn finish(&mut self) -> FilteredCoshRequestDelta {
        self.drain_pending(true)
    }

    pub(crate) fn clear(&mut self) {
        self.pending.clear();
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    fn drain_pending(&mut self, finishing: bool) -> FilteredCoshRequestDelta {
        let mut visible_text = String::new();
        let mut requests = Vec::new();
        let mut audit_records = Vec::new();

        loop {
            let Some(marker_start) = self.pending.find(REQUEST_MARKER) else {
                if finishing {
                    visible_text.push_str(&self.pending);
                    self.pending.clear();
                } else {
                    let keep_from = partial_marker_suffix_start(&self.pending);
                    visible_text.push_str(&self.pending[..keep_from]);
                    self.pending.drain(..keep_from);
                }
                break;
            };

            visible_text.push_str(&self.pending[..marker_start]);
            self.pending.drain(..marker_start);

            let body_start = REQUEST_MARKER.len();
            let Some(close_rel) = self.pending[body_start..].find(CLOSING_FENCE) else {
                if finishing || self.pending.len() > MAX_BUFFERED_REQUEST_BYTES {
                    audit_records.push(CoshRequestAuditRecord {
                        raw_block: self.pending.clone(),
                        outcome: CoshRequestAuditOutcome::Invalid,
                        reason: if finishing {
                            "unclosed_request_block"
                        } else {
                            "request_block_buffer_limit_exceeded"
                        },
                    });
                    visible_text.push_str(&self.pending);
                    self.pending.clear();
                }
                break;
            };

            let close_start = body_start + close_rel;
            let close_end = close_start + CLOSING_FENCE.len();
            let block = self.pending[..close_end].to_string();
            self.pending.drain(..close_end);

            if let Some(parsed) = parse_first_cosh_request(&block) {
                requests.push(parsed);
                audit_records.push(CoshRequestAuditRecord {
                    raw_block: block,
                    outcome: CoshRequestAuditOutcome::Parsed,
                    reason: "parsed",
                });
            } else {
                visible_text.push_str(&block);
                audit_records.push(CoshRequestAuditRecord {
                    raw_block: block,
                    outcome: CoshRequestAuditOutcome::Invalid,
                    reason: "parse_error",
                });
            }
        }

        FilteredCoshRequestDelta {
            visible_text,
            requests,
            audit_records,
        }
    }
}

fn partial_marker_suffix_start(text: &str) -> usize {
    let max = text.len().min(REQUEST_MARKER.len().saturating_sub(1));
    for suffix_len in (1..=max).rev() {
        let start = text.len() - suffix_len;
        if text.is_char_boundary(start) && REQUEST_MARKER.starts_with(&text[start..]) {
            return start;
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::model::OutputExcerptDirection;
    use crate::evidence::request::CoshRequest;

    #[test]
    fn filters_complete_request_block_from_visible_text() {
        let mut filter = CoshRequestStreamFilter::default();
        let result = filter.filter_delta(
            "before\n```cosh-request\noutput terminal-output://raw-1/cmd-2 tail\nlines 20\n```\nafter",
        );

        assert_eq!(result.visible_text, "before\n\nafter");
        assert_eq!(result.requests.len(), 1);
        assert_eq!(result.audit_records.len(), 1);
        assert_eq!(
            result.audit_records[0].outcome,
            CoshRequestAuditOutcome::Parsed
        );
        assert!(matches!(
            result.requests[0].request,
            CoshRequest::Output(ref request)
                if request.output_id == "terminal-output://raw-1/cmd-2"
                    && request.direction == OutputExcerptDirection::Tail
                    && request.lines == Some(20)
        ));
    }

    #[test]
    fn buffers_request_block_split_across_deltas() {
        let mut filter = CoshRequestStreamFilter::default();
        let first = filter.filter_delta("hello\n```cosh-");
        assert_eq!(first.visible_text, "hello\n");
        assert!(first.requests.is_empty());

        let second = filter.filter_delta("request\nhistory\n```\nworld");
        assert_eq!(second.visible_text, "\nworld");
        assert_eq!(second.requests.len(), 1);
        assert_eq!(second.requests[0].request, CoshRequest::History);
    }

    #[test]
    fn releases_invalid_request_block_as_visible_text() {
        let mut filter = CoshRequestStreamFilter::default();
        let result = filter.filter_delta("```cosh-request\nread /tmp/out\n```");

        assert_eq!(result.visible_text, "```cosh-request\nread /tmp/out\n```");
        assert!(result.requests.is_empty());
        assert_eq!(result.audit_records.len(), 1);
        assert_eq!(
            result.audit_records[0].outcome,
            CoshRequestAuditOutcome::Invalid
        );
        assert_eq!(result.audit_records[0].reason, "parse_error");
    }

    #[test]
    fn finish_releases_unclosed_request_block() {
        let mut filter = CoshRequestStreamFilter::default();
        assert_eq!(
            filter.filter_delta("```cosh-request\nhistory").visible_text,
            ""
        );

        let result = filter.finish();
        assert_eq!(result.visible_text, "```cosh-request\nhistory");
        assert!(result.requests.is_empty());
        assert_eq!(result.audit_records.len(), 1);
        assert_eq!(result.audit_records[0].reason, "unclosed_request_block");
    }

    #[test]
    fn releases_oversized_unclosed_request_block_as_visible_text() {
        let mut filter = CoshRequestStreamFilter::default();
        let oversized = format!(
            "```cosh-request\n{}",
            "x".repeat(MAX_BUFFERED_REQUEST_BYTES)
        );

        let result = filter.filter_delta(&oversized);

        assert_eq!(result.visible_text, oversized);
        assert!(result.requests.is_empty());
        assert_eq!(result.audit_records.len(), 1);
        assert_eq!(
            result.audit_records[0].reason,
            "request_block_buffer_limit_exceeded"
        );
    }
}
