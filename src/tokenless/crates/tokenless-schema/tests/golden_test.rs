//! Schema input/output examples; use `-- --nocapture` to display comparisons.
//! Regenerate intentional changes with `REGEN_GOLDENS=1`, then review the diff.

use std::{fs, path::PathBuf};

use serde_json::Value;
use tokenless_schema::SchemaCompressor;

fn print_case(category: &str, name: &str, input: &str, output: &str) {
    let border = "=".repeat(88);
    let divider = "-".repeat(88);
    let mut display = format!("\n\n{border}\n  GOLDEN CASE | {category} | {name}\n{border}\n");
    for (label, text) in [("INPUT / BEFORE", input), ("OUTPUT / AFTER", output)] {
        display.push_str(&format!(
            "\n  {label}  [{} bytes, {} lines]\n{divider}\n",
            text.len(),
            text.lines().count()
        ));
        for line in text.lines() {
            display.push_str("  | ");
            display.push_str(line);
            display.push('\n');
        }
        display.push_str(&format!("{divider}\n  END {label}\n"));
    }
    display.push_str(&format!("\n  END CASE | {category} | {name}\n{border}\n"));
    print!("{display}");
}

#[test]
fn schema_goldens() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schemas");
    for name in [
        "simple_calculator",
        "github_create_issue",
        "hubspot_contact",
        "stripe_payment",
        "slack_send_message",
        "aws_describe_instances",
    ] {
        let input_path = directory.join(format!("{name}.json"));
        let input = fs::read_to_string(&input_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", input_path.display()));
        let value: Value = serde_json::from_str(&input).unwrap();
        let compressor = SchemaCompressor::new();
        let mut compressed = compressor.compress(&value);
        assert_eq!(compressed, compressor.compress(&value));
        // Dependency feature unification can change map insertion order.
        compressed.sort_all_objects();
        let output = serde_json::to_string_pretty(&compressed).unwrap() + "\n";
        print_case("SCHEMA", name, &input, &output);
        let path = directory.join(format!("{name}.expected.json"));
        if std::env::var_os("REGEN_GOLDENS").is_some() {
            fs::write(path, &output).unwrap();
        } else {
            let expected = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            assert_eq!(output, expected, "{name}");
        }
        if name == "simple_calculator" {
            assert_eq!(compressed, value, "compact schemas need no transformation");
        } else {
            assert!(
                serde_json::to_string(&compressed).unwrap().len()
                    < serde_json::to_string(&value).unwrap().len()
            );
        }
    }
}
