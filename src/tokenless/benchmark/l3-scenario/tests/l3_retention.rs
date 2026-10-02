// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Retention extraction against the committed assets.
//!
//! Unit tests cover the extraction rules on synthetic payloads; these check the
//! rules behave sensibly on the real scenarios, where a mis-derived critical
//! item would silently distort every retention number in the report.

use std::path::PathBuf;

use tokenless_l3_bench::l3::{asset, retention};

fn assets() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// Print what each scenario yields, for diagnosing a suspicious retention score.
///
/// Run with `cargo test --test l3_retention -- --nocapture`.
#[test]
fn extraction_inventory_over_real_assets() {
    let scenarios = asset::load_all(&assets()).expect("assets load");
    for s in &scenarios {
        let items = retention::critical_items(s);
        let mut kinds: Vec<&str> = items.iter().map(|i| i.kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        println!(
            "{:<9} {:<20} items={:<4} kinds={:?}",
            s.content_type,
            s.scenario,
            items.len(),
            kinds
        );
        for item in items.iter().take(6) {
            println!("      {:<22} {}", item.kind, item.check);
        }
    }
}

/// The uncompressed conversation must retain everything, on every scenario.
///
/// A failure here means an extraction rule invented a needle that is not
/// actually present in its own source payload, which would make every
/// compressed-side score meaningless.
#[test]
fn originals_retain_all_their_own_critical_items() {
    let scenarios = asset::load_all(&assets()).expect("assets load");
    for s in &scenarios {
        let items = retention::critical_items(s);
        let score = retention::check(&items, &s.messages, s.tools.as_deref());
        assert_eq!(
            score.passed, score.total,
            "{} lost items in its own original: {:?}",
            s.scenario, score.missing
        );
    }
}
