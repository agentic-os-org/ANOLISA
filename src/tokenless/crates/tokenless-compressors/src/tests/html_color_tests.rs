#[test]
fn named_colours_are_sorted_for_binary_search() {
    assert!(NAMED_COLORS.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(NAMED_COLORS.iter().all(|&(name, _)| parse(name)
        == Some(Paint::Color(Color::rgb(
            NAMED_COLORS.iter().find(|entry| entry.0 == name).unwrap().1
        )))));
}

#[test]
fn css_colours_are_read_only_where_every_browser_agrees() {
    let key = |value: &str| match parse(value) {
        Some(Paint::Color(color)) => Some(color.key()),
        _ => None,
    };
    assert_eq!(key("#0f08"), Some([0x00, 0xff, 0x00, 0x88]));
    assert_eq!(key("rgb(0 0 0 / 50%)"), Some([0, 0, 0, 128]));
    assert_eq!(key("rgba(255, 0, 0, .5)"), Some([255, 0, 0, 128]));
    assert_eq!(key("hsl(120deg 100% 25%)"), Some([0, 128, 0, 255]));
    assert_eq!(key("hsla(0.5turn, 100%, 50%, 1)"), Some([0, 255, 255, 255]));
    assert_eq!(parse("currentcolor"), Some(Paint::Current));
    for value in [
        "rgb(255, 255, 100%)",
        "rgb(1., 0, 0)",
        "rgb(0 0 0, 1)",
        "rgb(none 0 0)",
        "rgb(inf, 0, 0)",
        "hsl(0, 50, 50)",
        "lab(50% 0 0)",
        "color-mix(in srgb, red, blue)",
        "#ff",
        "canvastext",
        "inherit",
    ] {
        assert_eq!(parse(value), None, "{value}");
    }
}

#[test]
fn legacy_colour_attributes_read_as_a_browser_does() {
    assert_eq!(legacy("chucknorris"), Some(Color::rgb(0xc0_00_00)));
    assert_eq!(legacy("fff"), Some(Color::rgb(0x0f_0f_0f)));
    assert_eq!(legacy(" #FFF "), Some(Color::WHITE));
    assert_eq!(legacy("White"), Some(Color::WHITE));
    assert_eq!(legacy("#123456789"), Some(Color::rgb(0x12_45_78)));
    assert_eq!(legacy("transparent"), None);
    assert_eq!(legacy("  "), None);
}

#[test]
fn contrast_follows_wcag() {
    assert!((Color::WHITE.contrast(Color::BLACK) - 21.0).abs() < 1e-9);
    assert!(Color::rgb(0xd0_d0_d0).contrast(Color::WHITE) > 1.5);
    assert!(Color::rgb(0xd8_d8_d8).contrast(Color::WHITE) < 1.5);
    let half = match parse("rgba(0, 0, 0, 0.5)") {
        Some(Paint::Color(color)) => color,
        other => panic!("{other:?}"),
    };
    assert_eq!(half.over(Color::WHITE).key(), [128, 128, 128, 255]);
}
