use super::*;

#[test]
fn file_nr_parses_three_raw_integers() {
    assert_eq!(
        parse_file_nr("1234\t56\t789012\n"),
        Some((1234, 56, 789012))
    );
    assert_eq!(parse_file_nr("0 0 0"), Some((0, 0, 0)));
    assert_eq!(parse_file_nr("1 2"), None);
    assert_eq!(parse_file_nr("1 2 3 4"), None);
    assert_eq!(parse_file_nr("a 2 3"), None);
    assert_eq!(parse_file_nr("1 -2 3"), None);
    assert_eq!(parse_file_nr(""), None);
}
