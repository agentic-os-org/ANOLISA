use super::{example, parse, Error};
use serde_json::json;

#[test]
fn plaintext_http_requires_exact_authorities_on_a_claude_sdk_target() {
    let mut value = example();
    let hosts = json!([
        "Model.EXAMPLE.",
        "Model.EXAMPLE",
        "Model.EXAMPLE.:80",
        "Model.EXAMPLE:80",
        "model:00080",
        "192.0.2.1",
        "[fd00::1]:8080",
        "fd00::2"
    ]);
    value["spec"]["agents"]["sdk"] = json!({"adapter":"claude-sdk", "argv":["python","worker.py"],
        "claude_sdk":{"allow_plaintext_http_hosts":hosts}});
    assert!(parse(&value).is_ok());
    for entry in [
        "",
        " ",
        "host name",
        "*.test",
        ".",
        ".example",
        "model..example",
        "192.0.2.1.",
        "192.0.2.1.:80",
        "-host",
        "host..",
        "http://host",
        "https://host",
        "host/path",
        "host?x",
        "host#x",
        "user@host",
        "192.0.2.0/24",
        "host:",
        "host:0",
        "host:65536",
        "host:+80",
        "host:abc",
        "[::1]suffix",
        "[::1]:",
        "[host]",
        "999.0.0.1",
        "fe80::1%eth0",
    ] {
        value["spec"]["agents"]["sdk"]["claude_sdk"]["allow_plaintext_http_hosts"] = json!([entry]);
        assert!(parse(&value).is_err(), "{entry:?}");
    }
    value["spec"]["agents"]["sdk"]["claude_sdk"] = json!({});
    assert!(parse(&value).is_ok());
    value["spec"]["agents"]["sdk"]["adapter"] = json!("pi");
    assert!(
        matches!(parse(&value), Err(Error::Invariant {path,..}) if path.ends_with("/claude_sdk"))
    );
}
