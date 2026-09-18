use aw_contracts::canonical;
use serde_json::Value;

#[test]
fn canonical_wire_vectors_and_ambiguous_inputs() {
    let vectors: Value =
        serde_json::from_str(include_str!("fixtures/canonical-vectors.json")).unwrap();
    for v in vectors.as_array().unwrap() {
        assert_eq!(
            canonical::bytes(&v["input"]).unwrap(),
            v["canonical"].as_str().unwrap().as_bytes()
        );
        assert_eq!(
            canonical::document_digest(&v["input"]).unwrap(),
            v["digest"]
        );
    }
    for bad in [
        r#"{"a":1,"a":2}"#,
        r#"{"x":{"a":1,"\u0061":2}}"#,
        "1.0",
        "1e0",
        "-0",
        "9007199254740992",
        "-9007199254740992",
        r#"{"中":1}"#,
        "{}{}",
        r#""\ud800""#,
    ] {
        assert!(canonical::parse(bad.as_bytes()).is_err(), "{bad}");
    }
    let deep = format!("{}0{}", "[".repeat(34), "]".repeat(34));
    assert!(canonical::parse(deep.as_bytes()).is_err());
    assert!(canonical::parse(&vec![b' '; canonical::MAX_DOCUMENT_BYTES + 1]).is_err());
}
