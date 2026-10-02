#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputExcerptDirection {
    Head,
    Tail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidenceExcerptRequest {
    pub(crate) output_id: String,
    pub(crate) direction: OutputExcerptDirection,
    pub(crate) lines: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidenceExcerpt {
    pub(crate) text: Option<String>,
    pub(crate) status: &'static str,
    pub(crate) redaction_status: &'static str,
    pub(crate) capture_status: EvidenceCaptureStatus,
    pub(crate) confirmation_required: bool,
    pub(crate) truncated: bool,
    pub(crate) truncated_by_lines: bool,
    pub(crate) truncated_by_bytes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvidenceCaptureStatus {
    Available,
    Truncated,
    Unavailable,
    Expired,
    ReadFailed,
}

pub(crate) fn evidence_capture_status_for_block(block: &CommandBlock) -> EvidenceCaptureStatus {
    let Some(output_ref) = block.output.terminal_output_ref.as_deref() else {
        return EvidenceCaptureStatus::Unavailable;
    };
    if !Path::new(output_ref).is_file() {
        return EvidenceCaptureStatus::Expired;
    }
    if std::fs::read_to_string(output_ref).is_err() {
        return EvidenceCaptureStatus::ReadFailed;
    }
    if block.output.terminal_output_bytes as usize > COMMAND_OUTPUT_REF_MAX_BYTES {
        EvidenceCaptureStatus::Truncated
    } else {
        EvidenceCaptureStatus::Available
    }
}

impl EvidenceExcerpt {
    pub(crate) fn evidence_status(&self) -> &'static str {
        match self.capture_status {
            EvidenceCaptureStatus::Available => "available",
            EvidenceCaptureStatus::Truncated => "truncated",
            EvidenceCaptureStatus::Unavailable => "unavailable",
            EvidenceCaptureStatus::Expired => "expired",
            EvidenceCaptureStatus::ReadFailed => "read_failed",
        }
    }

    pub(crate) fn truncation_status(&self) -> &'static str {
        if self.capture_status == EvidenceCaptureStatus::Truncated || self.truncated {
            "truncated"
        } else {
            "complete"
        }
    }
}

pub(crate) type OutputExcerptRequest = EvidenceExcerptRequest;
use std::path::Path;

use crate::types::{CommandBlock, COMMAND_OUTPUT_REF_MAX_BYTES};
