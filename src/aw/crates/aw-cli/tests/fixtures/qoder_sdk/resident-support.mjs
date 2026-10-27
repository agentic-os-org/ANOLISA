/** Isolated native admission gates for resident ownership regressions. */
import fs from "node:fs";
import path from "node:path";
import { Resident } from "../../../../aw-adapter/native/qoder-sdk/resident.mjs";

export const deferred = () => {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
};

export function fixture(t, holdInitialization = false) {
  const root = fs.mkdtempSync(path.join(process.cwd(), "sdk-resident-"));
  const sdk = path.join(root, "sdk.mjs");
  const state = { imported: deferred(), importGate: deferred(), closeEntered: deferred(),
    closeGate: deferred(), created: 0, ready: 0, nativeCloses: 0, scopeCloses: 0,
    initializationEntered: deferred(), initializationGate: deferred(), executions: 0,
    settled: false, events: [] };
  if (!holdInitialization) state.initializationGate.resolve();
  globalThis[root] = state;
  fs.writeFileSync(sdk, `const state = globalThis[${JSON.stringify(root)}];
state.imported.resolve();
await state.importGate.promise;
export function query({ prompt, options }) {
  state.options = options;
  state.created++;
  state.dispatch = prompt.next().then(item => { if (!item.done) state.executions++; });
  return state.query;
}
`);
  state.query = {
    recordInboundSessionMessage() {},
    async handleControlRequest() {},
    async processControlRequest() {},
    transport: { write() {} },
    async interrupt() {},
    async getContextUsage() {},
    async next() { return { done: true }; },
    async streamInput() {},
    async initializationResult() {
      state.initializationEntered.resolve();
      await state.initializationGate.promise;
      return { capabilities: ["msg_lifecycle_v1"] };
    },
    async close() {
      state.nativeCloses++;
      state.closeEntered.resolve();
      await state.closeGate.promise;
      state.events.push("native-closed");
    },
  };
  const config = { sdk, resident: true, binding: path.join(root, "binding"), hooks: [] };
  const resident = new Resident(config);
  const scope = resident.scope;
  scope.open = async () => { scope.handle = {}; };
  scope.close = async () => {
    state.scopeCloses++;
    state.events.push("scope-closed");
    scope.handle = null;
    scope.closed = true;
  };
  const create = (extra = {}, compose = (_config, options) => ({ ...options, hooks: {} })) => {
    state.pending = resident.create("fixture", { cwd: process.cwd(), ...extra },
      compose, config => { state.ready++; state.readyConfig = config; })
      .then(value => ({ value }), error => ({ error }))
      .finally(() => { state.settled = true; });
    return state.pending;
  };
  t.after(async () => {
    state.importGate.resolve();
    state.initializationGate.resolve();
    state.closeGate.resolve();
    try {
      await state.pending;
      await state.dispatch;
      await resident.close().catch(() => {});
      await state.daemon?.stop();
    } finally {
      delete globalThis[root];
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
  return { state, config, resident, scope, create };
}
