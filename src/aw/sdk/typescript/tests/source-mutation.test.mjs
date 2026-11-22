import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import path from "node:path";
import test from "node:test";

import {
  LanguageSource,
  SourceDenied,
  SourceResultWithheld,
  confirmSourceRoutes,
  sourceCapabilities,
} from "../src/sources.mjs";
import { BINARY, startAwService } from "./source-mutation-fixture.mjs";

test("opt-in sourceCapabilities and lifecycle compatibility", () => {
  const base = sourceCapabilities(["tool.before", "tool.after"]);
  assert.equal(base.failure_actions, undefined);
  assert.equal(
    confirmSourceRoutes(base, {
      hooks: [{ event: "tool.before", replace_input: true }],
    }),
    false,
  );
  assert.equal(
    confirmSourceRoutes(base, {
      hooks: [{ event: "tool.after", replace_result: true }],
    }),
    false,
  );
  assert.equal(
    confirmSourceRoutes(base, {
      hooks: [{ event: "tool.after", on_error: "withhold_result" }],
    }),
    false,
  );

  const full = sourceCapabilities(
    [
      "session.start",
      "turn.stop",
      "session.end",
      "input.submit",
      "tool.before",
      "tool.after",
    ],
    {
      replaceInput: true,
      replaceResult: true,
      withholdResult: true,
    },
  );
  assert.deepEqual(full.events["session.start"], ["observe"]);
  assert.deepEqual(full.events["turn.stop"], ["observe", "block"]);
  assert.deepEqual(full.events["session.end"], ["observe"]);
  assert.ok(full.events["tool.before"].includes("replace_input"));
  assert.ok(full.events["tool.after"].includes("replace_result"));
  assert.deepEqual(full.failure_actions, { "tool.after": ["withhold_result"] });
  assert.equal(
    confirmSourceRoutes(full, {
      hooks: [
        { event: "session.start" },
        { event: "turn.stop" },
        { event: "tool.before", replace_input: true },
        {
          event: "tool.after",
          replace_result: true,
          on_error: "withhold_result",
        },
      ],
    }),
    true,
  );
  assert.throws(
    () => sourceCapabilities(["tool.after"], { replaceInput: true }),
    { code: "semantic_event_unavailable" },
  );
  assert.throws(
    () => sourceCapabilities(["tool.before"], { withholdResult: true }),
    { code: "semantic_event_unavailable" },
  );
});

test(
  "Layer 1 rewrite, withhold and streaming boundaries against real AW service",
  { skip: !BINARY, timeout: 25000 },
  async () => {
    const root = mkdtempSync(path.join("/tmp", "awj1-"));
    const service = await startAwService(root);
    try {
      const source = LanguageSource.create({
        endpoint: service.endpoint,
        target: "app",
        availability: "required",
        replaceInput: true,
        replaceResult: true,
        withholdResult: true,
      });
      try {
        await source.connect();
        await source.withSession({ sessionId: "js-layer1" }, async (session) => {
          const executed = [];
          const wrapped = session.wrapTool(
            async (args) => {
              executed.push(args.command);
              return `out:${args.command}`;
            },
            { name: "shell" },
          );

          assert.equal(await wrapped({ command: "raw:hello" }), "out:safe:hello");
          assert.deepEqual(executed, ["safe:hello"]);

          await assert.rejects(
            wrapped({ command: "rewrite-to-deny" }),
            (err) => err instanceof SourceDenied && err.event === "tool.before",
          );
          assert.deepEqual(executed, ["safe:hello"]);

          assert.equal(await wrapped({ command: "SECRET" }), "out:[REDACTED]");

          const wrappedStruct = session.wrapTool(
            async (args) => ({ stdout: `data:${args.command}`, exit_code: 0 }),
            { name: "struct" },
          );
          assert.deepEqual(await wrappedStruct({ command: "SECRET" }), {
            stdout: "data:[REDACTED]",
            exit_code: 0,
          });

          await assert.rejects(
            wrapped({ command: "FAULT" }),
            (err) => err instanceof SourceResultWithheld && err.toolExecuted === true,
          );

          class Req {
            constructor(command) {
              this.command = command;
            }
          }
          const noApply = session.wrapTool(async (r) => r.command, {
            name: "obj",
            extractInput: (r) => ({ command: r.command }),
            snapshotArg: (r) => new Req(r.command),
          });
          await assert.rejects(noApply(new Req("raw:bad")), {
            code: "source_input_replace_unsupported",
          });
          const withApply = session.wrapTool(async (r) => r.command, {
            name: "obj",
            extractInput: (r) => ({ command: r.command }),
            applyInput: (u) => [new Req(u.command)],
            snapshotArg: (r) => new Req(r.command),
          });
          assert.equal(await withApply(new Req("raw:ok")), "safe:ok");

          let streamRuns = 0;
          async function* streamTool(args) {
            streamRuns++;
            yield `chunk:${args.command}`;
          }
          const unbuffered = session.wrapToolStream(streamTool, { name: "stream" });
          await assert.rejects(
            async () => {
              for await (const _ of unbuffered({ command: "ok" })) {
                /* empty */
              }
            },
            { code: "source_stream_withhold_requires_buffering" },
          );
          assert.equal(streamRuns, 0);

          const buffered = session.wrapToolStream(streamTool, {
            name: "stream",
            bufferStream: true,
            extractResult: (chunks) => chunks.join("\n"),
            applyResult: (_orig, rep) => rep.split("\n"),
          });
          const replaced = [];
          for await (const c of buffered({ command: "SECRET" })) replaced.push(c);
          assert.deepEqual(replaced, ["chunk:[REDACTED]"]);

          const leaked = [];
          await assert.rejects(
            async () => {
              for await (const c of buffered({ command: "FAULT" })) leaked.push(c);
            },
            (err) => err instanceof SourceResultWithheld,
          );
          assert.deepEqual(leaked, []);
        });
      } finally {
        await source.close();
      }
    } finally {
      await service.stop();
      rmSync(root, { recursive: true, force: true });
    }
  },
);
