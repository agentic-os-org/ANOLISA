//! Synthetic reference decisions are stated in fixtures, independently of checking.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use semantic_arbiter::check::check;
use semantic_arbiter::contract::UnitAnswer;
use semantic_arbiter::outcome::CandidateControl;
use semantic_arbiter::protocol::{
    decode_request, decode_response, input_digest, MAX_MESSAGE_BYTES,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SampleIndex {
    synthetic: bool,
    cases: Vec<Sample>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sample {
    case_id: String,
    request: String,
    expected: Expected,
    rationale: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    candidate_control: CandidateControl,
    obligation_kinds: Vec<String>,
    answers: Vec<UnitAnswer>,
}

#[test]
fn every_example_is_indexed_bounded_and_matches_independent_expectations() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let index_bytes = fs::read(examples.join("index.json")).unwrap();
    assert!(index_bytes.len() <= MAX_MESSAGE_BYTES);
    let index: SampleIndex = serde_json::from_slice(&index_bytes).unwrap();
    assert!(index.synthetic);
    assert_eq!(index.cases.len(), 12);

    let mut indexed_files = BTreeSet::from([String::from("index.json")]);
    let mut case_ids = BTreeSet::new();
    for sample in index.cases {
        assert!(case_ids.insert(sample.case_id.clone()));
        assert_eq!(sample.request, format!("{}.json", sample.case_id));
        assert!(indexed_files.insert(sample.request.clone()));
        assert!(sample.rationale.starts_with("Synthetic "));
        assert!(sample.rationale.len() <= 256);

        let bytes = fs::read(examples.join(&sample.request)).unwrap();
        assert!(bytes.len() <= 8192, "{} must stay small", sample.case_id);
        let raw: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(raw.get("expected").is_none());
        let request = decode_request(&bytes).unwrap();
        assert_eq!(request.request_id, sample.case_id);
        assert_eq!(
            decode_request(&serde_json::to_vec(&request).unwrap()).unwrap(),
            request
        );

        let response = check(&request).unwrap();
        assert_eq!(
            response.answers, sample.expected.answers,
            "{}",
            sample.case_id
        );
        assert_eq!(
            response.outcome.control.candidate, sample.expected.candidate_control,
            "{}",
            sample.case_id
        );
        assert!(matches!(
            response.outcome.control.candidate,
            CandidateControl::Allow | CandidateControl::Block
        ));
        assert!(response.outcome.control.subject.is_empty());
        let kinds: Vec<String> = response
            .outcome
            .obligations
            .iter()
            .map(|obligation| {
                let value = serde_json::to_value(&obligation.action).unwrap();
                value["kind"].as_str().unwrap().to_owned()
            })
            .collect();
        assert_eq!(
            kinds, sample.expected.obligation_kinds,
            "{}",
            sample.case_id
        );
        assert!(response
            .outcome
            .obligations
            .iter()
            .all(|item| item.critical));
        let expected_refs: BTreeSet<_> = sample
            .expected
            .answers
            .iter()
            .flat_map(|answer| answer.evidence_refs.iter().cloned())
            .collect();
        assert_eq!(
            response.outcome.evidence_refs,
            expected_refs.into_iter().collect::<Vec<_>>()
        );
        assert_eq!(
            decode_response(&serde_json::to_vec(&response).unwrap(), &request).unwrap(),
            response
        );
        assert_eq!(check(&request).unwrap(), response);
    }

    let actual_files: BTreeSet<_> = fs::read_dir(&examples)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        actual_files, indexed_files,
        "Every JSON must be parsed as an index or request"
    );
}

#[test]
fn input_digest_has_a_fixed_domain_separated_vector() {
    let bytes = include_bytes!("../examples/preservation-allow.json");
    let request = decode_request(bytes).unwrap();
    // Independently computed with Python hashlib over the domain prefix and typed JSON.
    let expected = "sha256:8697479930b097001cd968179ece5288e787fdf1803303f0a4abb9321c9f00fe";
    assert_eq!(serde_json::to_vec(&request).unwrap().len(), 1257);
    assert_eq!(input_digest(&request).unwrap(), expected);
    assert_eq!(check(&request).unwrap().input_digest, expected);

    // Wire formatting and key order do not affect the typed-input digest.
    let value: Value = serde_json::from_slice(bytes).unwrap();
    let reordered = decode_request(&serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    assert_eq!(input_digest(&reordered).unwrap(), expected);
    let mut changed = request;
    changed.task.as_mut().unwrap().text.push_str(" Changed.");
    assert_ne!(input_digest(&changed).unwrap(), expected);
}
