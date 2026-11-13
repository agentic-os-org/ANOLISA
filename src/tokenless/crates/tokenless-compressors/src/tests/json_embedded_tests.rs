use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use tokenless_ccr::{InMemoryStore, StashError, recovery_hashes};

use crate::{JsonCompressionConfig, JsonCompressor, JsonOperation, JsonOutcome, Recoverability};

const HEADER: &str = "[HTML rendered as Markdown";

/// A rich-text fragment with typical inline presentation overhead.
fn rich(label: &str) -> String {
    format!(
        "<div class=\"rich-text\" style=\"font-family: Arial, sans-serif; color: #333333\">\
         <p style=\"margin: 0 0 8px 0\"><span style=\"font-weight: normal\">{label}: approve \
         the refund before</span> <strong>2024-06-01</strong>.</p>\
         <ul style=\"padding-left: 16px; margin: 0\"><li style=\"margin: 0\">\
         <a href=\"https://example.com/{label}\" target=\"_blank\" rel=\"noopener noreferrer\">\
         Reference</a></li></ul></div>"
    )
}

fn json_str(value: &str) -> String {
    serde_json::to_string(value).unwrap()
}

#[derive(Default)]
struct CountingStore {
    inner: InMemoryStore,
    stashes: AtomicUsize,
    fail: bool,
}

impl StashStore for CountingStore {
    fn stash(&self, payload: &str) -> Result<StashWrite, StashError> {
        self.stashes.fetch_add(1, Ordering::Relaxed);
        if self.fail {
            return Err(StashError::Backend("simulated failure".to_owned()));
        }
        self.inner.stash(payload)
    }

    fn retrieve(&self, hash: &str) -> Result<Option<String>, StashError> {
        self.inner.retrieve(hash)
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn evict_expired(&self) -> Result<usize, StashError> {
        self.inner.evict_expired()
    }

    fn delete(&self, hash: &str, generation: u64) -> Result<bool, StashError> {
        self.inner.delete(hash, generation)
    }
}

fn embedded_context(stash: Option<&dyn StashStore>) -> JsonCompressionContext<'_> {
    JsonCompressionContext {
        recovery: &RecoveryMethod::Shell,
        stash,
        allow_toon: false,
        min_toon_chars: usize::MAX,
        allow_unrecoverable: true,
        embedded_html: true,
    }
}

fn compress(input: &str, store: &dyn StashStore) -> JsonOutcome {
    JsonCompressor::default()
        .compress(input, &embedded_context(Some(store)))
        .unwrap()
}

/// Replaces every rewritten string with its stashed original and returns
/// how many were restored. Each rewrite must carry exactly one reference.
fn restore(value: &mut Value, store: &dyn StashStore) -> usize {
    match value {
        Value::String(text) if text.starts_with(HEADER) => {
            let hashes = recovery_hashes(text, &RecoveryMethod::Shell);
            assert_eq!(hashes.len(), 1, "{text}");
            *text = store
                .retrieve(hashes[0])
                .unwrap()
                .expect("stashed original");
            1
        }
        Value::Array(items) => items.iter_mut().map(|item| restore(item, store)).sum(),
        Value::Object(map) => map.values_mut().map(|item| restore(item, store)).sum(),
        _ => 0,
    }
}

#[test]
fn nested_html_reaches_the_renderer_and_everything_else_is_preserved() {
    let note = "Line\nwith \"quotes\", \\ backslash, \u{2028} separator and 😀 价格";
    let serialized = serde_json::to_string(&json!({"body": rich("inside-string")})).unwrap();
    let form = format!("<form action=\"/pay\">{}</form>", rich("form"));
    let input = format!(
        r#"{{"success":true,"record":{{"id":"example-123","amount":125.50,"big":18446744073709551617,"tiny":1e-400,"empty":"","none":null,"note":{note},"content":{content}}},"items":[{{"id":1,"body":{first},"tags":["<p>short</p>"]}},{{"id":2,"body":{second}}}],"deep":{{"a":{{"b":{{"c":[{deep}]}}}}}},"serialized":{serialized},"form":{form}}}"#,
        note = json_str(note),
        content = json_str(&rich("record")),
        first = json_str(&rich("first")),
        second = json_str(&rich("second")),
        deep = json_str(&rich("deep")),
        serialized = json_str(&serialized),
        form = json_str(&form),
    );
    let store = InMemoryStore::new();
    let outcome = compress(&input, &store);

    assert_eq!(outcome.operations, [JsonOperation::EmbeddedHtml]);
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);
    assert_eq!(outcome.metrics.embedded_rewrites, 4);
    assert_eq!(outcome.stash_writes.len(), 4);
    assert!(estimate_tokens(&outcome.output) < estimate_tokens(&input));
    // Exact numeric literals and field order are untouched.
    for literal in [
        r#""amount":125.50,"#,
        r#""big":18446744073709551617,"#,
        r#""tiny":1e-400,"#,
    ] {
        assert!(outcome.output.contains(literal), "{literal}");
    }
    assert!(
        outcome
            .output
            .starts_with(r#"{"success":true,"record":{"id":"example-123","#)
    );

    let original: Value = serde_json::from_str(&input).unwrap();
    let mut output: Value = serde_json::from_str(&outcome.output).unwrap();
    // Unsupported, protected, short and string-contained JSON values stay
    // the same strings; serialized JSON is never turned into an object.
    for pointer in [
        "/record/note",
        "/record/empty",
        "/record/none",
        "/items/0/tags",
        "/serialized",
        "/form",
    ] {
        assert_eq!(
            output.pointer(pointer),
            original.pointer(pointer),
            "{pointer}"
        );
    }
    assert!(output["serialized"].is_string());
    // Each rewrite stays in its own slot, next to the fields it belongs to.
    for (pointer, label) in [
        ("/record/content", "record"),
        ("/items/0/body", "first"),
        ("/items/1/body", "second"),
        ("/deep/a/b/c/0", "deep"),
    ] {
        let text = output.pointer(pointer).unwrap().as_str().unwrap();
        assert!(text.starts_with(HEADER), "{pointer}");
        assert!(text.contains(&format!(
            "{label}: approve the refund before **2024-06-01**."
        )));
        assert!(text.contains(&format!("[Reference](https://example.com/{label})")));
        assert!(!text.contains("style="));
    }
    assert_eq!(output["items"][1]["id"], 2);

    // Reconstruction from the Stash recovers the original document exactly,
    // and each payload is the original value byte for byte.
    assert_eq!(restore(&mut output, &store), 4);
    assert_eq!(output, original);
    assert_eq!(
        output["record"]["content"].as_str().unwrap(),
        rich("record")
    );
}

#[test]
fn the_lossless_early_return_no_longer_hides_embedded_html() {
    let input = serde_json::to_string_pretty(&json!({
        "data": {"id": 7, "description": rich("pretty")}
    }))
    .unwrap();
    let store = InMemoryStore::new();
    let disabled = JsonCompressor::default()
        .compress(
            &input,
            &JsonCompressionContext {
                embedded_html: false,
                ..embedded_context(Some(&store))
            },
        )
        .unwrap();
    // Compaction alone already clears the lossless early return.
    assert_eq!(disabled.operations, [JsonOperation::Cleanup]);
    assert!(disabled.output.contains("style="));

    let enabled = compress(&input, &store);
    assert_eq!(enabled.operations, [JsonOperation::EmbeddedHtml]);
    assert!(estimate_tokens(&enabled.output) < estimate_tokens(&disabled.output));
}

#[test]
fn repeated_values_are_rendered_and_stashed_once() {
    let input = serde_json::to_string(
        &(0..5)
            .map(|id| json!({"id": id, "body": rich("shared")}))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let store = CountingStore::default();
    let outcome = compress(&input, &store);

    assert_eq!(outcome.metrics.embedded_rewrites, 5);
    assert_eq!(store.stashes.load(Ordering::Relaxed), 1);
    let output: Value = serde_json::from_str(&outcome.output).unwrap();
    let bodies = output
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["body"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(bodies.len(), 1);
}

#[test]
fn rewrites_need_the_switch_a_stash_and_a_recovery_path() {
    let input = serde_json::to_string(&json!({"id": 1, "body": rich("gated")})).unwrap();
    let store = CountingStore::default();
    for context in [
        JsonCompressionContext {
            embedded_html: false,
            ..embedded_context(Some(&store))
        },
        embedded_context(None),
        JsonCompressionContext {
            recovery: &RecoveryMethod::None,
            ..embedded_context(Some(&store))
        },
    ] {
        let outcome = JsonCompressor::default()
            .compress(&input, &context)
            .unwrap();
        assert_eq!(outcome.output, input);
        assert!(outcome.operations.is_empty());
        assert_eq!(outcome.recoverability, Recoverability::Lossless);
    }
    assert_eq!(store.stashes.load(Ordering::Relaxed), 0);
}

#[test]
fn a_failed_stash_write_keeps_the_value_and_counts_once() {
    let input = serde_json::to_string(&json!({"id": 1, "body": rich("failing")})).unwrap();
    let store = CountingStore {
        fail: true,
        ..CountingStore::default()
    };
    let outcome = compress(&input, &store);

    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert!(outcome.stash_writes.is_empty());
    assert_eq!(outcome.metrics.stash_errors, 1);
    assert_eq!(store.stashes.load(Ordering::Relaxed), 1);
}

#[test]
fn views_without_enough_savings_are_rejected_before_any_write() {
    let prose = format!("<p>{}</p>", "Plain sentence without markup. ".repeat(12));
    let input = serde_json::to_string(&json!({"id": 1, "body": prose})).unwrap();
    let store = CountingStore::default();
    let outcome = compress(&input, &store);

    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(store.stashes.load(Ordering::Relaxed), 0);
}

#[test]
fn depth_size_and_render_budgets_bound_the_work() {
    // Values nested deeper than the routing depth stay unchanged.
    let nested = |depth: usize| {
        format!(
            "{}{}{}",
            "[".repeat(depth),
            json_str(&rich("nested")),
            "]".repeat(depth)
        )
    };
    let store = CountingStore::default();
    assert_eq!(compress(&nested(30), &store).metrics.embedded_rewrites, 1);
    assert_eq!(store.stashes.load(Ordering::Relaxed), 1);
    let store = CountingStore::default();
    let deep = compress(&nested(40), &store);
    assert_eq!(deep.metrics.embedded_rewrites, 0);
    assert!(!deep.operations.contains(&JsonOperation::EmbeddedHtml));
    // The only write is the existing depth bound's subtree, not a rewrite.
    assert!(!deep.output.contains(HEADER));
    assert_eq!(store.stashes.load(Ordering::Relaxed), 1);

    // An oversized value is not routed, although it would render with large
    // savings; the existing string bound still applies to it.
    let oversized = format!(
        "<div>{}</div>",
        "<p style=\"margin:0\">padding</p>".repeat(9000)
    );
    assert!(oversized.len() > 256 * 1024);
    assert!(HtmlExtractor.render_fragment(&oversized).is_some());
    let store = CountingStore::default();
    let input = serde_json::to_string(&json!({"body": oversized})).unwrap();
    let outcome = compress(&input, &store);
    assert_eq!(outcome.operations, [JsonOperation::Truncation]);
    assert_eq!(outcome.metrics.embedded_rewrites, 0);
    assert_eq!(store.stashes.load(Ordering::Relaxed), 1);

    // Only a bounded number of values is parsed per document.
    let many = (0..130)
        .map(|index| {
            (
                format!("k{index}"),
                Value::String(rich(&format!("v{index}"))),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let input = serde_json::to_string(&many).unwrap();
    let store = CountingStore::default();
    let outcome = compress(&input, &store);
    assert_eq!(outcome.metrics.embedded_rewrites, 128);
    assert_eq!(store.stashes.load(Ordering::Relaxed), 128);
    let output: Value = serde_json::from_str(&outcome.output).unwrap();
    assert_eq!(output["k129"], many["k129"]);
}

#[test]
fn toon_output_keeps_rewritten_values_recoverable() {
    let records = (0..3)
        .map(|id| json!({"id": id, "status": "open", "body": rich(&format!("ticket-{id}"))}))
        .collect::<Vec<_>>();
    let input = serde_json::to_string(&json!({ "records": records })).unwrap();
    let store = InMemoryStore::new();
    let outcome = JsonCompressor::default()
        .compress(
            &input,
            &JsonCompressionContext {
                allow_toon: true,
                min_toon_chars: 0,
                ..embedded_context(Some(&store))
            },
        )
        .unwrap();

    assert_eq!(
        outcome.operations,
        [JsonOperation::EmbeddedHtml, JsonOperation::Toon]
    );
    // Decoding TOON yields the rewritten document; only the Stash restores
    // the original values.
    let mut decoded: Value = toon_format::decode_default(&outcome.output).unwrap();
    assert_ne!(decoded, serde_json::from_str::<Value>(&input).unwrap());
    assert_eq!(restore(&mut decoded, &store), 3);
    assert_eq!(decoded, serde_json::from_str::<Value>(&input).unwrap());
}

#[test]
fn bounded_candidates_keep_their_string_limit_and_write_order() {
    // Plain text keeps lossless savings under the early-return threshold.
    let input = serde_json::to_string(&json!({
        "log": "plain words ".repeat(400),
        "body": rich("bounded"),
    }))
    .unwrap();
    let store = InMemoryStore::new();
    let outcome = JsonCompressor::new(JsonCompressionConfig {
        truncate_strings_at: 150,
        ..JsonCompressionConfig::default()
    })
    .compress(&input, &embedded_context(Some(&store)))
    .unwrap();

    assert_eq!(outcome.operations, [JsonOperation::Truncation]);
    let output: Value = serde_json::from_str(&outcome.output).unwrap();
    let body = output["body"].as_str().unwrap();
    assert!(body.chars().count() <= 150);
    assert!(body.contains("… Truncated."), "{body}");
    let hashes = recovery_hashes(body, &RecoveryMethod::Shell);
    assert_eq!(
        store.retrieve(hashes[0]).unwrap().as_deref(),
        Some(rich("bounded").as_str())
    );
    // The rewrite and the truncation stash the same value. Writes are
    // reported in the order they happened, so the second is recognizably
    // this call's own refresh of the first.
    let writes = &outcome.stash_writes;
    let first = writes
        .iter()
        .position(|write| write.key == hashes[0])
        .unwrap();
    let refresh = writes[first + 1..]
        .iter()
        .find(|write| write.key == hashes[0])
        .unwrap();
    assert!(writes[first].created);
    assert_eq!(refresh.previous_generation, Some(writes[first].generation));
}

#[test]
fn card_variants_compress_supported_subtrees_and_keep_unsupported_content() {
    let base = "<div class=\"venue-card\" style=\"padding:16px;border-radius:12px;color:#222222;background:#ffffff\">\
      <h2 style=\"margin:0 0 8px 0;font-size:18px\">Sushi Zen</h2>\
      <p style=\"margin:0 0 8px 0\">Rating: <strong>4.8</strong> (320 reviews) · ¥198/person</p>\
      <p style=\"margin:0 0 8px 0\">Address: 88 Nanjing West Road, Jing'an District</p>\
      <ul style=\"padding-left:18px;margin:0 0 8px 0\">\
        <li style=\"margin:0\">Hours: 11:30-22:00</li>\
        <li style=\"margin:0\"><a href=\"https://maps.example.com/v1\" target=\"_blank\" rel=\"noopener noreferrer\">Directions</a></li>\
        <li style=\"margin:0\"><img src=\"https://img.example.com/v1.jpg\" alt=\"Salmon platter\" width=\"320\" height=\"180\"></li>\
      </ul>\
      <button type=\"button\" class=\"btn\">Book table</button>";
    let variants = [
        ("supported", format!("{base}</div>")),
        (
            "svg",
            format!("{base}<svg viewBox=\"0 0 24 24\"><use xlink:href=\"#location-icon\"></use></svg></div>"),
        ),
        (
            "action_payload",
            format!(
                "{base}<script type=\"application/json\" data-action-payload=\"true\">{{\"venueId\":\"v1\",\"action\":\"open_map\"}}</script></div>"
            ),
        ),
        (
            "data_attribute",
            format!("{base}<button type=\"button\" data-action-type=\"open_map\">Open map</button></div>"),
        ),
    ];

    for allow_toon in [false, true] {
        for (name, html) in &variants {
            let input = serde_json::to_string(&json!({
                "code": 0,
                "card": {"id": "card-1", "html": html},
            }))
            .unwrap();
            let store = CountingStore::default();
            let outcome = JsonCompressor::default()
                .compress(
                    &input,
                    &JsonCompressionContext {
                        allow_toon,
                        min_toon_chars: 0,
                        ..embedded_context(Some(&store))
                    },
                )
                .unwrap();

            assert!(
                outcome.operations.contains(&JsonOperation::EmbeddedHtml),
                "{name} (toon={allow_toon})"
            );
            assert_eq!(outcome.metrics.embedded_rewrites, 1, "{name}");
            assert_eq!(outcome.stash_writes.len(), 1, "{name}");
            assert_eq!(store.stashes.load(Ordering::Relaxed), 1, "{name}");
            assert!(
                estimate_tokens(&input) >= estimate_tokens(&outcome.output) + 16,
                "{name} (toon={allow_toon})"
            );

            let mut decoded: Value = if outcome.operations.contains(&JsonOperation::Toon) {
                toon_format::decode_default(&outcome.output).unwrap()
            } else {
                serde_json::from_str(&outcome.output).unwrap()
            };
            let rendered = decoded["card"]["html"].as_str().unwrap();
            assert!(rendered.starts_with(HEADER), "{name}: {rendered}");
            assert!(rendered.contains("## Sushi Zen"), "{name}: {rendered}");
            assert!(
                rendered.contains("Rating: **4.8** (320 reviews) · ¥198/person"),
                "{name}: {rendered}"
            );
            assert!(
                rendered.contains("[Directions](https://maps.example.com/v1)"),
                "{name}: {rendered}"
            );
            assert!(
                rendered.contains("![Salmon platter](https://img.example.com/v1.jpg)"),
                "{name}: {rendered}"
            );
            assert!(!rendered.contains("padding:16px"), "{name}: {rendered}");
            match *name {
                "svg" => assert!(
                    rendered.contains(
                        "<svg viewBox=\"0 0 24 24\"><use xlink:href=\"#location-icon\"></use></svg>"
                    ),
                    "{rendered}"
                ),
                "action_payload" => assert!(
                    rendered.contains(
                        "<script type=\"application/json\" data-action-payload=\"true\">{\"venueId\":\"v1\",\"action\":\"open_map\"}</script>"
                    ),
                    "{rendered}"
                ),
                "data_attribute" => assert!(
                    rendered.contains(
                        "<button type=\"button\" data-action-type=\"open_map\">Open map</button>"
                    ),
                    "{rendered}"
                ),
                _ => {}
            }

            assert_eq!(restore(&mut decoded, &store), 1, "{name}");
            assert_eq!(
                decoded,
                serde_json::from_str::<Value>(&input).unwrap(),
                "{name}"
            );
        }
    }
}

#[test]
fn multi_venue_dining_card_payload_saves_tokens_and_recovers_byte_for_byte() {
    let mut card = String::from(
        "<div class=\"dining-feed\" style=\"font-family:'PingFang SC',sans-serif;padding:16px;background:#ffffff;color:#222222\" \
         data-xiaomi-card-click-actions='{\"scene\":\"dining_recommend\",\"cardCount\":15}'>\
         <style>.venue{border-radius:12px;box-shadow:0 4px 16px rgba(0,0,0,0.08);padding:4px 16px;margin-bottom:12px}</style>\
         <script type=\"application/json\" data-card-meta=\"true\">{\"feedId\":\"f-99\",\"total\":15}</script>",
    );
    for idx in 1..=15 {
        let _ = std::fmt::Write::write_fmt(
            &mut card,
            format_args!(
                "<article class=\"venue\" style=\"display:flex;flex-direction:column;gap:8px;padding:16px;border:1px solid #eeeeee\">\
                 <div style=\"display:flex;justify-content:space-between;align-items:center;gap:8px\">\
                   <h3 style=\"margin:0;font-size:16px;line-height:24px\">Venue #{idx} Bistro</h3>\
                   <span style=\"font-weight:bold;color:#ff5500\">¥{price}/人</span>\
                 </div>\
                 <p style=\"margin:0;font-size:13px;line-height:20px;color:#555555\">\
                   Rating <strong>4.{rating}</strong> · {reviews} reviews · \
                   <span style=\"text-decoration:line-through\">¥{orig}</span> set menu\
                 </p>\
                 <p style=\"margin:0;font-size:13px;line-height:20px;color:#555555\">\
                   <svg class=\"ic\" style=\"width:14px;height:14px\" viewBox=\"0 0 24 24\"><use xlink:href=\"#ic-pin\"></use></svg> \
                   No. {idx} Nanjing West Rd · \
                   <svg class=\"ic\" style=\"width:14px;height:14px\" viewBox=\"0 0 24 24\"><use xlink:href=\"#ic-clock\"></use></svg> \
                   11:00-22:00 · \
                   <svg class=\"ic\" style=\"width:14px;height:14px\" viewBox=\"0 0 24 24\"><use xlink:href=\"#ic-metro\"></use></svg> \
                   280m · \
                   <svg class=\"ic\" style=\"width:14px;height:14px\" viewBox=\"0 0 24 24\"><use xlink:href=\"#ic-star\"></use></svg> \
                   Top pick · \
                   <svg class=\"ic\" style=\"width:14px;height:14px\" viewBox=\"0 0 24 24\"><use xlink:href=\"#ic-phone\"></use></svg> \
                   <a href=\"https://dining.example.com/v/{idx}\" target=\"_blank\" rel=\"noopener noreferrer\">Details</a>\
                 </p>\
                 <img src=\"https://img.example.com/v/{idx}.jpg\" alt=\"Venue {idx} dish\" data-image-src=\"https://cdn.example.com/hd/{idx}.webp\" width=\"320\" height=\"180\">\
                 <button type=\"button\" class=\"action-btn\" style=\"padding:6px 12px;border-radius:6px;cursor:pointer\" data-action-type=\"navigate\" data-venue-id=\"v-{idx}\">Navigate</button>\
                 </article>",
                price = 120 + idx * 5,
                rating = idx % 9 + 1,
                reviews = 200 + idx * 17,
                orig = 180 + idx * 5,
            ),
        );
    }
    card.push_str(
        "<script type=\"application/json\" data-action-payload=\"true\">{\"actions\":[\"navigate\",\"call\",\"book\"]}</script></div>",
    );
    assert!(card.len() > 18_000, "card len={}", card.len());

    let input = format!(
        r#"{{"traceId":18446744073709551617,"status":"ok","cardHtml":{}}}"#,
        json_str(&card)
    );
    let store = CountingStore::default();
    let outcome = compress(&input, &store);

    assert_eq!(outcome.operations, [JsonOperation::EmbeddedHtml]);
    assert_eq!(outcome.metrics.embedded_rewrites, 1);
    assert_eq!(outcome.stash_writes.len(), 1);
    assert!(outcome.output.contains(r#""traceId":18446744073709551617,"#));
    assert!(estimate_tokens(&outcome.output) * 100 < estimate_tokens(&input) * 65);

    let mut decoded: Value = serde_json::from_str(&outcome.output).unwrap();
    let rendered = decoded["cardHtml"].as_str().unwrap();
    assert!(rendered.contains("data-xiaomi-card-click-actions="));
    assert!(rendered.contains("<use xlink:href=\"#ic-pin\"></use>"));
    assert!(rendered.contains("data-image-src=\"https://cdn.example.com/hd/15.webp\""));
    assert!(rendered.contains("data-action-type=\"navigate\""));
    assert!(rendered.contains("{\"actions\":[\"navigate\",\"call\",\"book\"]}"));
    assert_eq!(restore(&mut decoded, &store), 1);
    assert_eq!(decoded["cardHtml"].as_str().unwrap(), card);
}

#[test]
fn hybrid_views_dominated_by_retained_markup_below_sixteen_tokens_pass_through() {
    let html = format!(
        "<div><p>Summary</p><script type=\"application/json\">{}</script></div>",
        serde_json::to_string(&json!({
            "items": (0..12).map(|i| format!("payload-entry-{i}")).collect::<Vec<_>>()
        }))
        .unwrap()
    );
    assert!(HtmlExtractor.render_fragment(&html).is_some());
    let input = serde_json::to_string(&json!({"id": 1, "body": html})).unwrap();
    let store = CountingStore::default();
    let outcome = compress(&input, &store);

    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(store.stashes.load(Ordering::Relaxed), 0);
}

#[test]
fn inherited_whitespace_unset_and_zero_sized_svg_sprites_compress_faithfully() {
    for (extra, expected) in [
        (
            "<p><span style=\"white-space:pre\">before\n\
             <span style=\"white-space:unset\">one\n  two</span></span></p>",
            "<span style=\"white-space:pre\">before\n<span>one\n  two</span></span>",
        ),
        (
            "<p><span style=\"white-space:pre-line\">before\n\
             <span style=\"white-space:pre\">one   two\nthree</span></span></p>",
            "<span style=\"white-space:pre-line\">before\n\
             <span style=\"white-space:pre\">one   two\nthree</span></span>",
        ),
        (
            "<p><span style=\"white-space:pre\">before\n\
             <span style=\"white-space:pre-line\">one   two\nthree</span></span></p>",
            "<span style=\"white-space:pre\">before\n\
             <span style=\"white-space:pre-line\">one   two\nthree</span></span>",
        ),
        (
            "<svg width=\"0\" height=\"0\"><defs><symbol id=\"icon\">\
             <title>Location</title><path d=\"M0 0\"></path></symbol></defs></svg>\
             <p><svg viewBox=\"0 0 16 16\"><use xlink:href=\"#icon\"></use></svg> Downtown</p>",
            "<defs><symbol id=\"icon\"><title>Location</title><path d=\"M0 0\"></path></symbol></defs>",
        ),
    ] {
        let html = format!("{}{extra}", rich("card"));
        let input = serde_json::to_string(&json!({"id": 1, "html": html})).unwrap();
        let store = CountingStore::default();
        let outcome = compress(&input, &store);

        assert_eq!(outcome.operations, [JsonOperation::EmbeddedHtml], "{extra}");
        let mut decoded: Value = serde_json::from_str(&outcome.output).unwrap();
        let rendered = decoded["html"].as_str().unwrap();
        assert!(rendered.starts_with(HEADER), "{rendered}");
        assert!(rendered.contains(expected), "{rendered}");
        if extra.contains("xlink:href=\"#icon\"") {
            assert!(
                rendered.contains("<use xlink:href=\"#icon\"></use>"),
                "{rendered}"
            );
        }
        assert_eq!(restore(&mut decoded, &store), 1, "{extra}");
        assert_eq!(
            decoded,
            serde_json::from_str::<Value>(&input).unwrap(),
            "{extra}"
        );
    }

    let hidden_symbol = format!(
        "{}<svg width=\"100\" height=\"100\"><symbol id=\"icon\">\
         <svg style=\"font-size:0\"><text x=\"0\" y=\"15\">Hidden instruction</text></svg>\
         </symbol><use xlink:href=\"#icon\"></use></svg>",
        rich("card")
    );
    let input = serde_json::to_string(&json!({"id": 1, "html": hidden_symbol})).unwrap();
    let store = CountingStore::default();
    let outcome = compress(&input, &store);
    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(store.stashes.load(Ordering::Relaxed), 0);
}
