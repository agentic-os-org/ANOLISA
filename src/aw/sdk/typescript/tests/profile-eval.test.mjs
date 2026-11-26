import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  intersectHooks,
  parseFieldPath,
  readField,
  replaceField,
  validateProfile,
  versionMatches,
} from "../src/profile-eval.mjs";
import { ProfileSource } from "../src/profile-source.mjs";
import { field, hook, profile, target } from "./profile-fixture.mjs";

const invalid = { code: "source_profile_invalid" };
const vectors = JSON.parse(
  readFileSync(
    new URL(
      "../../../crates/aw-contracts/tests/fixtures/instrumentation-profile-vectors.json",
      import.meta.url,
    ),
  ),
);
test("matches the shared JS contract and its R1–R15 rejection vectors", () => {
  for (const vector of vectors.valid.filter(
    ({ document }) => document.runtime === "js",
  ))
    assert.deepEqual(
      validateProfile(vector.document),
      vector.document,
      vector.name,
    );
  // The Rust vectors also cover Python. Reuse language-neutral rejections
  // with a valid JS runtime so they reach the invariant under test.
  for (const vector of vectors.invalid) {
    if (/python|invalid_runtime/.test(vector.name)) continue;
    const document = { ...vector.document, runtime: "js" };
    assert.throws(() => validateProfile(document), invalid, vector.name);
  }
  assert.throws(
    () => validateProfile(profile([target("f")], { runtime: "python" })),
    invalid,
  );
  assert.throws(
    () => validateProfile(profile([target("f")], { extra: true })),
    invalid,
  );
  assert.deepEqual(parseFieldPath("arg.31.a.b.c.d.e.1023").parts, [
    "a",
    "b",
    "c",
    "d",
    "e",
    "1023",
  ]);
  for (const path of [
    "arg.00",
    "receiver.a.constructor",
    "arg.0.__proto__",
    "kwarg.input",
  ])
    assert.throws(() => parseFieldPath(path), invalid);
});

test("npm package names and subpaths bind the fingerprint to the declared package", () => {
  for (const name of ["a".repeat(214), "@x/" + "a".repeat(211)]) {
    const document = profile([target("f", { module: name + "/SDK-v2.js" })], {
      package: { name, versions: ">=1" },
    });
    assert.equal(validateProfile(document).package.name, name);
    document.package.name += "x";
    assert.throws(() => validateProfile(document), invalid);
  }
  for (const module of [
    "profile-fixturex",
    "profile-fixture/../other",
    "profile-fixture/a//b",
    "./profile-fixture",
    "/absolute",
    "https://example.com/package",
    "node:fs",
  ]) {
    assert.throws(
      () => validateProfile(profile([target("f", { module })])),
      invalid,
    );
  }
});

test("R16 rejects duplicate escaped keys, ambiguous integers, and oversized or malformed input", () => {
  const value = profile([target("f")]);
  const json = JSON.stringify(value);
  assert.deepEqual(validateProfile(Buffer.from(json)), value);
  assert.throws(
    () =>
      validateProfile(json.replace('"mode"', '"m\\u006fde":"observe","mode"')),
    invalid,
  );
  assert.throws(
    () => validateProfile(json.replace('"id"', '"id":"first","id"')),
    invalid,
  );
  assert.throws(() => validateProfile(json + " ".repeat(1024 * 1024)), invalid);
  assert.throws(() => validateProfile(Buffer.from([0xc0, 0xa1])), invalid);
  const stream = profile([
    target("f", {
      call: "async_iter",
      stream: {
        buffer: true,
        max_items: 2,
        max_bytes: 100,
      },
    }),
  ]);
  assert.throws(
    () =>
      validateProfile(
        JSON.stringify(stream).replace('"max_items":2', '"max_items":2.0'),
      ),
    invalid,
  );
});

test("intersects Hook capabilities across all declared seams", () => {
  const fields = {
    input: field("arg.0", "json"),
    tool_name: { const: "tool" },
  };
  const hooks = intersectHooks(
    validateProfile(
      profile([
        target("a", {
          fields,
          hooks: {
            "tool.before": hook(
              ["block", "replace_input"],
              ["block", "report"],
            ),
          },
        }),
        target("b", {
          fields,
          hooks: { "tool.before": hook(["block"], ["block"]) },
        }),
        target("c", { fields, hooks: { "tool.before": hook([], ["report"]) } }),
      ]),
    ).targets,
  );
  assert.deepEqual(hooks, {
    "tool.before": {
      mode: "observe",
      effects: ["observe"],
      failure_actions: [],
    },
  });
});

test("the seven valid unwired Hooks report semantic_event_unavailable", async () => {
  for (const [event, fields] of [
    ["permission.request", { tool_name: { const: "tool" } }],
    ["compact.before", {}],
    ["compact.after", {}],
    ["subagent.start", {}],
    ["subagent.stop", {}],
    ["model.before_request", {}],
    ["security.violation", { output: field("result") }],
  ]) {
    await assert.rejects(
      ProfileSource.probe(
        profile([target("f", { fields, hooks: { [event]: hook() } })]),
      ),
      { code: "semantic_event_unavailable" },
      event,
    );
  }
});

test("numeric comparators compare every bound and pad trailing zero components", () => {
  assert(versionMatches("1.20.3", ">=1,<2,>1.2,<=1.20.3.0,==1.20.03"));
  assert(versionMatches("90071992547409919", ">90071992547409918"));
  assert(!versionMatches("1.20.3", ">=1,<1.20.3"));
  assert(!versionMatches("1.20.3-rc.1", ">=1"));
  assert.throws(() => versionMatches("1", ">=1, <2"), invalid);
});

test("evaluates own data fields without getters and rebuilds JSON and JSON text", () => {
  const { signal } = new AbortController();
  const args = [
    { signal, payload: { items: [{ command: "raw" }] } },
    { payload: '{"command":"raw"}' },
  ];
  const receiver = {
    get sessionId() {
      throw new Error("getter executed");
    },
  };
  const context = { args, receiver, result: { value: false } };
  assert.equal(readField(field("receiver.sessionId"), context), undefined);
  assert.equal(readField(field("result.value"), context), "false");
  const input = field("arg.0.payload.items.0", "json");
  assert.deepEqual(readField(input, context), { command: "raw" });
  const rebuilt = replaceField(input, context, { command: "safe" }, "arg");
  assert.equal(rebuilt[0].payload.items[0].command, "safe");
  assert.equal(rebuilt[0].signal, signal);
  assert.equal(args[0].payload.items[0].command, "raw");
  const encoded = field("arg.1.payload", "json_string");
  assert.deepEqual(readField(encoded, context), { command: "raw" });
  assert.equal(
    replaceField(encoded, context, { command: "safe" }, "arg")[1].payload,
    '{"command":"safe"}',
  );
});

test("turn.stop output is contract-valid but unavailable at SDK ingress", async () => {
  const document = profile([
    target("finish", {
      hooks: { "turn.stop": hook() },
      fields: { output: field("result") },
    }),
  ]);
  assert.deepEqual(validateProfile(document), document);
  await assert.rejects(ProfileSource.probe(document), {
    code: "source_field_unavailable",
  });
});
