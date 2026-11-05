/** Real Bash timeout/background and close admission against the pinned Worker. */
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

async function waitFor(check, milliseconds, label) {
  const deadline = Date.now() + milliseconds;
  while (!check()) {
    assert(Date.now() < deadline, `${label} deadline`);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

export async function timeoutSafety({ session, results, root, update, state }) {
  const evidence = [];
  try {
    for (const mode of ["natural", "close"]) {
      const name = `timeout-${mode}`;
      const observed = [], messages = [], updates = [];
      const childFile = path.resolve(`${name}.mjs`);
      const pidFile = path.resolve(`${name}.pid`);
      const release = path.resolve(`${name}.release`);
      fs.writeFileSync(childFile, `import fs from "node:fs";
fs.writeFileSync(${JSON.stringify(pidFile)}, String(process.pid));
const timeout = setTimeout(() => process.exit(7), 120000);
const poll = setInterval(() => {
  if (fs.existsSync(${JSON.stringify(release)})) {
    clearTimeout(timeout); clearInterval(poll); process.exit(0);
  }
}, 50);
`);
      const q = await session(true, { hooks: { PreToolUse: [{ hooks: [async (input) => {
        observed.push(input); return {};
      }] }] } });
      let firstResult;
      const reading = (async () => {
        for await (const message of q.q) {
          messages.push(message);
          if (message.type === "result") firstResult ??= message;
        }
      })();
      // Observe reader failures immediately while preserving them for teardown.
      let readerError;
      reading.catch((error) => { readerError = error; });
      const live = () => {
        if (!fs.existsSync(pidFile)) return false;
        const pid = Number(fs.readFileSync(pidFile));
        try {
          const status = fs.readFileSync(`/proc/${pid}/stat`, "utf8").split(") ")[1].split(" ")[0];
          return !["Z", "X"].includes(status);
        } catch (error) {
          if (error.code === "ENOENT") return false;
          throw error;
        }
      };
      try {
        q.send(`Use Bash exactly once with ${JSON.stringify({ command: `node ${childFile}`, timeout: 1000 })}. After the tool returns, reply DONE and stop. Do not wait for background completion or run other tools.`);
        await waitFor(() => { if (readerError) throw readerError; return firstResult; }, 90000, "first result");
        assert.equal(firstResult.is_error, false);
        assert(observed.some((input) => input.tool_name === "Bash" &&
          input.tool_input.timeout === 1000 && !input.tool_input.run_in_background));
        assert(messages.some((message) => message.subtype === "background_tasks_changed" && message.tasks.length));
        assert(live(), "native timeout did not leave the child running");
        const pid = Number(fs.readFileSync(pidFile));
        evidence.push({ mode, pid, command: ["node", childFile], cwd: process.cwd(), ports: [],
          deadline_seconds: 120, stop: `kill -TERM -- ${pid}`, observed, messages, updates });
        for (let attempt = 0; attempt < 3; attempt++) {
          assert(live());
          updates.push(state());
          update("candidate.json", true);
          assert(live());
          await new Promise((resolve) => setTimeout(resolve, 100));
        }
        if (mode === "natural") {
          fs.writeFileSync(release, "finish");
          await waitFor(() => !live(), 5000, "child exit");
          await waitFor(() => {
            if (readerError) throw readerError;
            const current = state();
            if (current.requests === 0 && current.tools === 0) return true;
            update("candidate.json", true);
            return false;
          }, 30000, "background notification cleanup");
          assert(messages.some((message) => message.subtype === "task_notification"));
        } else {
          await q.q.return();
          assert(!live(), "Query close returned before its background child stopped");
        }
        update("candidate.json");
        results.push({ case: `native-timeout-${mode === "natural" ? "background-busy" : "close-cleanup"}`, status: "passed" });
      } finally {
        fs.writeFileSync(release, "finish");
        await q.q.return();
        await reading;
      }
    }
  } finally {
    fs.writeFileSync(path.join(root, "timeout.json"), JSON.stringify(evidence, null, 2));
  }
}
