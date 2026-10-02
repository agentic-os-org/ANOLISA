import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { isCapabilityEnabled } from "../../src/registration.js";
import type { SecurityCapability } from "../../src/types.js";
import { skillLedger } from "../../src/capabilities/skill-ledger.js";
import { piiScan } from "../../src/capabilities/pii-scan.js";

const OPENCLAW_COMPAT_FLOOR = ">=2026.4.14";

function capability(id: string): SecurityCapability {
  return {
    id,
    name: id,
    hooks: [],
    register: () => {},
  };
}

function readJson(path: string): Record<string, any> {
  return JSON.parse(readFileSync(resolve(path), "utf8")) as Record<string, any>;
}

describe("capability registration defaults", () => {
  it("enables capabilities by default", () => {
    assert.equal(isCapabilityEnabled(capability("scan-code"), {}), true);
  });

  it("enables skill-ledger by default", () => {
    assert.equal(isCapabilityEnabled(skillLedger, {}), true);
  });

  it("lets explicit config disable capabilities", () => {
    assert.equal(
      isCapabilityEnabled(capability("prompt-scan"), {
        "prompt-scan": { enabled: false },
      }),
      false,
    );
  });

  it("lets hook environment switches override capability enabled values", () => {
    process.env.SKILL_LEDGER_HOOK_ENABLED = "true";
    process.env.PII_CHECKER_HOOK_ENABLED = "false";
    try {
      assert.equal(
        isCapabilityEnabled(skillLedger, { "skill-ledger": { enabled: false } }),
        true,
      );
      assert.equal(
        isCapabilityEnabled(piiScan, { "pii-scan-user-input": { enabled: true } }),
        false,
      );
    } finally {
      delete process.env.SKILL_LEDGER_HOOK_ENABLED;
      delete process.env.PII_CHECKER_HOOK_ENABLED;
    }
  });

  it("lets code scanner environment switches override capability enabled values", () => {
    process.env.CODE_SCANNER_HOOK_ENABLED = "true";
    try {
      assert.equal(
        isCapabilityEnabled(capability("scan-code"), { "scan-code": { enabled: false } }),
        true,
      );
      process.env.CODE_SCANNER_HOOK_ENABLED = "false";
      assert.equal(
        isCapabilityEnabled(capability("scan-code"), { "scan-code": { enabled: true } }),
        false,
      );
    } finally {
      delete process.env.CODE_SCANNER_HOOK_ENABLED;
    }
  });

  it("ignores invalid code scanner enabled values", () => {
    process.env.CODE_SCANNER_HOOK_ENABLED = "invalid";
    try {
      assert.equal(
        isCapabilityEnabled(capability("scan-code"), { "scan-code": { enabled: false } }),
        false,
      );
      assert.equal(
        isCapabilityEnabled(capability("scan-code"), { "scan-code": { enabled: true } }),
        true,
      );
    } finally {
      delete process.env.CODE_SCANNER_HOOK_ENABLED;
    }
  });

  it("does not give deprecated skill-ledger enableBlock a schema default", () => {
    const manifest = readJson("openclaw.plugin.json");
    const enableBlock =
      manifest.configSchema.properties.capabilities.properties[
        "skill-ledger"
      ].properties.enableBlock;

    assert.equal(Object.hasOwn(enableBlock, "default"), false);
  });

  it("defaults skill-ledger policy to ask", () => {
    const manifest = readJson("openclaw.plugin.json");
    const policy =
      manifest.configSchema.properties.capabilities.properties[
        "skill-ledger"
      ].properties.policy;

    assert.equal(policy.default, "ask");
  });

  it("defaults PII hook policy to observe", () => {
    const manifest = readJson("openclaw.plugin.json");
    const policy =
      manifest.configSchema.properties.capabilities.properties[
        "pii-scan-user-input"
      ].properties.policy;

    assert.equal(policy.default, "observe");
    assert.deepEqual(policy.enum, [
      "observe",
      "warn",
      "ask",
      "block",
      "debug",
      "deny",
    ]);
  });

  it("keeps deprecated policy aliases schema-valid", () => {
    const manifest = readJson("openclaw.plugin.json");
    const capabilities = manifest.configSchema.properties.capabilities.properties;

    for (const capabilityId of ["pii-scan-user-input", "skill-ledger"]) {
      assert.ok(capabilities[capabilityId].properties.policy.enum.includes("debug"));
      assert.ok(capabilities[capabilityId].properties.policy.enum.includes("deny"));
    }
  });

  it("declares the OpenClaw install and plugin API compatibility floor", () => {
    const packageManifest = readJson("package.json");

    assert.equal(
      packageManifest.openclaw.install.minHostVersion,
      OPENCLAW_COMPAT_FLOOR,
    );
    assert.equal(
      packageManifest.openclaw.compat.pluginApi,
      OPENCLAW_COMPAT_FLOOR,
    );
    assert.equal(
      packageManifest.peerDependencies.openclaw,
      OPENCLAW_COMPAT_FLOOR,
    );
  });

  it("declares packaged runtime plugin entry points", () => {
    const packageManifest = readJson("package.json");
    const manifest = readJson("openclaw.plugin.json");

    assert.deepEqual(packageManifest.openclaw.extensions, ["./dist/index.js"]);
    assert.deepEqual(packageManifest.openclaw.runtimeExtensions, [
      "./dist/index.js",
    ]);
    assert.deepEqual(manifest.extensions, ["./dist/index.js"]);
  });
});
