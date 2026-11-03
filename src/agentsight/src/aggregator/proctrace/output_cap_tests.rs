use super::*;

#[test]
fn cap_keeps_complete_prefix_for_every_utf8_split() {
    for character in ["é", "中", "😀"] {
        let bytes = character.as_bytes();
        for split in 1..bytes.len() {
            for room in 1..bytes.len() {
                let prefix = vec![b'a'; MAX_RETAINED_PROCESS_OUTPUT_BYTES - room];
                let mut buffer = prefix.clone();
                let mut capped = false;
                append_capped(&mut buffer, &bytes[..split], &mut capped);
                append_capped(&mut buffer, &bytes[split..], &mut capped);
                assert_eq!(buffer, prefix, "{character}, split={split}, room={room}");
                assert!(capped);
                append_capped(&mut buffer, b"must not refill a discarded gap", &mut capped);
                assert_eq!(buffer, prefix);
            }
        }
    }
}

#[test]
fn incomplete_utf8_below_cap_is_completed_by_later_events() {
    let mut buffer = b"prefix".to_vec();
    let mut capped = false;
    append_capped(&mut buffer, &[0xf0], &mut capped);
    assert_eq!(buffer.last(), Some(&0xf0));
    append_capped(&mut buffer, &[0x9f, 0x98], &mut capped);
    append_capped(&mut buffer, &[0x80], &mut capped);
    assert_eq!(std::str::from_utf8(&buffer).unwrap(), "prefix😀");
    assert!(!capped);
}

#[test]
fn cap_keeps_ascii_complete_characters_and_invalid_bytes() {
    for tail in [b"abc".as_slice(), "中".as_bytes(), &[0xff, 0xfe, 0x80]] {
        let mut buffer = vec![b'a'; MAX_RETAINED_PROCESS_OUTPUT_BYTES - tail.len()];
        let mut expected = buffer.clone();
        expected.extend_from_slice(tail);
        let mut capped = false;
        assert_eq!(append_capped(&mut buffer, tail, &mut capped), 0);
        assert_eq!(buffer, expected);
        assert!(capped);
        assert_eq!(append_capped(&mut buffer, b"later", &mut capped), 5);
        assert_eq!(buffer, expected);
    }
}

#[test]
fn cap_can_trim_incomplete_tail_after_invalid_binary_prefix() {
    let mut buffer = vec![0xff; MAX_RETAINED_PROCESS_OUTPUT_BYTES - 2];
    let mut capped = false;
    append_capped(&mut buffer, &[0xe4], &mut capped);
    append_capped(&mut buffer, &[0xb8, 0xad], &mut capped);
    assert_eq!(buffer, vec![0xff; MAX_RETAINED_PROCESS_OUTPUT_BYTES - 2]);
    assert!(capped);
}
