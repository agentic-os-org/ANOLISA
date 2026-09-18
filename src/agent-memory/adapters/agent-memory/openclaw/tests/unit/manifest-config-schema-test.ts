/**
 * Manifest ↔ config-resolution drift guard.
 *
 * OpenClaw validates `plugins.entries.memory-anolisa.config` against this
 * plugin's `openclaw.plugin.json` `configSchema` *before* the plugin runtime
 * loads (host docs, `docs/plugins/manifest.md` → "JSON Schema requirements":
 * "Bundled plugin schemas are strict, so adding
 * `plugins.entries.<id>.config.myNewKey` in user config without adding
 * `myNewKey` to `configSchema.properties` will be rejected before the plugin
 * runtime loads"). The loader then skips the plugin outright — one undeclared
 * key costs the whole memory backend, not that one setting.
 *
 * The schema is strict (`additionalProperties: false`), which is the right
 * choice: it turns an operator's typo into a config error instead of a silently
 * ignored key. The price is that every key `resolveConfig` reads must also be
 * declared, and nothing used to check that. `sessionId` / `sessionDir` shipped
 * undeclared while the user guide documented both as plugin-config keys, so the
 * documented configuration was exactly the one the host rejected.
 *
 * These tests derive the consumed key set from `src/config.ts` rather than
 * restating it, so adding a key to the resolver without declaring it in the
 * manifest fails here. Both directions are asserted: a refactor that changes how
 * the resolver reads `api.pluginConfig` empties the derived set, and the
 * reverse-direction assertion then fails instead of passing vacuously.
 */

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const manifestPath = fileURLToPath(
  new URL("../../openclaw.plugin.json", import.meta.url),
);
const configSourcePath = fileURLToPath(new URL("../../src/config.ts", import.meta.url));

type SchemaProperty = {
  type?: string;
  description?: string;
  maxLength?: number;
  enum?: string[];
};

type Manifest = {
  id: string;
  configSchema: {
    type?: string;
    additionalProperties?: boolean;
    properties?: Record<string, SchemaProperty>;
  };
  uiHints?: Record<string, { label?: string; help?: string }>;
};

const manifest = JSON.parse(readFileSync(manifestPath, "utf8")) as Manifest;
const properties = manifest.configSchema.properties ?? {};
const declaredKeys = Object.keys(properties);

/** Config keys `resolveConfig` reads off `api.pluginConfig` (its `raw` local). */
function consumedKeys(): Set<string> {
  const source = readFileSync(configSourcePath, "utf8");
  const keys = new Set<string>();
  for (const match of source.matchAll(/\braw\.([A-Za-z_$][A-Za-z0-9_$]*)/g)) {
    keys.add(match[1]!);
  }
  return keys;
}

/** The plugin-config table documented in
 *  `docs/user-guide/{en,zh}/token-saving/agent-memory.md`. */
const DOCUMENTED_CONFIG: Record<string, unknown> = {
  binaryPath: "/usr/bin/agent-memory",
  userId: "1000",
  profile: "advanced",
  maxReadBytes: 1_048_576,
  maxWriteBytes: 16_777_216,
  sessionId: "ses_pinned_001",
  sessionDir: "/run/anolisa/sessions",
};

describe("openclaw.plugin.json configSchema", () => {
  it("stays strict, which is what makes an undeclared key fatal", () => {
    assert.equal(manifest.configSchema.type, "object");
    assert.equal(manifest.configSchema.additionalProperties, false);
  });

  it("declares every key resolveConfig reads", () => {
    const consumed = consumedKeys();
    assert.ok(consumed.size > 0, "no `raw.<key>` reads found in src/config.ts");
    const undeclared = [...consumed].filter((key) => !(key in properties)).sort();
    assert.deepEqual(
      undeclared,
      [],
      `resolveConfig reads ${undeclared.join(", ")} but configSchema.properties ` +
        `does not declare ${undeclared.length === 1 ? "it" : "them"}; the host ` +
        `rejects the whole plugin config and skips loading the plugin`,
    );
  });

  it("declares no key resolveConfig ignores", () => {
    const consumed = consumedKeys();
    const dead = declaredKeys.filter((key) => !consumed.has(key)).sort();
    assert.deepEqual(
      dead,
      [],
      `configSchema declares ${dead.join(", ")} but resolveConfig never reads ` +
        `${dead.length === 1 ? "it" : "them"} — either wire it up or drop the ` +
        `declaration and this guard's derived set`,
    );
  });

  it("gives every declared key a type and a description", () => {
    for (const key of declaredKeys) {
      const property = properties[key]!;
      assert.ok(property.type, `${key}: configSchema property needs a "type"`);
      assert.ok(
        property.description,
        `${key}: configSchema property needs a "description" (the host surfaces it in config validation errors and the Control UI)`,
      );
    }
  });

  it("hints every declared key so the Control UI can render it", () => {
    const hints = manifest.uiHints ?? {};
    const unhinted = declaredKeys.filter((key) => !(key in hints)).sort();
    assert.deepEqual(unhinted, [], `uiHints is missing ${unhinted.join(", ")}`);
    for (const key of declaredKeys) {
      assert.ok(hints[key]!.label, `${key}: uiHints entry needs a "label"`);
      assert.ok(hints[key]!.help, `${key}: uiHints entry needs a "help"`);
    }
    const stray = Object.keys(hints)
      .filter((key) => !declaredKeys.includes(key))
      .sort();
    assert.deepEqual(stray, [], `uiHints has entries for undeclared keys: ${stray.join(", ")}`);
  });

  it("leaves the resolver's byte bound stricter than the schema's code-point bound", async () => {
    // JSON Schema `maxLength` counts code points and has no byte-length
    // keyword, while `validate_user_id` counts UTF-8 bytes. Bytes are always
    // >= code points, so the schema can only ever be the weaker of the two:
    // it never rejects a value the resolver would accept, and a multibyte id
    // that slips past it is stopped by the resolver at plugin boot instead of
    // being silently replaced inside the subprocess. Pinning the direction
    // keeps anyone from "fixing" the mismatch by loosening the resolver.
    const { validateUserId } = await import("../../src/config.js");
    const limit = properties.sessionId?.maxLength ?? properties.userId?.maxLength;
    assert.ok(
      typeof limit === "number" && limit > 0,
      "expected a numeric maxLength on sessionId/userId to compare against",
    );
    const multibyte = "\u5b57".repeat(limit); // 3 bytes per code point
    assert.equal([...multibyte].length, limit);
    assert.equal(Buffer.byteLength(multibyte, "utf8"), limit * 3);
    assert.throws(() => validateUserId(multibyte), /exceeds 128 bytes/);
  });

  it("caps sessionId exactly like userId, which is what validates it", () => {
    // resolveSessionId() runs explicit config through validateUserId(), so the
    // two schema entries must not drift apart.
    assert.equal(properties.sessionId?.maxLength, properties.userId?.maxLength);
  });

  it("offers exactly the profiles the resolver can honor", async () => {
    // Both directions matter. An enum value the resolver rejects sends the
    // operator to a plugin that fails during register — the Control UI offers
    // it, the host validates it, and the memory slot still does not come up. A
    // value the resolver accepts but the enum omits is unreachable from config
    // the host validates. The resolver's own list is the source of truth, so
    // this reads it instead of restating it.
    const { SUPPORTED_PROFILES, resolveConfig } = await import("../../src/config.js");
    // Typed as string[] on purpose: the deepEqual below narrows the schema's
    // enum to the resolver's union, and the point of this guard is that
    // "expert" is *not* in it.
    const declaredProfiles: string[] = properties.profile?.enum ?? [];
    assert.ok(
      !declaredProfiles.includes("expert"),
      "the schema must not offer 'expert': the child hides the Tier B tools " +
        "this adapter's memory contract is built on, so resolveConfig rejects it",
    );
    assert.deepEqual(declaredProfiles, [...SUPPORTED_PROFILES]);
    for (const profile of SUPPORTED_PROFILES) {
      const cfg = resolveConfig({
        pluginConfig: { ...DOCUMENTED_CONFIG, profile, binaryPath: process.execPath },
        resolvePath: (p: string) => p,
        logger: { info: () => {}, warn: () => {}, debug: () => {} },
      } as never);
      assert.equal(cfg.profile, profile);
    }
  });
});

describe("documented plugin config", () => {
  it("passes the strict schema the host validates it against", () => {
    // With additionalProperties: false and no `required` list, a flat config
    // object of correctly-typed values is accepted iff every key is declared.
    const undeclared = Object.keys(DOCUMENTED_CONFIG)
      .filter((key) => !(key in properties))
      .sort();
    assert.deepEqual(
      undeclared,
      [],
      `the user guide documents ${undeclared.join(", ")} as plugin config but the ` +
        `manifest does not declare ${undeclared.length === 1 ? "it" : "them"}`,
    );
  });

  it("reaches the resolver instead of being rejected at the host", async () => {
    const { resolveConfig } = await import("../../src/config.js");
    delete process.env["MEMORY_SESSION_ID"];
    delete process.env["MEMORY_SESSION_DIR"];
    // `binaryPath` points at a binary that exists (this test runner) so the
    // assertion is about the session keys, not about agent-memory being
    // installed on the machine running the suite.
    const pluginConfig = { ...DOCUMENTED_CONFIG, binaryPath: process.execPath };
    const api = {
      pluginConfig,
      resolvePath: (p: string) => p,
      logger: { info: () => {}, warn: () => {}, debug: () => {} },
    } as never;
    const cfg = resolveConfig(api);
    assert.equal(cfg.sessionId, DOCUMENTED_CONFIG.sessionId);
    assert.equal(cfg.sessionDir, DOCUMENTED_CONFIG.sessionDir);
    assert.equal(cfg.userId, DOCUMENTED_CONFIG.userId);
    assert.equal(cfg.binaryPath, process.execPath);
  });
});
