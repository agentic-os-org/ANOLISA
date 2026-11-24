//! Public audit assertions for the Codex launcher fixture.

use super::support::Fixture;
use serde_json::{json, Value};
use std::fs;

fn after_page(f: &Fixture) -> Option<Value> {
    let request = f.root.join("audit-request.json");
    let query = |operation| {
        fs::write(
            &request,
            json!({"api_version":aw_service::audit_export::VERSION,"operation":operation})
                .to_string(),
        )
        .unwrap();
        let mut command = f.command();
        command
            .args(["audit-export", "--state-dir"])
            .arg(f.root.join("state"))
            .arg("--request")
            .arg(&request);
        let output = f.run(command);
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            output.status.success(),
            response["error"].is_null(),
            "{response}"
        );
        response
    };
    let inventory = query(json!({"type":"enumerate","cursor":null,"limit":64}));
    assert!(inventory["error"].is_null(), "{inventory}");
    let inventory = &inventory["result"];
    assert!(inventory["next_cursor"].is_null(), "{inventory}");
    let mut after = Vec::new();
    let (mut prompts, mut before) = (0, 0);
    for key in inventory["keys"].as_array().unwrap() {
        let response = query(json!({"type":"read","chain_id":key,"cursor":null,"limit":64}));
        if response["error"] == "unsupported_history" {
            continue;
        }
        if response["error"] == "projection_unavailable" {
            assert!(response["result"].is_null(), "{response}");
            let journal = aw_core::journal::FileJournal::new(f.root.join("state/journal")).unwrap();
            let records = journal.read(key.as_str().unwrap()).unwrap();
            let plan = &records[0]["record"]["plan"];
            assert_eq!(plan["event"]["name"], "tool.after");
            assert_eq!(
                plan["publicProjectionGap"],
                "aw.native_metadata_unavailable"
            );
            after.push(None);
            continue;
        }
        assert!(response["error"].is_null(), "{response}");
        let page = &response["result"];
        match page["event"]["data"]["name"].as_str().unwrap() {
            "input.submit" => prompts += 1,
            "tool.before" => before += 1,
            "tool.after" => after.push(Some(page.clone())),
            _ => continue,
        }
        assert_eq!(page["caught_up"], true);
        assert_eq!(page["terminal"], true);
        assert_eq!(page["event"]["kind"], "ExecutionEvent");
    }
    assert_eq!((prompts, before, after.len()), (1, 1, 1));
    after.pop().unwrap()
}

pub(super) fn assert_after_audit_unavailable(f: &Fixture) {
    assert!(after_page(f).is_none());
}

pub(super) fn assert_after_audit(
    f: &Fixture,
    result: &Value,
    status: &str,
    failure_action: Option<&str>,
) {
    let page = after_page(f).expect("expected a public tool.after event");
    let details = &page["event"]["data"]["details"];
    assert_eq!(details["resultStatus"], status);
    let digest = aw_contracts::canonical::digest(&serde_json::to_vec(result).unwrap());
    assert_eq!(
        details["resultDigest"],
        json!({"algorithm":"sha256","value":digest})
    );
    let completed = page["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["data"]["kind"] == "provider.completed")
        .unwrap();
    assert_eq!(
        completed["data"]["details"]["failureAction"],
        json!(failure_action)
    );
}
