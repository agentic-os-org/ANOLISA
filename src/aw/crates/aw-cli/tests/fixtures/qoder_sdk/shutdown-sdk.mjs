/** A live SDK transport that must acknowledge cleanup before daemon shutdown. */
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import { createInterface } from "node:readline";
import { query as fixtureQuery } from "./fixture-sdk.mjs";

let child, lines, exited;
export function accessTokenFromEnv() { return {}; }
export async function startDaemon() {
  child = spawn(process.execPath, ["-e", `
    process.stdout.write("ready\\n");
    process.stdin.on("data", () => process.stdout.write("drained\\n"));
    process.stdin.on("end", () => process.exit(0));
  `], { stdio: ["pipe", "pipe", "inherit"] });
  exited = new Promise(resolve => child.once("exit", resolve));
  lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
  assert.equal((await lines.next()).value, "ready");
  fs.writeFileSync("sdk-process.json", JSON.stringify({pid:child.pid, worker:process.pid,
    command:child.spawnargs, cwd:process.cwd(), ports:[], timeout_seconds:14,
    stop:`kill -TERM ${child.pid}`}));
  return { async stop() {
    child.stdin.end();
    await exited;
    fs.writeFileSync("daemon-stopped", "yes");
  } };
}
export function query(request) {
  const q = fixtureQuery(request);
  let wake, closed = false, closing;
  q.next = async () => {
    fs.writeFileSync("consuming", "yes");
    if (!closed) await new Promise(resolve => { wake = resolve; });
    return { done:true };
  };
  q.close = () => closing ??= (async () => {
    fs.writeFileSync("closing", "yes");
    if (process.env.QODER_STUBBORN) await new Promise(() => {});
    child.stdin.write("drain\n");
    assert.equal((await lines.next()).value, "drained");
    fs.writeFileSync("query-drained", "yes");
    closed = true; wake?.();
  })();
  q.return = async () => { await q.close(); return {done:true}; };
  return q;
}
