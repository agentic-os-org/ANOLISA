use super::*;

const SAMPLE: &str = "some avg10=0.50 avg60=1.25 avg300=2.00 total=123456\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";

#[test]
fn parses_some_and_full_fixed_point_values() {
    let sample = parse_pressure(SAMPLE);
    let some = sample.some.unwrap();
    assert_eq!(some.avg10, 0.5);
    assert_eq!(some.avg60, 1.25);
    assert_eq!(some.total, 123456);
    let full = sample.full.unwrap();
    assert_eq!(full.avg10, 0.0);
    assert_eq!(full.avg60, 0.0);
    assert_eq!(full.total, 0);
}

#[test]
fn missing_full_line_stays_unknown() {
    let sample = parse_pressure("some avg10=9.00 avg60=8.00 avg300=7.00 total=999\n");
    assert!(sample.some.is_some());
    assert!(sample.full.is_none());
}

#[test]
fn malformed_lines_stay_unknown() {
    for content in [
        "some avg10=abc avg60=1.00 total=5\n",
        "some avg10=1.00 avg60=1.00\n",
        "some avg10=-1.00 avg60=1.00 total=5\n",
        "some avg10=101.00 avg60=1.00 total=5\n",
        "some avg10=1.00 avg60=1.00 total=-5\n",
        "full avg10=1.00 avg60=NaN total=5\n",
    ] {
        let sample = parse_pressure(content);
        assert!(sample.some.is_none(), "{content}");
        assert!(sample.full.is_none(), "{content}");
    }
}

#[test]
fn empty_content_is_unknown_not_zero() {
    let sample = parse_pressure("");
    assert_eq!(sample, PressureSample::default());
}

#[test]
fn oversized_pressure_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.pressure");
    let mut content = SAMPLE.to_string();
    while content.len() as u64 <= PRESSURE_MAX_BYTES {
        content.push_str(SAMPLE);
    }
    std::fs::write(&path, content).unwrap();
    assert!(read_pressure(&path).is_none());
}
