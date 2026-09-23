# AgentSight test fixtures

## `policies/` — ActPlane YAML policy corpus

Verbatim copy of `test/policies/` from
[eunomia-bpf/ActPlane](https://github.com/eunomia-bpf/ActPlane) at rev
`a62e5d9d96f91101cda019519053e950d532380a` (MIT) — the same rev that
`src/agentsight/Cargo.toml` pins `actplane-ifc-compiler` to and redirects onto
`crates/actplane-ifc-compiler` through `[patch]`.

`crates/actplane-ifc-compiler` resolves its corpus as
`CARGO_MANIFEST_DIR/../../test/policies`. Inside upstream ActPlane, where the
crate lives at `crates/actplane-ifc-compiler`, that path is the committed
corpus; the crate was vendored here in 8574ecb02 without it, so the path landed
on a directory that never existed and both corpus tests
(`policy_corpus_files_compile`, `domain_policy_corpus_all_domains_compile`)
panicked on every run. This directory completes that vendoring: `src/agentsight/`
mirrors the upstream repository root (compare the crate's
`readme = "../../README.md"`), so the unmodified relative path resolves again.

Refresh from an ActPlane checkout with:

```bash
git -C <ActPlane> archive <rev> test/policies | tar -x -C src/agentsight
```

Keep the corpus rev in step with the `actplane-ifc-compiler` pin. The tests
assert that every policy in the corpus compiles and that the fixed-size
`taint_config` blob stays 74760 bytes, so a corpus taken from a different
upstream rev can fail legitimately.

`policies/invalid/` holds upstream's negative fixtures. Nothing in this
repository reads them yet; they are vendored so the tree matches upstream and a
later vendored test that expects them finds them.

This README is the only file under `src/agentsight/test/` that is not upstream's.
