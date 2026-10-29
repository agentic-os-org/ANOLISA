/** A real wrapper whose receipt is published only after its child process joins. */
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const [ticket, encoded] = process.argv.slice(2);
const record = JSON.parse(readFileSync(join(root, ticket + ".json"), "utf8"));
const command = Buffer.from(encoded, "base64url").toString("utf8");
assert.equal(command, record.command);
function identity(pid, group) {
  const fields = readFileSync(`/proc/${pid}/stat`, "utf8").split(") ")[1].split(" ");
  return { pid, start_ticks: fields[19], pgid: Number(fields[2]), group, kind: "consumer", cwd: root };
}
const child = spawn("/bin/sh", ["-c", command], { cwd: root, stdio: "inherit", detached: true });
writeFileSync(join(root, ticket + ".started"), JSON.stringify({ wrapper: process.pid, child: child.pid,
  processes: [identity(child.pid, true), identity(process.pid, false)],
  command: ["/bin/sh", "-c", command], cwd: root, ports: [], log: "inherited native Bash output",
  deadline_seconds: 90, stop: `kill -TERM -- -${child.pid}` }));
const exit = await new Promise((resolve, reject) => {
  child.once("error", reject);
  child.once("exit", (code, signal) => resolve({ code, signal }));
});
writeFileSync(join(root, ticket + ".joined"), JSON.stringify(exit));
process.exitCode = exit.code ?? 128;
