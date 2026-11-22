import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import path from "node:path";
import test from "node:test";

import { LanguageSource } from "../src/sources.mjs";
import {
  BINARY,
  readProviderLog,
  startAwService,
} from "./source-mutation-fixture.mjs";

test(
  "Strict isolation, strict array shapes, lazy iterators and unified extractResult byte budgets",
  { skip: !BINARY, timeout: 25000 },
  async () => {
    const root = mkdtempSync(path.join("/tmp", "awj2-"));
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
      await source.connect();
      try {
        await source.withSession({ sessionId: "js-hard" }, async (session) => {
          // 1) Async generator reusing one mutable object + mutating in finally.
          const shared = { v: -1 };
          async function* reusingStream() {
            try {
              for (let i = 0; i < 3; i++) {
                shared.v = i;
                yield shared;
              }
            } finally {
              shared.v = 999;
            }
          }
          const wrappedStream = session.wrapToolStream(reusingStream, {
            name: "reusing",
            bufferStream: true,
          });
          const seen = [];
          for await (const item of wrappedStream({ command: "ok" })) seen.push(item);
          assert.deepEqual(seen, [{ v: 0 }, { v: 1 }, { v: 2 }]);

          // 2) Normal tool returning a mutable object mutated afterward.
          let ref;
          const wrappedMut = session.wrapTool(
            async () => {
              ref = { text: "clean", nested: [1, 2] };
              return ref;
            },
            { name: "mut" },
          );
          const delivered = await wrappedMut({ command: "ok" });
          ref.text = "TAMPERED";
          ref.nested.push(99);
          assert.deepEqual(delivered, { text: "clean", nested: [1, 2] });

          // 3) P0: Strict array shape isolation (named/non-enumerable/symbol props & sparse arrays)
          //    on both normal results and async/sync streams, plus input-side parity.
          const makeNamedPropArr = () => {
            const arr = [1];
            arr.hidden = "UNAUDITED";
            return arr;
          };
          const makeNonEnumPropArr = () => {
            const arr = [1];
            Object.defineProperty(arr, "hidden", { value: "UNAUDITED", enumerable: false });
            return arr;
          };
          const makeSymbolPropArr = () => {
            const arr = [1];
            arr[Symbol("secret")] = "UNAUDITED";
            return arr;
          };
          const makeSparseArr = () => {
            // eslint-disable-next-line no-sparse-arrays
            return [1, , 3];
          };
          const makeGetterArr = () => {
            const arr = [];
            Object.defineProperty(arr, 0, { get: () => "x", enumerable: true });
            return arr;
          };

          const echoTool = session.wrapTool(async (arr) => arr, { name: "arr_in" });
          await assert.rejects(() => echoTool(makeNamedPropArr()), {
            code: "source_input_snapshot_required",
          });
          await assert.rejects(() => echoTool(makeSparseArr()), {
            code: "source_input_snapshot_required",
          });

          for (const [label, makeBad] of [
            ["named_prop", makeNamedPropArr],
            ["nested_named_prop", () => ({ items: makeNamedPropArr() })],
            ["non_enum_prop", makeNonEnumPropArr],
            ["symbol_prop", makeSymbolPropArr],
            ["sparse", makeSparseArr],
            ["nested_sparse", () => ({ items: makeSparseArr() })],
            ["getter", makeGetterArr],
          ]) {
            const badNormal = session.wrapTool(async () => makeBad(), {
              name: `n_${label}`,
            });
            await assert.rejects(
              () => badNormal({ command: "ok" }),
              (err) => err?.code === "source_result_isolation_required" && err.toolExecuted === true,
              `normal:${label}`,
            );

            const badAsyncStream = session.wrapToolStream(
              async function* () {
                yield makeBad();
              },
              { name: `as_${label}`, bufferStream: true },
            );
            const leaked = [];
            await assert.rejects(
              async () => {
                for await (const c of badAsyncStream({ command: "ok" })) leaked.push(c);
              },
              (err) => err?.code === "source_result_isolation_required" && err.toolExecuted === true,
              `asyncStream:${label}`,
            );
            assert.deepEqual(leaked, []);

            const badSyncStream = session.wrapTool(
              function* () {
                yield makeBad();
              },
              { name: `ss_${label}`, bufferStream: true },
            );
            await assert.rejects(
              () => badSyncStream({ command: "ok" }),
              (err) => err?.code === "source_result_isolation_required" && err.toolExecuted === true,
              `syncStream:${label}`,
            );
          }

          // 4) Dynamic lazy iterator / sync generator: rejected unbuffered, buffered into isolated copies.
          let syncStarted = 0;
          function* syncGen() {
            syncStarted += 1;
            yield "part";
          }
          await assert.rejects(
            () => session.wrapTool(syncGen, { name: "sgen" })({ command: "ok" }),
            { code: "source_stream_withhold_requires_buffering" },
          );
          assert.equal(syncStarted, 0);

          let iterClosed = 0;
          const createdIters = [];
          function returnsLazy() {
            const g = (function* () {
              const box = { n: 0 };
              try {
                for (const i of [1, 2]) {
                  box.n = i;
                  yield box;
                }
              } finally {
                box.n = 999;
                iterClosed += 1;
              }
            })();
            createdIters.push(g);
            return g;
          }
          await assert.rejects(
            () => session.wrapTool(returnsLazy, { name: "lazy" })({ command: "ok" }),
            (err) => err?.code === "source_stream_withhold_requires_buffering" && err.toolExecuted === true,
          );
          assert.equal(createdIters.at(-1).next().done, true);

          const bufferedIter = await session.wrapTool(returnsLazy, {
            name: "lazy_buf",
            bufferStream: true,
          })({ command: "ok" });
          assert.deepEqual([...bufferedIter], [{ n: 1 }, { n: 2 }]);
          assert.equal(iterClosed, 1);

          // 5) Exact JSON array byte boundary, single-chunk & multi-chunk overflow.
          let closedCount = 0;
          async function* sizedStream(args) {
            try {
              for (const c of args.chunks) yield c;
            } finally {
              closedCount += 1;
            }
          }
          // JSON '["abcd","efgh"]' is 15 bytes (exact boundary).
          const bounded = session.wrapToolStream(sizedStream, {
            name: "sized",
            bufferStream: true,
            maxStreamBytes: 15,
          });
          const exact = [];
          for await (const c of bounded({ chunks: ["abcd", "efgh"] })) exact.push(c);
          assert.deepEqual(exact, ["abcd", "efgh"]);

          const recvSingle = [];
          const c0 = closedCount;
          await assert.rejects(
            async () => {
              for await (const c of bounded({ chunks: ["123456789012"] })) recvSingle.push(c);
            },
            (err) => err?.code === "source_stream_result_too_large" && err.toolExecuted === true,
          );
          assert.deepEqual(recvSingle, []);
          assert.equal(closedCount, c0 + 1);

          const recvMulti = [];
          const c1 = closedCount;
          await assert.rejects(
            async () => {
              for await (const c of bounded({ chunks: ["abcd", "efgh", "x"] })) recvMulti.push(c);
            },
            (err) => err?.code === "source_stream_result_too_large" && err.toolExecuted === true,
          );
          assert.deepEqual(recvMulti, []);
          assert.equal(closedCount, c1 + 1);

          // 6) P1: Small plain JSON object extracted into oversized audited payload
          //    enforces maxStreamBytes, evaluates extractResult once, yields zero chunks,
          //    and never sends the oversized payload to AW tool.after.
          let extractCalls = 0;
          const expandPayload = () => {
            extractCalls += 1;
            return "x".repeat(10000);
          };
          const afterBefore = readProviderLog(service.logPath).filter(
            (e) => e.event === "tool.after",
          ).length;

          const expandedAsync = session.wrapToolStream(sizedStream, {
            name: "expand_async",
            bufferStream: true,
            maxStreamBytes: 64,
            extractResult: expandPayload,
          });
          const leakedAsync = [];
          await assert.rejects(
            async () => {
              for await (const c of expandedAsync({ chunks: [{ k: 1 }] })) leakedAsync.push(c);
            },
            (err) => err?.code === "source_stream_result_too_large" && err.toolExecuted === true,
          );
          assert.deepEqual(leakedAsync, []);
          assert.equal(extractCalls, 1);

          extractCalls = 0;
          const expandedSync = session.wrapTool(
            function* () {
              yield { k: 1 };
            },
            {
              name: "expand_sync",
              bufferStream: true,
              maxStreamBytes: 64,
              extractResult: expandPayload,
            },
          );
          await assert.rejects(
            () => expandedSync({ command: "ok" }),
            (err) => err?.code === "source_stream_result_too_large" && err.toolExecuted === true,
          );
          assert.equal(extractCalls, 1);

          const afterCount = readProviderLog(service.logPath).filter(
            (e) => e.event === "tool.after",
          ).length;
          assert.equal(afterCount, afterBefore);

          // 7) P1: extractResult mutating its argument cannot tamper with delivered chunks.
          extractCalls = 0;
          const mutExtract = session.wrapToolStream(sizedStream, {
            name: "mut_ext",
            bufferStream: true,
            maxStreamBytes: 128,
            extractResult: (chunks) => {
              extractCalls += 1;
              const text = chunks[0].v;
              chunks[0].v = "MUTATED_BY_EXTRACTOR";
              return text;
            },
          });
          const cleanOut = [];
          for await (const c of mutExtract({ chunks: [{ v: "clean" }] })) cleanOut.push(c);
          assert.deepEqual(cleanOut, [{ v: "clean" }]);
          assert.equal(extractCalls, 1);

          // 8) P1: Unmeterably opaque custom types (Map, Date, class instance) rejected
          //    even when extractResult (or opaque snapshotResult) is supplied.
          class BigBox {
            constructor(data) {
              this.data = data;
            }
          }
          for (const [label, makeOpaque] of [
            ["map", () => new Map([["k", "x".repeat(10000)]])],
            ["date", () => new Date()],
            ["class", () => new BigBox("x".repeat(10000))],
          ]) {
            const opNormal = session.wrapTool(async () => makeOpaque(), {
              name: `op_${label}`,
              extractResult: () => "short",
            });
            await assert.rejects(
              () => opNormal({ command: "ok" }),
              (err) => err?.code === "source_result_isolation_required" && err.toolExecuted === true,
              `opaque_normal:${label}`,
            );

            const opStream = session.wrapToolStream(
              async function* () {
                yield makeOpaque();
              },
              {
                name: `ops_${label}`,
                bufferStream: true,
                maxStreamBytes: 64,
                extractResult: () => "short",
              },
            );
            await assert.rejects(
              async () => {
                for await (const _ of opStream({ command: "ok" })) {
                  /* empty */
                }
              },
              (err) => err?.code === "source_result_isolation_required" && err.toolExecuted === true,
              `opaque_stream:${label}`,
            );
          }

          const opSnapStream = session.wrapToolStream(
            async function* () {
              yield new BigBox("x".repeat(10000));
            },
            {
              name: "ops_snap",
              bufferStream: true,
              maxStreamBytes: 64,
              snapshotResult: (b) => new BigBox(b.data),
              extractResult: (bs) => bs.map((b) => b.data).join(""),
            },
          );
          await assert.rejects(
            async () => {
              for await (const _ of opSnapStream({ command: "ok" })) {
                /* empty */
              }
            },
            (err) => err?.code === "source_result_isolation_required" && err.toolExecuted === true,
          );

          // 9) P2: Sync buffered replay iterator honors caller cancellation between next() calls.
          const abortController = new AbortController();
          const cancelReason = new Error("caller_cancelled_sync_replay");
          const cancellableSync = session.wrapTool(
            function* () {
              yield { part: 1 };
              yield { part: 2 };
            },
            {
              name: "sync_cancel",
              bufferStream: true,
              extractInput: (req) => req,
              extractSignal: (_req, sig) => sig,
            },
          );
          const replay = await cancellableSync(
            { command: "ok" },
            abortController.signal,
          );
          assert.deepEqual(replay.next(), { value: { part: 1 }, done: false });
          abortController.abort(cancelReason);
          assert.throws(
            () => replay.next(),
            (err) => err === cancelReason && err.toolExecuted === true,
          );
          assert.deepEqual(replay.next(), { value: undefined, done: true });
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
