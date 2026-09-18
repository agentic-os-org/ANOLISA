/**
 * Real OpenClaw 2026.9.2 loader and conversation-policy regression.
 * Build the plugin, then run: node tests/host-policy-test.mjs /path/to/node_modules/openclaw
 * The version check pins the internal bundle exports used by this integration test.
 */
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const host = path.resolve(process.argv[2]);
assert.equal(
  JSON.parse(fs.readFileSync(path.join(host, "package.json"), "utf8")).version,
  "2026.9.2",
);
const runtime = pathToFileURL(path.join(host, "dist/"));
const { o: loadPlugins } = await import(new URL("loader-DPiOPJjR.js", runtime));
const { n: resolvePluginTools } = await import(
  new URL("tools-D1ohN2ZT.js", runtime)
);
const { o: loadSnapshot } = await import(
  new URL("manifest-contract-eligibility-BbV7X6pV.js", runtime)
);
const { t: resolveProfile } = await import(
  new URL("conversation-capability-profile-B6PkMXFD.js", runtime)
);
const { r: resolvePolicies, t: buildSteps } = await import(
  new URL("conversation-tool-policy-pipeline-BzM9yFTb.js", runtime)
);
const { t: applyPolicies } = await import(
  new URL("tool-policy-pipeline-BwOD5Xc9.js", runtime)
);
const { r: toolMeta } = await import(
  new URL("tool-metadata-B5aqo73s.js", runtime)
);
const { r: sandboxPolicy } = await import(
  new URL("tool-policy-WMCUEiT6.js", runtime)
);
const root = fs.mkdtempSync(path.join(os.tmpdir(), "memory-host-policy-"));
process.env.OPENCLAW_STATE_DIR = root;
process.env.OPENCLAW_CONFIG_PATH = path.join(root, "openclaw.json");
const manifest = JSON.parse(
  fs.readFileSync(new URL("../openclaw.plugin.json", import.meta.url), "utf8"),
);
const names = manifest.contracts.tools;
const readNames = ["anolisa_memory_search", "anolisa_memory_get"];
const config = {
  tools: { profile: "coding" },
  plugins: {
    allow: ["memory-core", "memory-anolisa"],
    load: { paths: [fileURLToPath(new URL("../", import.meta.url))] },
    slots: { memory: "memory-anolisa" },
    entries: {
      "memory-core": { enabled: true },
      "memory-anolisa": {
        enabled: true,
        config: { binaryPath: process.execPath, sessionDir: root },
      },
    },
  },
};
try {
  const snapshot = loadSnapshot({ config, workspaceDir: root });
  const registry = loadPlugins({ config, workspaceDir: root, cache: false });
  const originalSnapshot = {
    ...snapshot,
    plugins: snapshot.plugins.map((p) =>
      p.id === "memory-anolisa" ? { ...p, toolMetadata: undefined } : p,
    ),
  };
  const cases = [
    {
      label: "missing profile metadata hides all plugin contracts",
      snapshot: originalSnapshot,
      expected: [],
    },
    {
      label: "coding keeps every plugin contract and both host tools",
      expected: names,
      hostTools: true,
    },
    {
      label: "explicit tool deny wins",
      tools: { deny: [names[0]] },
      expected: names.filter((name) => name !== names[0]),
    },
    {
      label: "explicit plugin deny wins",
      tools: { deny: ["memory-anolisa"] },
      expected: [],
    },
    {
      label: "explicit core-only allow wins",
      tools: { allow: ["group:memory"] },
      expected: [],
    },
    {
      label: "minimal profile stays restricted",
      tools: { profile: "minimal" },
      expected: [],
    },
    { label: "default sandbox stays restricted", sandbox: true, expected: [] },
    {
      label: "sandbox group:memory does not alias ANOLISA names",
      sandbox: true,
      tools: { sandbox: { tools: { allow: ["group:memory"] } } },
      expected: [],
    },
    {
      label: "explicit sandbox alsoAllow enables all plugin contracts",
      sandbox: true,
      tools: { sandbox: { tools: { alsoAllow: names } } },
      expected: names,
    },
    {
      label: "sandbox read-only grant does not expose observe or context",
      sandbox: true,
      tools: { sandbox: { tools: { alsoAllow: readNames } } },
      expected: readNames,
    },
    {
      label: "sandbox deny wins over alsoAllow",
      sandbox: true,
      tools: { sandbox: { tools: { alsoAllow: names, deny: [names[0]] } } },
      expected: names.filter((name) => name !== names[0]),
    },
    {
      label: "older metadata path can use explicit alsoAllow",
      snapshot: originalSnapshot,
      tools: { alsoAllow: names },
      expected: names,
    },
  ];
  for (const c of cases) {
    const cfg = { ...config, tools: { ...config.tools, ...c.tools } };
    const before = JSON.stringify(cfg);
    const profile = resolveProfile({
      config: cfg,
      workspaceDir: root,
      agentId: "main",
      sessionKey: "agent:main:main",
      pluginMetadataSnapshot: c.snapshot ?? snapshot,
      sandboxToolPolicy: c.sandbox ? sandboxPolicy(cfg, "main") : undefined,
    });
    const tools = resolvePluginTools({
      context: {
        config: cfg,
        workspaceDir: root,
        agentId: "main",
        sandboxed: c.sandbox,
      },
      runtimeRegistry: registry,
      toolAllowlist: profile.policy.explicitToolAllowlist,
      toolDenylist: profile.policy.explicitToolDenylist,
    });
    const visible = applyPolicies({
      tools,
      toolMeta,
      warn() {},
      steps: buildSteps({
        capabilityProfile: profile,
        policies: resolvePolicies({ capabilityProfile: profile }),
      }),
    }).map((t) => t.name);
    assert.deepEqual(
      visible.filter((n) => names.includes(n)).sort(),
      [...c.expected].sort(),
      c.label,
    );
    if (c.hostTools)
      for (const n of ["memory_search", "memory_get", ...names])
        assert.equal(visible.filter((v) => v === n).length, 1, n);
    assert.equal(
      JSON.stringify(cfg),
      before,
      "operator policy must not be rewritten",
    );
    console.log(`PASS ${c.label}`);
  }
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
