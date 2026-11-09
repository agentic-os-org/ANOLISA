/** The legacy run factory shares task ownership and explicit idle updates. */
import assert from "node:assert/strict";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { createSession as factory, runWorker, nativeArguments } from "./native-public.mjs";

export async function createSession(options) {
  return await factory({ index: "entry", ...options });
}

export async function run(session, { createSession, closeSession, args }) {
  assert.deepEqual(args, nativeArguments);
  const root = process.env.FAKE_ROOT;
  await session.prompt("fixture");
  assert.ok(existsSync(join(root, "effect")));
  await closeSession(session);
  await assert.rejects(session.prompt("closed task"), /closed/);
  await runWorker({ createSession, args });
  const path = join(root, "native-result.json");
  const result = JSON.parse(readFileSync(path, "utf8"));
  result.cases.push("run-entrypoint", "initial-session-cleanup");
  writeFileSync(path, JSON.stringify(result));
}
