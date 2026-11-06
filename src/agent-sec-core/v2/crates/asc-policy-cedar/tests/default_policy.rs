//! The default policy stays within the schema and denies what it does not grant.

use asc_policy_cedar::default_policy::{DEFAULT_POLICY_TEXT, default_policy_source};
use asc_policy_cedar::schema::load_schema;
use asc_policy_cedar::{pst, validate};
use cedar_policy::{
    Authorizer, Context, Decision, Entities, EntityUid, PolicySet, Request, Schema,
};
use serde_json::{Value, json};

struct Fixture {
    schema: Schema,
    policies: PolicySet,
}

impl Fixture {
    fn load() -> Self {
        let source = default_policy_source();
        assert_eq!(source.text, DEFAULT_POLICY_TEXT);
        validate(&source).expect("the default policy satisfies the embedded schema");
        let schema = load_schema(&source.schema).unwrap();
        let policies = PolicySet::from_pst(pst(&source).unwrap()).unwrap();
        assert_eq!(policies.policies().count(), 4);
        Self { schema, policies }
    }

    fn file(&self, action: &str, path: &str, input: &Value) -> Decision {
        let action: EntityUid = format!(r#"SecCore::Action::"{action}""#).parse().unwrap();
        let entities = Entities::from_json_value(
            json!([{
                "uid": { "type": "SecCore::File", "id": path },
                "attrs": { "path": path },
                "parents": []
            }]),
            Some(&self.schema),
        )
        .unwrap();
        let context =
            Context::from_json_value(json!({ "input": input }), Some((&self.schema, &action)))
                .unwrap();
        let request = Request::new(
            r#"SecCore::Subject::"worker""#.parse().unwrap(),
            action,
            format!(r#"SecCore::File::"{path}""#).parse().unwrap(),
            context,
            Some(&self.schema),
        )
        .unwrap();
        let response = Authorizer::new().is_authorized(&request, &self.policies, &entities);
        assert_eq!(response.diagnostics().errors().count(), 0, "{path}");
        response.decision()
    }

    fn network(&self) -> Decision {
        let action: EntityUid = r#"SecCore::Action::"network.use""#.parse().unwrap();
        let context = Context::from_json_value(json!({}), Some((&self.schema, &action))).unwrap();
        let request = Request::new(
            r#"SecCore::Subject::"worker""#.parse().unwrap(),
            action,
            r#"SecCore::Network::"all""#.parse().unwrap(),
            context,
            Some(&self.schema),
        )
        .unwrap();
        let response =
            Authorizer::new().is_authorized(&request, &self.policies, &Entities::empty());
        assert_eq!(response.diagnostics().errors().count(), 0);
        response.decision()
    }
}

#[test]
fn default_policy_denies_certbot_private_keys_but_allows_public_certificates() {
    let fixture = Fixture::load();
    let read = json!({ "program": "/usr/bin/cat" });

    for path in [
        "/etc/letsencrypt/archive/example.com/privkey1.pem",
        "/etc/letsencrypt/archive/example.com/privkey2.pem",
        "/etc/letsencrypt/live/example.com/privkey.pem",
    ] {
        assert_eq!(
            fixture.file("file.read", path, &read),
            Decision::Deny,
            "{path}"
        );
    }
    for path in [
        "/etc/letsencrypt/archive/example.com/cert1.pem",
        "/etc/letsencrypt/archive/example.com/chain1.pem",
        "/etc/letsencrypt/live/example.com/fullchain.pem",
    ] {
        assert_eq!(
            fixture.file("file.read", path, &read),
            Decision::Allow,
            "{path}"
        );
    }
}

#[test]
fn default_policy_grants_the_baseline_and_denies_everything_else() {
    let fixture = Fixture::load();
    let read = json!({ "program": "/usr/bin/cat" });
    let write = json!({
        "program": "/usr/bin/touch",
        "operation": { "__entity": { "type": "SecCore::WriteOperation", "id": "write" } }
    });
    let exec = json!({ "cwd": "/workspace", "arg_count": 0, "args": [] });

    let cases: &[(&str, &str, &Value, Decision)] = &[
        ("file.read", "/usr/bin/ls", &read, Decision::Allow),
        ("file.read", "/etc/hosts", &read, Decision::Allow),
        ("file.read", "/etc/shadow", &read, Decision::Deny),
        (
            "file.read",
            "/etc/ssh/ssh_host_ed25519_key",
            &read,
            Decision::Deny,
        ),
        (
            "file.read",
            "/etc/pki/tls/private/server.key",
            &read,
            Decision::Deny,
        ),
        ("file.read", "/root/.ssh/id_rsa", &read, Decision::Deny),
        ("file.read", "/etc/crypttab", &read, Decision::Deny),
        // procfs memory and magic links must not escape the forbids above.
        ("file.read", "/proc/kcore", &read, Decision::Deny),
        ("file.read", "/proc/kallsyms", &read, Decision::Deny),
        ("file.read", "/proc/1/auxv", &read, Decision::Deny),
        ("file.read", "/proc/1/mem", &read, Decision::Deny),
        ("file.read", "/proc/1/fd/3", &read, Decision::Deny),
        (
            "file.read",
            "/proc/1/root/etc/shadow",
            &read,
            Decision::Deny,
        ),
        ("file.read", "/proc/1/cwd/secret.txt", &read, Decision::Deny),
        ("file.read", "/proc/1/exe", &read, Decision::Deny),
        ("file.read", "/proc/1/maps", &read, Decision::Deny),
        ("file.read", "/proc/1/smaps", &read, Decision::Deny),
        // The bare magic-link directories and map_files stay unreadable too.
        ("file.read", "/proc/1/root", &read, Decision::Deny),
        ("file.read", "/proc/1/fd", &read, Decision::Deny),
        ("file.read", "/proc/1/cwd", &read, Decision::Deny),
        ("file.read", "/proc/1/map_files", &read, Decision::Deny),
        (
            "file.read",
            "/proc/1/map_files/7f0a00000000-7f0a00001000",
            &read,
            Decision::Deny,
        ),
        // The wildcard crosses directory boundaries, so the task, self and
        // thread-self aliases hit the same forbids as the direct paths.
        ("file.read", "/proc/1/task/1/exe", &read, Decision::Deny),
        ("file.read", "/proc/self/fd/3", &read, Decision::Deny),
        ("file.read", "/proc/thread-self/maps", &read, Decision::Deny),
        // Plain process information stays readable like in a container baseline.
        ("file.read", "/proc/1/status", &read, Decision::Allow),
        ("file.read", "/proc/1/cmdline", &read, Decision::Allow),
        // The workdir stays unlisted, so nothing inside it is reachable yet.
        (
            "file.write",
            "/workspace/src/main.rs",
            &write,
            Decision::Deny,
        ),
        ("file.write", "/tmp/out.log", &write, Decision::Allow),
        ("file.write", "/etc/passwd", &write, Decision::Deny),
        ("file.exec", "/usr/bin/make", &exec, Decision::Allow),
        ("file.exec", "/workspace/run.sh", &exec, Decision::Deny),
        ("file.exec", "/tmp/payload", &exec, Decision::Deny),
    ];
    for (action, path, input, expected) in cases {
        assert_eq!(fixture.file(action, path, input), *expected, "{path}");
    }
    assert_eq!(fixture.network(), Decision::Deny);
}
