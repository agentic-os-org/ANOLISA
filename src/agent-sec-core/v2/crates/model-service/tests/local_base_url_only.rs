//! Verifies the environment wiring that keeps the model service host-local.
//!
//! Lives outside `lib.rs` because it drives the public `create_client`, which
//! reads the environment, whereas the unit tests there exercise the private
//! `validate_base_url` directly.  Kept as one test in its own binary because
//! `set_var` mutates process-global state: a single `#[test]` leaves no
//! sibling thread able to observe the variable mid-change.

use model_service::create_client;

const ENV_BASE_URL: &str = "AGENT_SEC_MODEL_SERVICE_BASE_URL";

#[test]
fn only_a_loopback_base_url_is_accepted() {
    // The loopback default is usable with nothing set.
    std::env::remove_var(ENV_BASE_URL);
    assert!(
        create_client().is_ok(),
        "the default loopback base_url must work out of the box"
    );

    // A hijacked variable pointing off-host fails closed.
    std::env::set_var(ENV_BASE_URL, "http://attacker.example:18099");
    // `Box<dyn ModelClient>` is not `Debug`, so match rather than `expect_err`.
    let Err(error) = create_client() else {
        panic!("non-loopback base_url must be refused");
    };
    let message = error.to_string();
    assert!(
        message.contains("attacker.example"),
        "the refusal must name the offending host; got {message:?}"
    );

    // Userinfo that mimics loopback does not smuggle a remote host through:
    // `ureq` would have sent the body to whatever follows '@'.
    std::env::set_var(ENV_BASE_URL, "http://localhost:11434@attacker.example");
    let Err(error) = create_client() else {
        panic!("a base_url whose real host follows '@' must be refused");
    };
    assert!(
        error.to_string().contains("attacker.example"),
        "the refusal must name the offending URL; got {error}"
    );

    // A local service on a non-default port stays usable.
    std::env::set_var(ENV_BASE_URL, "http://127.0.0.1:18099");
    assert!(
        create_client().is_ok(),
        "a non-default loopback port must remain usable"
    );
}
