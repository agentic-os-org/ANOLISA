use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use tokenless_ccr::{InMemoryStore, StashError, recovery_hashes};

use super::{BUILD_LOG_VIEW_HEADER_PREFIX, HTML_VIEW_HEADER_PREFIX, TABULAR_VIEW_HEADER_PREFIX};
use crate::{JsonCompressionConfig, JsonCompressor, JsonOperation, JsonOutcome, Recoverability};

const HEADER: &str = HTML_VIEW_HEADER_PREFIX;
const BUILD_LOG_HEADER: &str = BUILD_LOG_VIEW_HEADER_PREFIX;
const TABULAR_HEADER: &str = TABULAR_VIEW_HEADER_PREFIX;

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
    deletes: AtomicUsize,
    fail: bool,
    fail_on_call: Option<usize>,
}

impl StashStore for CountingStore {
    fn stash(&self, payload: &str) -> Result<StashWrite, StashError> {
        let call = self.stashes.fetch_add(1, Ordering::Relaxed) + 1;
        if self.fail || self.fail_on_call == Some(call) {
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
        self.deletes.fetch_add(1, Ordering::Relaxed);
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
        embedded_content: true,
        is_error: false,
    }
}

fn compress(input: &str, store: &dyn StashStore) -> JsonOutcome {
    JsonCompressor::default()
        .compress(input, &embedded_context(Some(store)))
        .unwrap()
}

fn is_leaf_view(text: &str) -> bool {
    text.starts_with(HEADER)
        || text.starts_with(BUILD_LOG_HEADER)
        || text.starts_with(TABULAR_HEADER)
}

fn restore_string_in_place(text: &mut String, store: &dyn StashStore) -> usize {
    if is_leaf_view(text) {
        let hashes = recovery_hashes(text, &RecoveryMethod::Shell);
        assert_eq!(hashes.len(), 1, "{text}");
        *text = store
            .retrieve(hashes[0])
            .unwrap()
            .expect("stashed original");
        return 1;
    }
    let Ok(slots) = super::exact::scan_string_slots(text) else {
        return 0;
    };
    let mut restored = 0;
    let mut rebuilt = String::new();
    let mut cursor = 0;
    for slot in slots {
        let mut inner = slot.decoded;
        let count = restore_string_in_place(&mut inner, store);
        if count > 0 {
            if restored == 0 {
                rebuilt.reserve(text.len());
            }
            rebuilt.push_str(&text[cursor..slot.start]);
            rebuilt.push_str(&super::exact::encode_string_literal(&inner));
            cursor = slot.end;
            restored += count;
        }
    }
    if restored > 0 {
        rebuilt.push_str(&text[cursor..]);
        *text = rebuilt;
    }
    restored
}

/// Replaces every rewritten string (including nested slots inside serialized
/// JSON strings) with its stashed original and returns how many leaf values
/// were restored. Each leaf rewrite must carry exactly one reference.
fn restore(value: &mut Value, store: &dyn StashStore) -> usize {
    match value {
        Value::String(text) => restore_string_in_place(text, store),
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
    let html_only_store = InMemoryStore::new();
    let html_only = JsonCompressor::default()
        .compress(
            &input,
            &JsonCompressionContext {
                embedded_content: false,
                ..embedded_context(Some(&html_only_store))
            },
        )
        .unwrap();
    assert_eq!(html_only.metrics.embedded_rewrites, 4);
    let html_only_val: Value = serde_json::from_str(&html_only.output).unwrap();
    assert_eq!(
        html_only_val["serialized"].as_str(),
        Some(serialized.as_str())
    );

    let store = InMemoryStore::new();
    let outcome = compress(&input, &store);

    assert_eq!(outcome.operations, [JsonOperation::EmbeddedHtml]);
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);
    assert_eq!(outcome.metrics.embedded_rewrites, 5);
    assert_eq!(outcome.stash_writes.len(), 5);
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
    // Unsupported, protected, and short values stay the same strings;
    // serialized JSON stays a valid JSON string (never turned into an object)
    // while its inner HTML leaf is rendered in place.
    for pointer in [
        "/record/note",
        "/record/empty",
        "/record/none",
        "/items/0/tags",
        "/form",
    ] {
        assert_eq!(
            output.pointer(pointer),
            original.pointer(pointer),
            "{pointer}"
        );
    }
    assert!(output["serialized"].is_string());
    let serialized_inner: Value =
        serde_json::from_str(output["serialized"].as_str().unwrap()).unwrap();
    assert!(
        serialized_inner["body"]
            .as_str()
            .unwrap()
            .starts_with(HEADER)
    );
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
    // including the outer serialized JSON string byte for byte.
    assert_eq!(restore(&mut output, &store), 5);
    assert_eq!(output, original);
    assert_eq!(output["serialized"].as_str().unwrap(), serialized);
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
            format!(
                "{base}<svg viewBox=\"0 0 24 24\"><use xlink:href=\"#location-icon\"></use></svg></div>"
            ),
        ),
        (
            "action_payload",
            format!(
                "{base}<script type=\"application/json\" data-action-payload=\"true\">{{\"venueId\":\"v1\",\"action\":\"open_map\"}}</script></div>"
            ),
        ),
        (
            "data_attribute",
            format!(
                "{base}<button type=\"button\" data-action-type=\"open_map\">Open map</button></div>"
            ),
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
    assert!(
        outcome
            .output
            .contains(r#""traceId":18446744073709551617,"#)
    );
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

fn sample_cargo_failure_log() -> String {
    let mut log = String::new();
    for idx in 1..=60 {
        let _ = std::fmt::Write::write_fmt(
            &mut log,
            format_args!(
                "   \x1b[32mCompiling\x1b[0m workspace-crate-{idx:02} v0.4.{idx} (/workspace/crates/crate-{idx:02})\n"
            ),
        );
    }
    log.push_str(
        "error[E0308]: mismatched types\n  --> crates/core/src/lib.rs:88:16\n   |\n         88 |     Ok(result_count)\n   |        ^^^^^^^^^^^^ expected `String`, found `usize`\n         error: could not compile `workspace-core` (lib) due to 1 previous error\n",
    );
    log
}

fn sample_quoted_crlf_csv(rows: usize) -> String {
    let mut csv =
        String::from("\"order_id\",\"customer_region\",\"settlement_state\",\"currency\"\r\n");
    for idx in 1..=rows {
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!("\"ORD-{idx:05}\",\"ap-southeast-1\",\"settled-confirmed\",\"CNY\"\r\n"),
        );
    }
    csv
}

#[test]
fn embedded_build_log_retains_trailing_diagnostics_in_one_stash_write() {
    let log = sample_cargo_failure_log();
    assert!(log.chars().count() > 4096, "len={}", log.chars().count());
    let input = serde_json::to_string(&json!({
        "exit_code": 101,
        "stdout": log,
        "stderr": "",
    }))
    .unwrap();

    // Without embedded content routing, string truncation cuts off the trailing compiler error.
    let truncated_store = InMemoryStore::new();
    let truncated = JsonCompressor::default()
        .compress(
            &input,
            &JsonCompressionContext {
                embedded_content: false,
                ..embedded_context(Some(&truncated_store))
            },
        )
        .unwrap();
    assert_eq!(truncated.operations, [JsonOperation::Truncation]);
    assert!(!truncated.output.contains("error[E0308]"));

    // With embedded content routing, routine progress collapses inline, the trailing
    // diagnostic survives intact, and only one whole-value Stash entry is written.
    let store = CountingStore::default();
    let outcome = compress(&input, &store);
    assert_eq!(outcome.operations, [JsonOperation::EmbeddedBuildLog]);
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);
    assert_eq!(outcome.metrics.embedded_rewrites, 1);
    assert_eq!(outcome.stash_writes.len(), 1);
    assert_eq!(store.stashes.load(Ordering::Relaxed), 1);

    let mut output: Value = serde_json::from_str(&outcome.output).unwrap();
    let stdout = output["stdout"].as_str().unwrap();
    assert!(stdout.starts_with(BUILD_LOG_HEADER), "{stdout}");
    assert!(!stdout.contains('—'), "header stays pure ASCII: {stdout}");
    assert!(stdout.contains("lines omitted.\n"), "{stdout}");
    assert!(
        stdout.contains("error[E0308]: mismatched types"),
        "{stdout}"
    );
    assert!(
        stdout.contains("--> crates/core/src/lib.rs:88:16"),
        "{stdout}"
    );
    assert!(
        stdout.contains("could not compile `workspace-core`"),
        "{stdout}"
    );
    assert_eq!(recovery_hashes(stdout, &RecoveryMethod::Shell).len(), 1);

    assert_eq!(restore(&mut output, &store), 1);
    assert_eq!(output["stdout"].as_str().unwrap(), log);
    assert_eq!(output, serde_json::from_str::<Value>(&input).unwrap());
}

#[test]
fn embedded_tabular_compacts_quoted_csv_and_rejects_low_savings_without_writes() {
    let csv = sample_quoted_crlf_csv(28);
    let input = serde_json::to_string(&json!({
        "query_id": "q-77",
        "export": csv,
    }))
    .unwrap();
    let store = CountingStore::default();
    let outcome = compress(&input, &store);

    assert_eq!(outcome.operations, [JsonOperation::EmbeddedTabular]);
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);
    assert_eq!(outcome.metrics.embedded_rewrites, 1);
    assert_eq!(outcome.stash_writes.len(), 1);
    assert_eq!(store.stashes.load(Ordering::Relaxed), 1);

    let mut output: Value = serde_json::from_str(&outcome.output).unwrap();
    let rendered = output["export"].as_str().unwrap();
    assert!(rendered.starts_with(TABULAR_HEADER), "{rendered}");
    assert!(
        !rendered.contains('—'),
        "header stays pure ASCII: {rendered}"
    );
    assert!(
        rendered.contains("order_id,customer_region,settlement_state,currency\n"),
        "{rendered}"
    );
    assert!(
        rendered.contains("ORD-00028,ap-southeast-1,settled-confirmed,CNY"),
        "{rendered}"
    );
    assert_eq!(restore(&mut output, &store), 1);
    assert_eq!(output["export"].as_str().unwrap(), csv);

    // Unquoted LF CSV and marginal quoted CSV are rejected before touching Stash.
    let unquoted = csv.replace('"', "").replace("\r\n", "\n");
    let small_quoted = sample_quoted_crlf_csv(4);
    for candidate in [unquoted, small_quoted] {
        let doc = serde_json::to_string(&json!({"export": candidate})).unwrap();
        let clean_store = CountingStore::default();
        let unchanged = compress(&doc, &clean_store);
        assert_eq!(unchanged.output, doc);
        assert!(unchanged.operations.is_empty());
        assert_eq!(clean_store.stashes.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn span_preserving_serialized_json_keeps_string_type_formatting_exact_numbers_and_noncanonical_escapes()
 {
    let log = sample_cargo_failure_log();
    let csv = sample_quoted_crlf_csv(24);
    let deep_inner = serde_json::to_string(&json!({
        "card": rich("level-two"),
        "table": csv,
    }))
    .unwrap();
    // Pretty-printed inner JSON carrying 65-bit integer, subnormal-like exponent,
    // negative zero, nested JSON-in-JSON, and a Go-style `\u003c` escaped string.
    let go_escaped = format!(
        "\"\\u003cdiv style=\\\"color:#333\\\"\\u003e{}\\u003c/div\\u003e\"",
        "escaped body copy ".repeat(20)
    );
    let payload = format!(
        "{{\n  \"seq\": 18446744073709551617,\n  \"tiny\": 1e-400,\n  \"negZero\": -0,\n  \"buildOutput\": {},\n  \"nestedEnvelope\": {},\n  \"goEscaped\": {}\n}}",
        json_str(&log),
        json_str(&deep_inner),
        go_escaped,
    );
    let input = format!(
        r#"{{"rpc":"2.0","id":9223372036854775808,"payload":{}}}"#,
        json_str(&payload)
    );

    let store = CountingStore::default();
    let outcome = compress(&input, &store);
    assert_eq!(
        outcome.operations,
        [
            JsonOperation::EmbeddedHtml,
            JsonOperation::EmbeddedBuildLog,
            JsonOperation::EmbeddedTabular,
        ]
    );
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);
    assert_eq!(outcome.metrics.embedded_rewrites, 3);
    assert_eq!(outcome.stash_writes.len(), 3);
    assert_eq!(store.stashes.load(Ordering::Relaxed), 3);
    assert!(outcome.output.contains(r#""id":9223372036854775808,"#));

    let mut outer: Value = serde_json::from_str(&outcome.output).unwrap();
    // Outer `payload` remains a valid JSON string, never an object.
    let rewritten_payload = outer["payload"]
        .as_str()
        .expect("outer payload stays string");
    assert!(rewritten_payload.starts_with(
        "{\n  \"seq\": 18446744073709551617,\n  \"tiny\": 1e-400,\n  \"negZero\": -0,\n"
    ));
    assert!(rewritten_payload.contains(&go_escaped));

    let parsed_payload: Value = serde_json::from_str(rewritten_payload).unwrap();
    assert!(
        parsed_payload["buildOutput"]
            .as_str()
            .unwrap()
            .starts_with(BUILD_LOG_HEADER)
    );
    assert!(parsed_payload["nestedEnvelope"].is_string());
    let parsed_deep: Value =
        serde_json::from_str(parsed_payload["nestedEnvelope"].as_str().unwrap()).unwrap();
    assert!(parsed_deep["card"].as_str().unwrap().starts_with(HEADER));
    assert!(
        parsed_deep["table"]
            .as_str()
            .unwrap()
            .starts_with(TABULAR_HEADER)
    );

    // Restoring the 3 leaf Stash entries reconstructs `payload` and `input` byte for byte.
    assert_eq!(restore(&mut outer, &store), 3);
    assert_eq!(outer["payload"].as_str().unwrap(), payload);

    // HTML switch off keeps HTML untouched inside serialized JSON while still reducing BuildLog and Tabular.
    let no_html_store = CountingStore::default();
    let no_html = JsonCompressor::default()
        .compress(
            &input,
            &JsonCompressionContext {
                embedded_html: false,
                ..embedded_context(Some(&no_html_store))
            },
        )
        .unwrap();
    assert_eq!(
        no_html.operations,
        [
            JsonOperation::EmbeddedBuildLog,
            JsonOperation::EmbeddedTabular,
        ]
    );
    assert_eq!(no_html.metrics.embedded_rewrites, 2);

    // Serialized JSON container depth counts toward `MAX_DEPTH` (32): inner
    // leaf at outer depth 25 + 1 + 5 = 31 rewrites, while outer depth 28 + 1 + 5 = 34 stops.
    let inner_six = serde_json::to_string(&json!({
        "a": {"b": {"c": {"d": {"e": {"leaf": rich("depth-bound")}}}}}
    }))
    .unwrap();
    let wrap_at = |outer_depth: usize| {
        format!(
            "{}{}{}",
            "[".repeat(outer_depth),
            json_str(&inner_six),
            "]".repeat(outer_depth)
        )
    };
    let within_store = CountingStore::default();
    let within = JsonCompressor::new(JsonCompressionConfig {
        max_depth: 64,
        ..JsonCompressionConfig::default()
    })
    .compress(&wrap_at(25), &embedded_context(Some(&within_store)))
    .unwrap();
    assert_eq!(within.metrics.embedded_rewrites, 1);
    assert_eq!(within_store.stashes.load(Ordering::Relaxed), 1);

    let over_store = CountingStore::default();
    let over = JsonCompressor::new(JsonCompressionConfig {
        max_depth: 64,
        ..JsonCompressionConfig::default()
    })
    .compress(&wrap_at(28), &embedded_context(Some(&over_store)))
    .unwrap();
    assert_eq!(over.metrics.embedded_rewrites, 0);
    assert_eq!(over_store.stashes.load(Ordering::Relaxed), 0);
}

#[test]
fn outer_string_bound_prunes_oversized_tabular_and_serialized_json_without_orphan_stash_writes() {
    // 1. Oversized CSV (> 4,096 chars even after quote stripping) skips embedded
    //    rendering and writes only the single bounded truncation Stash entry.
    let big_csv = sample_quoted_crlf_csv(180);
    assert!(big_csv.chars().count() > 8_000);
    let csv_doc = serde_json::to_string(&json!({"export": big_csv})).unwrap();
    let csv_store = CountingStore::default();
    let csv_outcome = compress(&csv_doc, &csv_store);
    assert_eq!(csv_outcome.operations, [JsonOperation::Truncation]);
    assert_eq!(csv_outcome.metrics.embedded_rewrites, 0);
    assert_eq!(csv_store.stashes.load(Ordering::Relaxed), 1);
    assert_eq!(csv_outcome.stash_writes.len(), 1);

    // 2. Oversized serialized JSON envelope whose non-slot skeleton already
    //    exceeds `truncate_strings_at` skips inner leaf routing & Stash writes.
    let log = sample_cargo_failure_log();
    let skeleton_items: Vec<usize> = (0..1_200).collect();
    let huge_envelope = serde_json::to_string(&json!({
        "numbers": skeleton_items,
        "build": log,
    }))
    .unwrap();
    assert!(huge_envelope.chars().count() > 9_000);
    let env_doc = serde_json::to_string(&json!({"envelope": huge_envelope})).unwrap();
    let env_store = CountingStore::default();
    let env_outcome = compress(&env_doc, &env_store);
    assert_eq!(env_outcome.operations, [JsonOperation::Truncation]);
    assert_eq!(env_outcome.metrics.embedded_rewrites, 0);
    assert_eq!(env_store.stashes.load(Ordering::Relaxed), 1);
    assert_eq!(env_outcome.stash_writes.len(), 1);

    // 3. Borderline serialized JSON envelope (skeleton < 4,096 chars, but
    //    skeleton + rewritten inner log > 4,096 chars) neither writes an orphan
    //    Stash row nor poisons the cache for a top-level slot carrying `log`.
    let near_limit_skeleton = "x".repeat(200);
    let near_envelope = serde_json::to_string(&json!({
        "s0": near_limit_skeleton, "s1": near_limit_skeleton, "s2": near_limit_skeleton,
        "s3": near_limit_skeleton, "s4": near_limit_skeleton, "s5": near_limit_skeleton,
        "s6": near_limit_skeleton, "s7": near_limit_skeleton, "s8": near_limit_skeleton,
        "s9": near_limit_skeleton, "s10": near_limit_skeleton, "s11": near_limit_skeleton,
        "s12": near_limit_skeleton, "s13": near_limit_skeleton, "s14": near_limit_skeleton,
        "s15": near_limit_skeleton, "s16": near_limit_skeleton, "s17": near_limit_skeleton,
        "build": log,
    }))
    .unwrap();
    let combined_doc = serde_json::to_string(&json!({
        "pad": "plain words ".repeat(1_500),
        "envelope": near_envelope,
        "top_stdout": log,
    }))
    .unwrap();
    let combined_store = CountingStore::default();
    let combined = compress(&combined_doc, &combined_store);
    assert_eq!(
        combined.operations,
        [JsonOperation::Truncation, JsonOperation::EmbeddedBuildLog]
    );
    assert_eq!(combined.metrics.embedded_rewrites, 1);
    // Exactly 3 writes: `top_stdout` embedded build log + `pad` & `envelope` truncations (0 inner orphans).
    assert_eq!(combined_store.stashes.load(Ordering::Relaxed), 3);
}

#[test]
fn negative_outcomes_are_cached_and_error_mode_disables_html_tabular_and_generic_logs() {
    // 200 repeated plain-text strings (>= 256 chars) hit the negative cache
    // without exhausting `MAX_RENDERS` (128) before subsequent HTML fields.
    let repeated_plain = "plain diagnostic-free prose words ".repeat(12);
    assert!(repeated_plain.chars().count() >= 256);
    let mut obj = serde_json::Map::new();
    for idx in 0..200 {
        obj.insert(format!("p{idx}"), Value::String(repeated_plain.clone()));
    }
    obj.insert("card".into(), Value::String(rich("after-negative-cache")));
    let doc = Value::Object(obj).to_string();
    let store = CountingStore::default();
    let outcome = compress(&doc, &store);
    assert_eq!(outcome.metrics.embedded_rewrites, 1);
    assert_eq!(store.stashes.load(Ordering::Relaxed), 1);

    // Under `is_error: true`, HTML, Tabular, and Generic-only build logs stay
    // untouched while Cargo `DiagnosticFailure` build logs still reduce.
    let mut generic_log = String::from("$ ./deploy.sh\n");
    for idx in 1..=20 {
        let _ = std::fmt::Write::write_fmt(
            &mut generic_log,
            format_args!("Processing chunk-{idx:02} of 20...\n"),
        );
    }
    generic_log.push_str("exit code: 1\n");
    let cargo_err = sample_cargo_failure_log();
    let csv = sample_quoted_crlf_csv(28);
    let html = rich("err-html");
    let err_doc = serde_json::to_string(&json!({
        "html": html,
        "csv": csv,
        "generic": generic_log,
        "cargo": cargo_err,
    }))
    .unwrap();
    let err_store = CountingStore::default();
    let err_outcome = JsonCompressor::default()
        .compress(
            &err_doc,
            &JsonCompressionContext {
                is_error: true,
                ..embedded_context(Some(&err_store))
            },
        )
        .unwrap();
    assert_eq!(err_outcome.operations, [JsonOperation::EmbeddedBuildLog]);
    assert_eq!(err_outcome.metrics.embedded_rewrites, 1);
    assert_eq!(err_store.stashes.load(Ordering::Relaxed), 1);
    let parsed_err: Value = serde_json::from_str(&err_outcome.output).unwrap();
    assert_eq!(parsed_err["html"].as_str(), Some(html.as_str()));
    assert_eq!(parsed_err["csv"].as_str(), Some(csv.as_str()));
    assert_eq!(parsed_err["generic"].as_str(), Some(generic_log.as_str()));
    assert!(
        parsed_err["cargo"]
            .as_str()
            .unwrap()
            .starts_with(BUILD_LOG_HEADER)
    );
}
#[test]
fn multibyte_short_circuit_and_bounded_digest_negative_cache_preserve_render_budget() {
    let store = CountingStore::default();
    let ctx = embedded_context(Some(&store));
    let mut router = EmbeddedRouter::new(&ctx, Some(4096)).unwrap();
    let mut writes = Vec::new();
    let mut errors = 0;

    // >= 256 UTF-8 bytes (450 bytes) yet < 256 Unicode scalar values (150 chars):
    // short-circuits in O(MIN_CHARS) before BLAKE3 hashing or entering `outcomes`.
    let multibyte_short = "中文测试文本".repeat(25);
    assert!(multibyte_short.len() >= MIN_CHARS);
    assert!(multibyte_short.len() < MIN_CHARS * 4);
    assert!(multibyte_short.chars().count() < MIN_CHARS);
    assert!(
        router
            .route(&multibyte_short, 1, Some(4096), &mut writes, &mut errors)
            .is_none()
    );
    assert!(router.outcomes.is_empty());
    assert_eq!(router.negative_entries, 0);
    assert_eq!(router.remaining_renders, MAX_RENDERS);

    // 1,200 distinct large plain-text payloads (> MAX_NEGATIVE_CACHE_ENTRIES and > MAX_RENDERS)
    // cap `negative_entries` at 1,024 fixed-size 32-byte digests and consume 0 render budget.
    for idx in 0..1_200 {
        let plain = format!(
            "distinct-plain-{idx:04} {}",
            "diagnostic-free prose words ".repeat(40)
        );
        assert!(plain.len() >= MIN_CHARS * 4);
        assert!(
            router
                .route(&plain, 1, Some(4096), &mut writes, &mut errors)
                .is_none()
        );
    }
    assert_eq!(router.negative_entries, MAX_NEGATIVE_CACHE_ENTRIES);
    assert_eq!(router.outcomes.len(), MAX_NEGATIVE_CACHE_ENTRIES);
    assert_eq!(router.remaining_renders, MAX_RENDERS);

    // End-to-end: 1,100 distinct plain-text strings ahead of 128 compressible HTML fields
    // leave all 128 render slots available.
    let mut obj = serde_json::Map::new();
    for idx in 0..1_100 {
        obj.insert(
            format!("plain_{idx:04}"),
            Value::String(format!(
                "entry-{idx:04} {}",
                "ordinary narrative segment ".repeat(11)
            )),
        );
    }
    for idx in 0..MAX_RENDERS {
        obj.insert(
            format!("html_{idx:03}"),
            Value::String(rich(&format!("after-distinct-neg-{idx}"))),
        );
    }
    let doc = Value::Object(obj).to_string();
    let e2e_store = CountingStore::default();
    let outcome = compress(&doc, &e2e_store);
    assert_eq!(outcome.metrics.embedded_rewrites, MAX_RENDERS);
    assert_eq!(e2e_store.stashes.load(Ordering::Relaxed), MAX_RENDERS);
}

#[test]
fn serialized_json_outer_fallback_prevents_orphan_leaf_stash_writes_and_rolls_back_partial_failures()
 {
    let log = sample_cargo_failure_log();
    let log_key = compute_key(log.as_bytes());
    let plain_3800 = "plain diagnostic-free prose words ".repeat(112);
    assert!(plain_3800.chars().count() >= 3_800);
    // Sniffs as HTML (`<p>...</p>`) so Slot 0 (`log`) renders in memory before Slot 1 fails `saves_enough`.
    let low_savings_html_3800 = format!("<p>{plain_3800}</p>");

    for sibling in [&plain_3800, &low_savings_html_3800] {
        let envelope = serde_json::to_string(&json!([log, sibling])).unwrap();
        let standalone_doc = serde_json::to_string(&json!({
            "pad": "plain words ".repeat(1_500),
            "envelope": envelope,
        }))
        .unwrap();
        let standalone_store = CountingStore::default();
        let standalone = compress(&standalone_doc, &standalone_store);
        assert_eq!(standalone.operations, [JsonOperation::Truncation]);
        assert_eq!(standalone.metrics.embedded_rewrites, 0);
        // Only `pad` and `envelope` outer truncations are stashed: 0 inner BuildLog orphans.
        assert_eq!(standalone_store.stashes.load(Ordering::Relaxed), 2);
        assert!(standalone_store.retrieve(&log_key).unwrap().is_none());

        // When the same BuildLog appears again at the top level, it reuses the cached
        // in-memory rewrite, writes 1 Stash entry, and restores byte-for-byte.
        let reuse_doc = serde_json::to_string(&json!({
            "pad": "plain words ".repeat(1_500),
            "envelope": envelope,
            "top_stdout": log,
        }))
        .unwrap();
        let reuse_store = CountingStore::default();
        let reuse = compress(&reuse_doc, &reuse_store);
        assert_eq!(
            reuse.operations,
            [JsonOperation::Truncation, JsonOperation::EmbeddedBuildLog]
        );
        assert_eq!(reuse.metrics.embedded_rewrites, 1);
        assert_eq!(reuse_store.stashes.load(Ordering::Relaxed), 3);
        let parsed_reuse: Value = serde_json::from_str(&reuse.output).unwrap();
        let top_stdout = parsed_reuse["top_stdout"].as_str().unwrap();
        assert!(top_stdout.starts_with(BUILD_LOG_HEADER));
        let hashes = recovery_hashes(top_stdout, &RecoveryMethod::Shell);
        assert_eq!(hashes, [log_key.as_str()]);
        assert_eq!(
            reuse_store.retrieve(hashes[0]).unwrap().as_deref(),
            Some(log.as_str())
        );
    }

    // Doubly-nested JSON where inner JSON fits <= 4,096 chars after rewriting `log`,
    // while re-escaping quotes into outer JSON pushes outer string past 4,096 chars.
    let quote_heavy = "\"".repeat(1_500);
    let inner_json = serde_json::to_string(&json!({
        "build": log,
        "quotes": quote_heavy,
    }))
    .unwrap();
    let outer_json = serde_json::to_string(&json!({ "nested": inner_json })).unwrap();
    assert!(outer_json.chars().count() > 6_000);
    let double_doc = serde_json::to_string(&json!({
        "pad": "plain words ".repeat(1_500),
        "outer": outer_json,
    }))
    .unwrap();
    let double_store = CountingStore::default();
    let double_outcome = compress(&double_doc, &double_store);
    assert_eq!(double_outcome.operations, [JsonOperation::Truncation]);
    assert_eq!(double_outcome.metrics.embedded_rewrites, 0);
    assert_eq!(double_store.stashes.load(Ordering::Relaxed), 2);
    assert!(double_store.retrieve(&log_key).unwrap().is_none());

    // Partial Stash failure on the 2nd leaf of a multi-leaf serialized JSON rolls back
    // the 1st leaf's newly created Stash entry and reports 1 error.
    let multi_leaf = serde_json::to_string(&json!({
        "first": rich("rollback-leaf-one"),
        "second": rich("rollback-leaf-two"),
    }))
    .unwrap();
    let rollback_doc = serde_json::to_string(&json!({ "envelope": multi_leaf })).unwrap();
    let rollback_store = CountingStore {
        fail_on_call: Some(2),
        ..CountingStore::default()
    };
    let rollback_outcome = compress(&rollback_doc, &rollback_store);
    assert_eq!(rollback_outcome.output, rollback_doc);
    assert!(rollback_outcome.operations.is_empty());
    assert!(rollback_outcome.stash_writes.is_empty());
    assert_eq!(rollback_outcome.metrics.stash_errors, 1);
    assert_eq!(rollback_store.stashes.load(Ordering::Relaxed), 2);
    assert_eq!(rollback_store.deletes.load(Ordering::Relaxed), 1);
    assert_eq!(rollback_store.len(), 0);
}

#[test]
fn bom_and_blank_separator_lines_do_not_falsely_prune_embedded_tabular() {
    let mut csv = String::from(
        "\u{feff}\r\n\r\n\"order_id\",\"customer_region\",\"settlement_state\",\"currency\"\r\n",
    );
    let blank_run = "\r\n".repeat(35);
    for idx in 1..=55 {
        csv.push_str(&blank_run);
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!("\"ORD-{idx:05}\",\"ap-southeast-1\",\"settled-confirmed\",\"CNY\"\r\n"),
        );
    }
    csv.push_str(&blank_run);

    assert!(
        csv.chars().count() > 4_500,
        "raw chars={}",
        csv.chars().count()
    );
    assert!(
        tabular_min_chars_lower_bound(&csv) <= 4_096,
        "lower_bound={}",
        tabular_min_chars_lower_bound(&csv)
    );

    // Tier 1 (unbounded document) + Tier 2 (bounded fallback forced by large plain sibling)
    // both accept the compacted CSV within `truncate_strings_at` (4,096) and recover exact bytes.
    let input = serde_json::to_string(&json!({
        "pad": "plain words ".repeat(4_000),
        "export": csv,
    }))
    .unwrap();
    let store = CountingStore::default();
    let outcome = compress(&input, &store);

    assert_eq!(
        outcome.operations,
        [JsonOperation::Truncation, JsonOperation::EmbeddedTabular]
    );
    assert_eq!(outcome.metrics.embedded_rewrites, 1);
    assert_eq!(store.stashes.load(Ordering::Relaxed), 2);

    let parsed: Value = serde_json::from_str(&outcome.output).unwrap();
    let rendered = parsed["export"].as_str().unwrap();
    assert!(rendered.starts_with(TABULAR_HEADER), "{rendered}");
    assert!(
        rendered.chars().count() <= 4_096,
        "len={}",
        rendered.chars().count()
    );
    assert!(rendered.contains("ORD-00055,ap-southeast-1,settled-confirmed,CNY"));

    let mut restored_export = rendered.to_owned();
    assert_eq!(restore_string_in_place(&mut restored_export, &store), 1);
    assert_eq!(restored_export, csv);
}
