#!/usr/bin/env node

// OpenCode 2 entrypoint coverage for the Tokenless plugin.
//
// The host is faked at its documented V2 plugin boundary: a context whose
// tool/session domains record hook registrations, and events shaped exactly as
// `@opencode/plugin` 2.x declares them (`event.id`, `event.input`,
// `event.result.content` as a string or a content-part list, and a
// `name -> { description, input }` tool record). Shared hook scripts are faked
// by one runner that logs every payload, so the assertions cover both the event
// mapping and what Tokenless actually sends to Core.

import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import definition, { TokenlessPlugin } from "../adapters/tokenless/opencode/plugin.js";
// The plugin is loaded straight from `plugins/tokenless.js`, so the module's
// export shape is part of the supported-version contract: the namespace import
// is what every host loader below is fed.
import * as pluginModule from "../adapters/tokenless/opencode/plugin.js";

const sandbox = mkdtempSync(join(tmpdir(), "tokenless-opencode-v2-"));
const runner = join(sandbox, "run-hook.sh");
const log = join(sandbox, "hooks.log");

writeFileSync(
  runner,
  `#!/usr/bin/env bash
set -euo pipefail
hook="\${1:?hook name required}"
payload="$(cat)"
printf '%s\\t%s\\n' "$hook" "$payload" >> "$TOKENLESS_TEST_LOG"
case "$hook" in
  tool_ready_hook.sh)
    case "\${TOKENLESS_TEST_READY:-}" in
      block) printf '%s\\n' '{"decision":"block","reason":"missing required dependency"}' ;;
      partial) printf '%s\\n' '{"hookSpecificOutput":{"additionalContext":"[tokenless:ready] partial"}}' ;;
      *) printf '%s\\n' '{}' ;;
    esac
    ;;
  rewrite_hook.py)
    printf '%s\\n' '{"hookSpecificOutput":{"updatedInput":{"command":"rtk git status"}}}'
    ;;
  compress_response_hook.py)
    if [[ "$payload" == *'"is_error":true'* ]]; then
      printf '%s\\n' '{"hookSpecificOutput":{"additionalContext":"[tokenless:env] command failed"}}'
    elif [[ -n "\${TOKENLESS_TEST_PASSTHROUGH:-}" && "$payload" == *"\${TOKENLESS_TEST_PASSTHROUGH}"* ]]; then
      # Core returned this slot unchanged: echo the model-visible text back.
      TOKENLESS_TEST_PAYLOAD="$payload" node -e '
        const payload = JSON.parse(process.env.TOKENLESS_TEST_PAYLOAD);
        process.stdout.write(JSON.stringify({
          hookSpecificOutput: { updatedToolOutput: String(payload.tool_response ?? "") },
        }));
      '
    else
      printf '%s\\n' '{"hookSpecificOutput":{"updatedToolOutput":"compressed-response","additionalContext":"[tokenless:env] warning"}}'
    fi
    ;;
  compress_schema_hook.py)
    TOKENLESS_TEST_PAYLOAD="$payload" TOKENLESS_TEST_RENAME="\${TOKENLESS_TEST_RENAME:-}" node -e '
      const request = JSON.parse(process.env.TOKENLESS_TEST_PAYLOAD);
      const renamed = process.env.TOKENLESS_TEST_RENAME === "1";
      const tools = request.llm_request.config.tools.map((tool) => ({
        name: renamed ? "host-renamed" : tool.name,
        description: "compressed description",
        parameters: { type: "object", properties: { command: { type: "string" } } },
      }));
      process.stdout.write(JSON.stringify({ hookSpecificOutput: { llm_request: { config: { tools } } } }));
    '
    ;;
  *) printf '%s\\n' '{}' ;;
esac
`,
);

process.env.TOKENLESS_HOOK_RUNNER = runner;
process.env.TOKENLESS_TEST_LOG = log;

function createHost() {
  const registered = [];
  const domains = {
    tool: new Map(),
    session: new Map(),
  };
  const domain = (name) => ({
    hook: async (hookName, callback) => {
      registered.push(`${name}.${hookName}`);
      domains[name].set(hookName, callback);
      return { dispose: async () => domains[name].delete(hookName) };
    },
  });
  return {
    ctx: { tool: domain("tool"), session: domain("session") },
    registered,
    domains,
    fire: (name, hookName, event) => domains[name].get(hookName)(event),
  };
}

function records() {
  const raw = readFileSync(log, "utf8").trim();
  if (!raw) return [];
  return raw.split("\n").map((line) => {
    const separator = line.indexOf("\t");
    return { hook: line.slice(0, separator), payload: JSON.parse(line.slice(separator + 1)) };
  });
}

function recordsFor(hook) {
  return records().filter((record) => record.hook === hook);
}

try {
  // -- The dual entrypoint contract both host generations load ----------------
  assert.equal(definition.id, "tokenless");
  assert.equal(typeof definition.setup, "function");
  assert.equal(typeof definition.server, "function");
  assert.equal(definition.server, TokenlessPlugin, "server() must be the V1 implementation");

  const host = createHost();
  await definition.setup(host.ctx);
  assert.deepEqual(host.registered.sort(), [
    "session.context",
    "tool.execute.after",
    "tool.execute.before",
  ]);

  // -- execute.before: command rewriting across the shell tool family ---------
  for (const [index, tool] of ["shell", "bash", "Shell", "run_in_terminal"].entries()) {
    const event = {
      tool,
      sessionID: "ses_v2",
      agent: "build",
      messageID: "msg_v2",
      id: `call_rewrite_${index}`,
      input: { command: "git status", workdir: "/tmp" },
    };
    await host.fire("tool", "execute.before", event);
    assert.equal(event.input.command, "rtk git status", `${tool} should be rewritten`);
    assert.equal(event.input.workdir, "/tmp", "unrelated arguments survive");
    const rewrite = recordsFor("rewrite_hook.py").find(
      (record) => record.payload.tool_call_id === `call_rewrite_${index}`,
    );
    assert.ok(rewrite, `${tool} should reach the rewrite hook`);
    assert.equal(rewrite.payload.tool_name, tool);
    assert.equal(rewrite.payload.session_id, "ses_v2");
  }

  // A non-shell tool keeps its arguments and never reaches the rewrite hook.
  const readEvent = {
    tool: "read",
    sessionID: "ses_v2",
    id: "call_read",
    input: { filePath: "/tmp/x" },
  };
  await host.fire("tool", "execute.before", readEvent);
  assert.deepEqual(readEvent.input, { filePath: "/tmp/x" });
  assert.ok(
    !recordsFor("rewrite_hook.py").some((record) => record.payload.tool_call_id === "call_read"),
    "read is not a shell tool",
  );

  // A shell call without a command string is left alone.
  const noCommandEvent = { tool: "shell", sessionID: "ses_v2", id: "call_no_command", input: {} };
  await host.fire("tool", "execute.before", noCommandEvent);
  assert.deepEqual(noCommandEvent.input, {});

  // A frozen argument object still receives the rewrite through replacement.
  const frozenEvent = {
    tool: "shell",
    sessionID: "ses_v2",
    id: "call_frozen",
    input: Object.freeze({ command: "git status", workdir: "/tmp" }),
  };
  await host.fire("tool", "execute.before", frozenEvent);
  assert.deepEqual(frozenEvent.input, { command: "rtk git status", workdir: "/tmp" });

  // The readiness gate still blocks, as it does on OpenCode 1.
  process.env.TOKENLESS_TEST_READY = "block";
  await assert.rejects(
    host.fire("tool", "execute.before", {
      tool: "shell",
      sessionID: "ses_v2",
      id: "call_blocked",
      input: { command: "cargo test" },
    }),
    /missing required dependency/,
  );
  process.env.TOKENLESS_TEST_READY = "";

  // -- execute.after: string and content-part results -------------------------
  const stringEvent = {
    tool: "read",
    sessionID: "ses_v2",
    id: "call_string",
    input: {},
    status: "completed",
    result: { content: "original-response", metadata: {} },
  };
  await host.fire("tool", "execute.after", stringEvent);
  assert.equal(stringEvent.result.content, "[tokenless:env] warning\ncompressed-response");

  // Content parts are ordered model input. An interleaved result keeps every
  // part where the host put it: the two texts are separate slots, so the second
  // one is never moved ahead of the file part.
  const filePart = { type: "file", uri: "file:///tmp/report.csv", mime: "text/csv" };
  const compressed = "[tokenless:env] warning\ncompressed-response";
  const partsEvent = {
    tool: "shell",
    sessionID: "ses_v2",
    id: "call_parts",
    input: { command: "cat report.csv" },
    status: "completed",
    result: {
      content: [
        { type: "text", text: "first text part" },
        filePart,
        { type: "text", text: "second text part" },
      ],
      metadata: { exit: 0 },
    },
  };
  const partsBefore = recordsFor("compress_response_hook.py").length;
  await host.fire("tool", "execute.after", partsEvent);
  assert.deepEqual(partsEvent.result.content, [
    { type: "text", text: compressed },
    filePart,
    { type: "text", text: compressed },
  ]);
  assert.equal(
    partsEvent.result.content[1],
    filePart,
    "the file part keeps both its position and its identity",
  );
  const partsCalls = recordsFor("compress_response_hook.py").slice(partsBefore);
  assert.deepEqual(
    partsCalls.map((record) => record.payload.tool_response),
    ["first text part", "second text part"],
    "each text run is compressed in its own slot, in host order",
  );
  assert.ok(
    partsCalls.every((record) => record.payload.is_error !== true),
    "exit 0 is not a failure",
  );

  // Adjacent text parts are one run: still a single hook call, collapsed into
  // the run's first position, which cannot reorder anything.
  const runEvent = {
    tool: "read",
    sessionID: "ses_v2",
    id: "call_run",
    input: {},
    status: "completed",
    result: {
      content: [
        { type: "text", text: "run part one" },
        { type: "text", text: "run part two" },
        filePart,
      ],
      metadata: {},
    },
  };
  const runBefore = recordsFor("compress_response_hook.py").length;
  await host.fire("tool", "execute.after", runEvent);
  assert.deepEqual(runEvent.result.content, [{ type: "text", text: compressed }, filePart]);
  const runCalls = recordsFor("compress_response_hook.py").slice(runBefore);
  assert.equal(runCalls.length, 1, "one adjacent text run costs one hook call");
  assert.equal(runCalls[0].payload.tool_response, "run part one\nrun part two");

  // A run Core returned unchanged keeps every one of its original parts, even
  // when another run in the same result did change: only the changed run is
  // rebuilt, so the untouched run's shape and part identity both survive.
  const headPart = { type: "text", text: "unchanged run head" };
  const tailPart = { type: "text", text: "unchanged run tail" };
  const changedPart = { type: "text", text: "compressible tail run" };
  const mixedEvent = {
    tool: "read",
    sessionID: "ses_v2",
    id: "call_mixed_runs",
    input: {},
    status: "completed",
    result: { content: [headPart, tailPart, filePart, changedPart], metadata: {} },
  };
  const mixedBefore = recordsFor("compress_response_hook.py").length;
  process.env.TOKENLESS_TEST_PASSTHROUGH = "unchanged run head";
  await host.fire("tool", "execute.after", mixedEvent);
  delete process.env.TOKENLESS_TEST_PASSTHROUGH;

  assert.deepEqual(mixedEvent.result.content, [
    headPart,
    tailPart,
    filePart,
    { type: "text", text: compressed },
  ]);
  assert.equal(mixedEvent.result.content[0], headPart, "an unchanged run keeps its head part");
  assert.equal(mixedEvent.result.content[1], tailPart, "an unchanged run keeps its tail part");
  assert.equal(mixedEvent.result.content[2], filePart, "the file part still sits between them");
  assert.deepEqual(
    recordsFor("compress_response_hook.py")
      .slice(mixedBefore)
      .map((record) => record.payload.tool_response),
    ["unchanged run head\nunchanged run tail", "compressible tail run"],
    "each run is still sent to Core separately, in host order",
  );

  // When Core changes nothing at all, the host's own array is left in place.
  const untouchedEvent = {
    tool: "read",
    sessionID: "ses_v2",
    id: "call_untouched",
    input: {},
    status: "completed",
    result: { content: [headPart, filePart], metadata: {} },
  };
  const untouchedContent = untouchedEvent.result.content;
  process.env.TOKENLESS_TEST_PASSTHROUGH = "unchanged run head";
  await host.fire("tool", "execute.after", untouchedEvent);
  delete process.env.TOKENLESS_TEST_PASSTHROUGH;
  assert.equal(
    untouchedEvent.result.content,
    untouchedContent,
    "an unchanged result keeps the host's own content array",
  );

  // A result with no text part has nothing model-visible to replace.
  const fileOnlyEvent = {
    tool: "read",
    sessionID: "ses_v2",
    id: "call_file_only",
    input: {},
    status: "completed",
    result: { content: [filePart], metadata: {} },
  };
  const fileOnlyBefore = recordsFor("compress_response_hook.py").length;
  await host.fire("tool", "execute.after", fileOnlyEvent);
  assert.deepEqual(fileOnlyEvent.result.content, [filePart]);
  assert.equal(
    recordsFor("compress_response_hook.py").length,
    fileOnlyBefore,
    "a file-only result never spawns a hook",
  );

  // A failed shell call forwards the exit-code evidence Core cannot see.
  for (const [index, [tool, exit, expected]] of [
    ["shell", 1, true],
    ["shell", 127, true],
    ["shell", 0, false],
    ["shell", "1", false],
    ["read", 1, false],
  ].entries()) {
    const event = {
      tool,
      sessionID: "ses_v2",
      id: `call_exit_${index}`,
      input: { command: "missing-command" },
      status: "completed",
      result: { content: "command not found: important failure details", metadata: { exit } },
    };
    await host.fire("tool", "execute.after", event);
    const record = recordsFor("compress_response_hook.py").find(
      (entry) => entry.payload.tool_call_id === event.id,
    );
    assert.equal(record.payload.is_error === true, expected, `${tool} exit=${String(exit)}`);
    if (expected) {
      // The failure branch returns diagnostics only, so the host keeps its text.
      assert.equal(
        event.result.content,
        "[tokenless:env] command failed\ncommand not found: important failure details",
      );
    }
  }

  // A failed tool call carries no result, and a structured-only result has no
  // model-visible text yet: both are left for the host.
  const errorEvent = {
    tool: "shell",
    sessionID: "ses_v2",
    id: "call_error",
    input: { command: "false" },
    status: "error",
    error: { message: "boom" },
  };
  await host.fire("tool", "execute.after", errorEvent);
  assert.ok(
    !recordsFor("compress_response_hook.py").some(
      (record) => record.payload.tool_call_id === "call_error",
    ),
  );

  const outputOnlyEvent = {
    tool: "mcp__reports__get_report",
    sessionID: "ses_v2",
    id: "call_output_only",
    input: {},
    status: "completed",
    result: { output: { rows: 10 }, metadata: {} },
  };
  await host.fire("tool", "execute.after", outputOnlyEvent);
  assert.deepEqual(outputOnlyEvent.result, { output: { rows: 10 }, metadata: {} });
  assert.ok(
    !recordsFor("compress_response_hook.py").some(
      (record) => record.payload.tool_call_id === "call_output_only",
    ),
  );

  // -- session context: batched schema compression, edited in place -----------
  const hostShellSchema = { type: "object", title: "Shell", properties: {} };
  const hostReadSchema = { type: "object", properties: { filePath: { type: "string" } } };
  const shellEntry = { description: "A very long shell description", input: hostShellSchema };
  const readEntry = { description: "A very long read description", input: hostReadSchema };
  class RuntimeCodec {}
  const codecEntry = { description: "Runtime codec tool", input: new RuntimeCodec() };

  const contextEvent = {
    sessionID: "ses_v2",
    agent: "build",
    model: { providerID: "anthropic", modelID: "sonnet" },
    system: [],
    messages: [],
    options: {},
    tools: { shell: shellEntry, read: readEntry, legacy: codecEntry },
  };
  const schemaCallsBefore = recordsFor("compress_schema_hook.py").length;
  await host.fire("session", "context", contextEvent);

  const schemaCalls = recordsFor("compress_schema_hook.py").slice(schemaCallsBefore);
  assert.equal(schemaCalls.length, 1, "one batched schema call per model request");
  const declared = schemaCalls[0].payload.llm_request.config.tools;
  assert.deepEqual(
    declared.map((tool) => tool.name),
    ["shell", "read"],
    "a runtime codec never crosses the hook boundary",
  );

  // Core matches hooked entries by identity, so the entry objects survive.
  assert.equal(contextEvent.tools.shell, shellEntry);
  assert.equal(contextEvent.tools.read, readEntry);
  assert.equal(shellEntry.description, "compressed description");
  assert.deepEqual(shellEntry.input, { type: "object", properties: { command: { type: "string" } } });
  assert.deepEqual(readEntry.input, { type: "object", properties: { command: { type: "string" } } });
  assert.equal(codecEntry.description, "Runtime codec tool");
  assert.ok(codecEntry.input instanceof RuntimeCodec);
  // The host's own schema objects are untouched, and each entry gets its own copy.
  assert.deepEqual(hostShellSchema, { type: "object", title: "Shell", properties: {} });
  assert.deepEqual(hostReadSchema, {
    type: "object",
    properties: { filePath: { type: "string" } },
  });
  assert.notStrictEqual(shellEntry.input, readEntry.input);

  // An unchanged declaration set is served from cache: no second subprocess.
  const replayEvent = {
    ...contextEvent,
    tools: {
      shell: { description: "A very long shell description", input: hostShellSchema },
      read: { description: "A very long read description", input: hostReadSchema },
    },
  };
  await host.fire("session", "context", replayEvent);
  assert.equal(
    recordsFor("compress_schema_hook.py").length,
    schemaCallsBefore + 1,
    "identical declarations must not spawn another hook call",
  );
  assert.equal(replayEvent.tools.shell.description, "compressed description");
  replayEvent.tools.shell.input.properties.command.type = "number";
  assert.deepEqual(shellEntry.input.properties.command, { type: "string" }, "cache is not aliased");

  // Declarations Core returns under another name keep the host's definition.
  process.env.TOKENLESS_TEST_RENAME = "1";
  const renamedEntry = {
    description: "An uncompressed description",
    input: { type: "object", properties: { fresh: { type: "string" } } },
  };
  const renamedEvent = { ...contextEvent, tools: { fresh: renamedEntry } };
  await host.fire("session", "context", renamedEvent);
  assert.equal(renamedEntry.description, "An uncompressed description");
  assert.deepEqual(renamedEntry.input, {
    type: "object",
    properties: { fresh: { type: "string" } },
  });
  delete process.env.TOKENLESS_TEST_RENAME;

  // -- Host loader contracts: which module shapes each generation accepts -----
  //
  // Each loader mirrors the upstream implementation for the release it names,
  // and is fed the module namespace a host gets from `await import(...)`.

  const isRecord = (value) => !!value && typeof value === "object" && !Array.isArray(value);

  // OpenCode 1.3.4-1.19.x: `readV1Plugin(mod, spec, "server", "detect")` first,
  // then `getLegacyPlugins(mod)` for a module with no usable default export.
  function readV1Plugin(mod) {
    const value = mod.default;
    if (!isRecord(value)) return undefined;
    if (!("id" in value) && !("server" in value) && !("tui" in value)) return undefined;
    const server = "server" in value ? value.server : undefined;
    if (server !== undefined && typeof server !== "function") {
      throw new TypeError("plugin has invalid server export");
    }
    if (server === undefined && !("tui" in value)) {
      throw new TypeError("plugin must default export an object with server()");
    }
    return value;
  }

  function loadOpenCode1(mod) {
    const detected = readV1Plugin(mod);
    if (detected) return [detected.server];
    const seen = new Set();
    const servers = [];
    for (const entry of Object.values(mod)) {
      if (seen.has(entry)) continue;
      seen.add(entry);
      const server =
        typeof entry === "function"
          ? entry
          : isRecord(entry) && typeof entry.server === "function"
            ? entry.server
            : undefined;
      if (!server) throw new TypeError("Plugin export is not a function");
      servers.push(server);
    }
    return servers;
  }

  // OpenCode 1.0.0-1.3.3: every export is called as a plugin function.
  function loadOpenCode1Naive(mod) {
    const seen = new Set();
    const exports = [];
    for (const [, fn] of Object.entries(mod)) {
      if (seen.has(fn)) continue;
      seen.add(fn);
      exports.push(fn);
    }
    return exports;
  }

  // OpenCode 2: `default` must decode as `{ id, setup }` or `{ id, effect }`.
  function readV2Definition(mod) {
    const value = mod.default;
    if (!isRecord(value) || typeof value.id !== "string") return undefined;
    if (typeof value.setup !== "function" && typeof value.effect !== "function") return undefined;
    return value;
  }

  assert.ok(readV2Definition(pluginModule), "OpenCode 2 must decode the default definition");

  const v1Servers = loadOpenCode1(pluginModule);
  assert.deepEqual(v1Servers, [TokenlessPlugin], "OpenCode 1.3.4+ loads exactly one server plugin");
  const loadedHooks = await v1Servers[0]({});
  assert.deepEqual(
    Object.keys(loadedHooks).sort(),
    ["tool.definition", "tool.execute.after", "tool.execute.before"],
    "the loader path registers the V1 hooks, not just the named export",
  );

  // The pre-1.3.4 loader calls every export, including the object default export
  // OpenCode 2 requires, so it cannot initialize this module. That is why the
  // manifest floor is 1.3.4. The invariant below ties the two together: the
  // advertised range may only include those releases once every export is
  // callable again, so the range cannot be widened without fixing the shape.
  const naiveExports = loadOpenCode1Naive(pluginModule);
  await assert.rejects(
    (async () => {
      for (const fn of naiveExports) await fn({});
    })(),
    TypeError,
    "the pre-1.3.4 loader must still fail on the object default export",
  );
  const manifest = JSON.parse(
    readFileSync(new URL("../adapters/tokenless/manifest.json.in", import.meta.url), "utf8").replace(
      /@VERSION@/g,
      "0.0.0",
    ),
  );
  const supported = manifest.targets.opencode.compatibleVersions;
  const floor = /^>=\s*(\d+)\.(\d+)\.(\d+)/.exec(supported);
  assert.ok(floor, `opencode compatibleVersions must declare a lower bound, got "${supported}"`);
  const [, major, minor, patch] = floor.map(Number);
  const reachesNaiveLoader =
    major < 1 || (major === 1 && (minor < 3 || (minor === 3 && patch < 4)));
  assert.ok(
    !reachesNaiveLoader || naiveExports.every((entry) => typeof entry === "function"),
    `opencode compatibleVersions "${supported}" advertises releases whose loader calls every export as a function`,
  );

  // `anolisa adapter enable` never runs the bundled installer, so the ANOLISA
  // component manifest has to carry the same floor or that path links a module
  // the host cannot initialize. All three declarations must agree, and the
  // component manifest's sub-table must sit inside the opencode adapter element
  // rather than the neighbouring one.
  const contract = readFileSync(new URL("../.anolisa/component.toml.in", import.meta.url), "utf8");
  const adapterBlocks = contract.split(/^\[\[adapters\]\][^\S\n]*$/m).slice(1);
  const opencodeBlock = adapterBlocks.find((block) =>
    /^framework\s*=\s*"opencode"[^\S\n]*$/m.test(block),
  );
  assert.ok(opencodeBlock, "component.toml.in must declare an opencode adapter");
  const declaredCompat = /\[adapters\.compat\][\s\S]*?framework_version\s*=\s*"([^"]*)"/.exec(
    opencodeBlock,
  );
  assert.ok(
    declaredCompat,
    "the opencode adapter must declare [adapters.compat].framework_version, or the managed"
      + " enable path gates on nothing while the bundled installer gates on " + supported,
  );
  assert.equal(
    declaredCompat[1],
    supported,
    `component.toml.in declares "${declaredCompat[1]}" but manifest.json.in declares "${supported}"`,
  );

  // The bundled scripts gate on their own constant; it must not drift either.
  for (const script of ["detect.sh", "install.sh"]) {
    const source = readFileSync(
      new URL(`../adapters/tokenless/opencode/scripts/${script}`, import.meta.url),
      "utf8",
    );
    const gate = /^MIN_OPENCODE_VERSION="([^"]*)"/m.exec(source);
    assert.ok(gate, `${script} must declare MIN_OPENCODE_VERSION`);
    assert.equal(
      `>=${gate[1]}`,
      supported,
      `${script} gates on ${gate[1]} but the manifests declare "${supported}"`,
    );
  }

  // -- The V1 entrypoint shares the shell-family fix --------------------------
  const v1Hooks = await definition.server();
  const v1Output = { args: { command: "git status" } };
  await v1Hooks["tool.execute.before"](
    { tool: "shell", sessionID: "ses_v1", callID: "call_v1_shell" },
    v1Output,
  );
  assert.equal(v1Output.args.command, "rtk git status", "OpenCode 2's shell name on the V1 path");

  const v1Result = { output: "command not found", metadata: { exit: 127 } };
  await v1Hooks["tool.execute.after"](
    { tool: "shell", sessionID: "ses_v1", callID: "call_v1_exit", args: { command: "nope" } },
    v1Result,
  );
  const v1Record = recordsFor("compress_response_hook.py").find(
    (record) => record.payload.tool_call_id === "call_v1_exit",
  );
  assert.equal(v1Record.payload.is_error, true);
  assert.equal(v1Result.output, "[tokenless:env] command failed\ncommand not found");

  console.log("OpenCode 2 plugin tests passed");
} finally {
  rmSync(sandbox, { recursive: true, force: true });
}
