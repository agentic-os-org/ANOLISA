/** Native command leases retain logical policy input through the real SDK parser. */
import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { pause, deferred } from "./parser-support.mjs";
import { wrapper, ticket, lease, setup, preTool, prepare, retire } from "./parser-command-wrapper-support.mjs";


test("command wrapper scans immutable logical input and preserves ordinary permission", { timeout: 5000 }, async t => {
  const applicationPosts = [];
  const f = await setup(t, {}, async (input, id) => {
    if (input.hook_event_name === "PostToolUse") applicationPosts.push({ input, id });
    return {};
  });
  const original = { command: "echo '$SECRET'", description: "keep" };
  const response = await prepare(f, "call", original);
  const native = response.hookSpecificOutput.updatedInput;
  assert.equal(response.hookSpecificOutput.permissionDecision, "ask");
  assert.equal(native.command, `'${wrapper}' '${ticket}' '${Buffer.from(original.command).toString("base64url")}'`);
  assert.deepEqual(original, { command: "echo '$SECRET'", description: "keep" });
  assert.deepEqual(f.state.policies[0].payload.tool_input, original);
  assert(!JSON.stringify(f.state.policies).includes(ticket));
  assert.equal(f.state.released.length, 0);
  f.hook("call", "post-app", "PostToolUse", native);
  await f.response("post-app");
  assert.deepEqual(applicationPosts[0].input.tool_input, original);
  assert.equal(applicationPosts[0].input.tool_use_id, "call");
  assert.equal(applicationPosts[0].id, "call");
  assert(!JSON.stringify(applicationPosts).includes(ticket));
  f.hook("call", "post-policy", "PostToolUse", native, { group: 1 });
  await f.response("post-policy");
  assert.deepEqual(f.state.policies[1].payload.tool_input, original);
  assert(!JSON.stringify(f.state.policies).includes(ticket));
  assert.equal(f.state.released.length, 0);
  await retire(f, "call");
  assert.deepEqual(f.state.released, [f.state.acquired[0].invocationId]);
});

test("security denial prevents ticket acquisition and native wrapping", { timeout: 5000 }, async t => {
  const f = await setup(t);
  f.scope.invoke = async () => ({ code: 2, stdout: Buffer.from('"denied"') });
  assert.equal((await prepare(f, "denied", { command: "blocked" })).hookSpecificOutput.permissionDecision, "deny");
  assert.deepEqual(f.state.acquired, []);
});

test("parallel native Hook dispatch still waits for all AW policy steps before acquisition", { timeout: 5000 }, async t => {
  const entered = deferred(), release = deferred();
  const f = await setup(t);
  f.scope.invoke = async () => {
    entered.resolve();
    await release.promise;
    return { code: 0, stdout: Buffer.from("{}") };
  };
  const preparing = prepare(f, "early", { command: "checked after suspension" });
  await entered.promise;
  assert.equal(f.state.acquired.length, 0);
  release.resolve();
  assert.equal((await preparing).hookSpecificOutput.permissionDecision, "ask");
  assert.equal(f.state.acquired.length, 1);
  await retire(f, "early");
});

test("parallel application denial remains authoritative during credential acquisition", { timeout: 5000 }, async t => {
  const entered = deferred(), releaseFactory = deferred(), denyApplication = deferred();
  let released = false, settled = false;
  const f = await setup(t, { awNativeExecutionFactory: async () => {
    entered.resolve();
    await releaseFactory.promise;
    return { ...lease(async () => { released = true; }), settle: async () => { settled = true; } };
  } }, async () => {
    await denyApplication.promise;
    return { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny" } };
  });
  f.model("denied-parallel");
  const preparing = f.parallelHooks("denied-parallel", "pre", "PreToolUse", { command: "fixture" });
  await entered.promise;
  denyApplication.resolve();
  await pause();
  releaseFactory.resolve();
  const responses = await preparing;
  assert(responses.every(value => value.response.hookSpecificOutput.permissionDecision === "deny"));
  assert.equal(settled, false);
  assert.equal(released, true);
});

test("native permission sees logical input and denial retires an unused ticket", { timeout: 5000 }, async t => {
  const seen = [];
  const f = await setup(t, { canUseTool: async (_name, input) => {
    seen.push(input);
    return { behavior: "deny", message: "business veto" };
  } });
  const prepared = await preTool(f);
  f.transport.emit({ type: "control_request", request_id: "permission", request: {
    subtype: "can_use_tool", tool_name: "Bash", tool_use_id: "call",
    input: { command: "echo logical" } } });
  assert.equal((await f.response("permission")).response.behavior, "deny");
  assert.deepEqual(seen, [{ command: "echo logical" }]);
  assert.equal(f.state.released.length, 1);
});

test("acceptEdits mode authorizes logical input before wrapper publication", { timeout: 5000 }, async t => {
  const f = await setup(t, { permissionMode: "acceptEdits" });
  assert.equal((await prepare(f)).hookSpecificOutput.permissionDecision, "ask");
  await retire(f, "call");
});

test("changed Post arguments fail closed before the application Hook", { timeout: 5000 }, async t => {
  let posts = 0;
  const f = await setup(t, {}, async input => { if (input.hook_event_name === "PostToolUse") posts++; return {}; });
  const prepared = await prepare(f);
  f.hook("call", "forged-post", "PostToolUse", {
    ...prepared.hookSpecificOutput.updatedInput, command: "unrelated command" });
  await f.response("forged-post");
  assert.equal(posts, 0);
  assert.match(String(f.resident.failure), /changed native Hook input/);
});

test("terminal queue barrier retains the ticket through original Post finalizers", { timeout: 5000 }, async t => {
  const entered = deferred(), release = deferred();
  const f = await setup(t, {}, async input => {
    if (input.hook_event_name === "PostToolUse") { entered.resolve(); await release.promise; }
    return {};
  });
  const prepared = await prepare(f);
  f.hook("call", "held-post", "PostToolUse", prepared.hookSpecificOutput.updatedInput);
  await entered.promise;
  f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: "call" }] } });
  await pause();
  assert.equal(f.state.released.length, 0);
  assert.equal(f.scope.invocations.size, 1);
  release.resolve();
  await f.response("held-post");
  for (let count = 0; f.scope.invocations.size && count < 1000; count++) await pause();
  assert.equal(f.state.released.length, 1);
});

test("invalid wrapper acquisition releases its lease and fences the Query", { timeout: 5000 }, async t => {
  let released = false;
  const f = await setup(t, { awNativeExecutionFactory: async () => ({
    ...lease(async () => { released = true; }), commandWrapper: { executable: "/unadmitted", ticket },
  }) });
  const response = await prepare(f);
  assert.equal(response.hookSpecificOutput.permissionDecision, "deny");
  assert.equal(released, true);
  assert(f.resident.failure);
});

test("native close joins Worker shutdown before retiring outstanding tickets", { timeout: 5000 }, async t => {
  const f = await setup(t);
  await prepare(f);
  assert.equal(f.state.released.length, 0);
  await f.query.close();
  assert.equal(f.transport.closed, true);
  assert.equal(f.state.released.length, 1);
});

test("explicit background execution rejects before credential acquisition", { timeout: 5000 }, async t => {
  const f = await setup(t);
  const result = await prepare(f, "background", { command: "echo fixture", run_in_background: true });
  assert.equal(result.hookSpecificOutput.permissionDecision, "deny");
  assert.equal(f.state.acquired.length, 0);
});

test("concurrent native calls keep distinct immutable leases and terminal release", { timeout: 5000 }, async t => {
  const f = await setup(t);
  const first = await prepare(f, "first", { command: "echo first" });
  const second = await prepare(f, "second", { command: "echo second" });
  assert.notEqual(first.hookSpecificOutput.updatedInput.command, second.hookSpecificOutput.updatedInput.command);
  assert.notEqual(f.state.acquired[0].invocationId, f.state.acquired[1].invocationId);
  f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: "second" }] } });
  for (let count = 0; !f.state.released.length && count < 1000; count++) await pause();
  assert.deepEqual(f.state.released, [f.state.acquired[1].invocationId]);
  await retire(f, "first");
  assert.equal(f.state.released.length, 2);
});

test("query close waits for a suspended factory and releases its unused lease", { timeout: 5000 }, async t => {
  const entered = deferred(), release = deferred();
  let released = false;
  const f = await setup(t, { awNativeExecutionFactory: async () => {
    entered.resolve();
    await release.promise;
    return lease(async () => { released = true; });
  } });
  const input = { command: "echo held" };
  f.model("held", input);
  f.hook("held", "held-wrapper", "PreToolUse", input, { group: 1 });
  await entered.promise;
  let closed = false;
  const closing = f.query.close().then(() => { closed = true; });
  await pause();
  assert.equal(closed, false);
  assert.equal(released, false);
  release.resolve();
  await closing;
  assert.equal(released, true);
  assert.equal(f.transport.closed, true);
});

test("facade permission changes reject modes that can skip wrapper approval", { timeout: 5000 }, async t => {
  const f = await setup(t, { permissionMode: "acceptEdits" });
  await f.query.setPermissionMode("default");
  assert.equal((await prepare(f, "normal")).hookSpecificOutput.permissionDecision, "ask");
  await retire(f, "normal");
  for (const mode of ["yolo", "bypassPermissions", "auto", "dontAsk", "plan"]) {
    const count = f.transport.requests.length;
    await assert.rejects(f.query.setPermissionMode(mode), /requires default or acceptEdits/);
    assert.equal(f.transport.requests.length, count);
  }
  assert.equal((await prepare(f, "unchanged")).hookSpecificOutput.permissionDecision, "ask");
  await retire(f, "unchanged");
});

test("pending permission downgrade blocks wrapper publication", { timeout: 5000 }, async t => {
  const f = await setup(t, { permissionMode: "acceptEdits" });
  f.transport.holdPermission = true;
  const changing = f.query.setPermissionMode("default");
  for (let count = 0; !f.transport.permissionRequest && count < 100; count++) await pause();
  assert(f.transport.permissionRequest);
  let prepared = false;
  const preparing = prepare(f).then(value => { prepared = true; return value; });
  for (let count = 0; !f.state.acquired.length && count < 100; count++) await pause();
  assert.equal(prepared, false);
  f.transport.holdPermission = false;
  f.transport.write(JSON.stringify(f.transport.permissionRequest));
  await changing;
  assert.equal((await preparing).hookSpecificOutput.permissionDecision, "ask");
  await retire(f, "call");
});

test("Query close retains the ticket until the original Post finally has settled", { timeout: 5000 }, async t => {
  const entered = deferred(), release = deferred(), settling = deferred(), joined = deferred();
  const f = await setup(t, { awNativeExecutionFactory: async context => {
    f.state.acquired.push(context);
    return { ...lease(async () => f.state.released.push(context.invocationId)),
      settle: async ({ signal }) => {
        assert.equal(signal.aborted, true);
        settling.resolve();
        await joined.promise;
      },
    };
  } }, async input => {
    if (input.hook_event_name === "PostToolUse") {
      try { entered.resolve(); await release.promise; }
      finally { assert.equal(f.state.released.length, 0); }
    }
    return {};
  });
  const prepared = await prepare(f);
  f.hook("call", "close-post", "PostToolUse", prepared.hookSpecificOutput.updatedInput);
  await entered.promise;
  let closed = false;
  const closing = f.query.close().then(() => { closed = true; });
  await settling.promise;
  assert.equal(f.transport.closed, false, "consumer join must precede native shutdown");
  assert.equal(closed, false);
  assert.equal(f.state.released.length, 0);
  joined.resolve();
  for (let count = 0; !f.transport.closed && count < 100; count++) await pause();
  assert.equal(f.transport.closed, true);
  assert.equal(closed, false);
  assert.equal(f.state.released.length, 0, "Post finally still owns the ticket after consumer ACK");
  release.resolve();
  await closing;
  assert.equal(f.state.released.length, 1);
});

for (const mode of ["timeout auto-background", "silent completion", "delayed completion"]) {
  test(`${mode} cannot retire a live consumer through native result metadata`, { timeout: 5000 }, async t => {
    const entered = deferred(), complete = deferred();
    let released = false;
    const f = await setup(t, { awNativeExecutionFactory: async () => ({
      ...lease(async () => { released = true; }), settle: async () => {
        entered.resolve();
        await complete.promise;
      },
    }) });
    await prepare(f, "timeout", { command: "long command", timeout: 1000, run_in_background: false });
    if (mode === "delayed completion")
      f.transport.emit({ type: "system", subtype: "background_tasks_changed", tasks: [
        { task_id: "observed-task", task_type: "bash", description: "background operation" },
      ], session_id: "session", uuid: "early-snapshot" });
    f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: "timeout" }] } });
    await entered.promise;
    assert.equal(released, false);
    assert.equal(f.scope.invocations.size, 1);
    if (mode === "timeout auto-background")
      f.transport.emit({ type: "system", subtype: "background_tasks_changed", tasks: [
        { task_id: "late-task", task_type: "bash", description: "late timeout promotion" },
      ], session_id: "session", uuid: "late-snapshot" });
    else if (mode === "delayed completion") {
      f.transport.emit({ type: "system", subtype: "background_tasks_changed", tasks: [],
        session_id: "session", uuid: "finished-snapshot" });
      f.transport.emit({ type: "system", subtype: "task_notification", task_id: "observed-task",
        status: "completed", output_file: "fixture", summary: "metadata precedes consumer finally",
        session_id: "session", uuid: "notification" });
    }
    await pause();
    assert.equal(released, false);
    complete.resolve();
    for (let count = 0; f.scope.invocations.size && count < 1000; count++) await pause();
    assert.equal(released, true);
    assert.equal(f.scope.invocations.size, 0);
    f.transport.emit({ type: "system", subtype: "background_tasks_changed", tasks: [],
      session_id: "session", uuid: "completed-snapshot" });
  });
}

test("Query close cancels and joins a background consumer before ticket retirement", { timeout: 5000 }, async t => {
  const entered = deferred(), aborted = deferred(), joined = deferred();
  let released = false;
  const f = await setup(t, { awNativeExecutionFactory: async () => ({
    ...lease(async () => { released = true; }), settle: async ({ signal }) => {
      entered.resolve();
      await new Promise(resolve => {
        if (signal.aborted) { aborted.resolve(); resolve(); }
        else signal.addEventListener("abort", () => { aborted.resolve(); resolve(); }, { once: true });
      });
      await joined.promise;
    },
  }) });
  await prepare(f);
  f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: "call" }] } });
  await entered.promise;
  let closed = false;
  const closing = f.query.close().then(() => { closed = true; });
  await aborted.promise;
  assert.equal(closed, false);
  assert.equal(released, false);
  joined.resolve();
  await closing;
  assert.equal(released, true);
});

test("missing consumer completion contract rejects before publishing the wrapper", { timeout: 5000 }, async t => {
  let released = false;
  const f = await setup(t, { awNativeExecutionFactory: async () => {
    const result = lease(async () => { released = true; });
    delete result.settle;
    return result;
  } });
  assert.equal((await prepare(f)).hookSpecificOutput.permissionDecision, "deny");
  assert.equal(released, true);
});

test("uncertain consumer cleanup retains credentials and fences future Queries", { timeout: 5000 }, () => {
  for (const mode of ["settle-error", "missing-release"]) {
    const child = spawnSync(process.execPath, [fileURLToPath(new URL("./command-wrapper-cleanup.mjs", import.meta.url)),
      process.argv[2], mode], { encoding: "utf8", timeout: 2000 });
    assert.equal(child.status, 0, `${child.stdout}\n${child.stderr}\n${child.error ?? ""}`);
  }
});

test("consumer cleanup deadline retains tickets until original join and fences the process", { timeout: 5000 }, () => {
  const child = spawnSync(process.execPath, [fileURLToPath(new URL("./command-wrapper-cleanup.mjs", import.meta.url)),
    process.argv[2], "timeout"], { encoding: "utf8", timeout: 3000 });
  assert.equal(child.status, 0, `${child.stdout}\n${child.stderr}\n${child.error ?? ""}`);
});


test("wrapper waits for the latest queued permission downgrade", { timeout: 5000 }, async t => {
  const f = await setup(t, { permissionMode: "acceptEdits" });
  f.transport.holdPermission = true;
  const first = f.query.setPermissionMode("acceptEdits");
  for (let count = 0; !f.transport.permissionRequest && count < 1000; count++) await pause();
  const firstRequest = f.transport.permissionRequest;
  assert(firstRequest);
  const preparing = prepare(f, "queued-mode");
  for (let count = 0; !f.state.acquired.length && count < 1000; count++) await pause();
  assert.equal(f.state.acquired.length, 1);
  const downgrade = f.query.setPermissionMode("default");
  let prepared = false;
  preparing.then(() => { prepared = true; });
  f.transport.emit({ type: "control_response", response: { subtype: "success",
    request_id: firstRequest.request_id, response: {} } });
  await first;
  for (let count = 0; f.transport.permissionRequest === firstRequest && count < 1000; count++) await pause();
  assert.notEqual(f.transport.permissionRequest, firstRequest);
  await pause();
  assert.equal(prepared, false);
  f.transport.emit({ type: "control_response", response: { subtype: "success",
    request_id: f.transport.permissionRequest.request_id, response: {} } });
  await downgrade;
  assert.equal((await preparing).hookSpecificOutput.permissionDecision, "ask");
  await retire(f, "queued-mode");
});


for (const cancel of ["natural", "close", "signal"]) {
  test(`iterator EOF ${cancel} preserves consumer ACK before native close`,
    { timeout: 5000 }, async t => {
      const entered = deferred(), joined = deferred(), aborted = deferred();
      const events = [];
      let signal;
      const controller = new AbortController();
      const f = await setup(t, { abortController: controller, awNativeExecutionFactory: async () => ({
        ...lease(async () => events.push("released")), settle: async options => {
          signal = options.signal;
          signal.addEventListener("abort", () => aborted.resolve(), { once: true });
          entered.resolve();
          await joined.promise;
          events.push("consumer-ACK");
        },
      }) });
      try {
        await prepare(f);
        f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: "call" }] } });
        await entered.promise;
        const nativeClose = f.resident.nativeClose;
        f.resident.nativeClose = async () => {
          assert(events.includes("consumer-ACK"), "native close killed the consumer watchdog before ACK");
          events.push("native-close");
          return await nativeClose();
        };
        f.resident.query.inputStream.done();
        const eof = (async () => { for await (const _message of f.query) {} })();
        await pause();
        assert.equal(f.resident.closing, true);
        assert.equal(f.resident.cleanupDeadline, undefined, "graceful EOF must not time out a live task");
        assert.equal(signal.aborted, false);
        assert.equal(f.transport.closed, false);
        let cancelling;
        if (cancel !== "natural") {
          if (cancel === "signal") controller.abort();
          else cancelling = f.query.close();
          await aborted.promise;
          assert.equal(signal.aborted, true);
          assert.equal(f.transport.closed, false, "cancellation must also join consumer ACK first");
        }
        joined.resolve();
        await Promise.all([eof, cancelling]);
        assert(events.includes("released"));
        assert(events.indexOf("consumer-ACK") < events.indexOf("native-close"));
        assert.equal(f.resident.nativeAbort.signal.aborted, cancel === "signal",
          "native abort must preserve the caller's cancellation semantics");
        assert.equal(f.resident.failure, null, "confirmed shutdown must not become a cancelled Pre delivery");
      } finally { joined.resolve(); }
    });
}

test("foreground cancellation starts settlement without waiting for a native tool result", { timeout: 5000 }, async t => {
  const entered = deferred(), joined = deferred();
  const events = [];
  const f = await setup(t, { awNativeExecutionFactory: async () => ({
    ...lease(async () => events.push("released")), settle: async ({ signal }) => {
      assert.equal(signal.aborted, true);
      entered.resolve();
      await joined.promise;
      events.push("consumer-ACK");
    },
  }) });
  try {
    await prepare(f);
    f.resident.background.add("native-task");
    f.resident.query.stopTask = async () => {
      assert(events.includes("consumer-ACK"));
      events.push("native-stop");
    };
    const nativeClose = f.resident.nativeClose;
    f.resident.nativeClose = async () => { events.push("native-close"); return await nativeClose(); };
    const closing = f.query.close();
    await entered.promise;
    f.resident.query.transport.endInput();
    await pause();
    assert.equal(f.transport.inputEnds ?? 0, 0, "finite input EOF bypassed a closing consumer's ACK");
    assert.equal(f.transport.closed, false);
    assert.deepEqual(events, []);
    joined.resolve();
    await closing;
    assert.deepEqual(events, ["consumer-ACK", "native-stop", "native-close", "released"]);
  } finally { joined.resolve(); }
});

test("finite EOF skips only an acknowledged consumer's correlated background task", { timeout: 5000 }, async t => {
  const f = await setup(t);
  await prepare(f, "background");
  f.transport.emit({ type: "system", subtype: "task_started", task_id: "owned",
    tool_use_id: "background", session_id: "session", uuid: "start", description: "fixture" });
  f.transport.emit({ type: "system", subtype: "background_tasks_changed", tasks: [
    { task_id: "owned", task_type: "bash", description: "fixture" },
  ], session_id: "session", uuid: "snapshot" });
  await retire(f, "background");
  assert.equal(f.resident.nativeCommands.records.size, 0);
  assert.equal(f.resident.nativeCommands.backgroundConsumers.size, 1);
  // Finite native input has already closed the session. A stale task snapshot
  // must not send a control request after its own consumer joined successfully.
  f.resident.query.stopTask = async () => { throw new Error("closed native transport"); };
  await f.query.close();
  assert.equal(f.scope.closed, true);
  assert.equal(f.resident.nativeCommands.backgroundConsumers.size, 0);
});

for (const identity of ["unrelated", "wrong-session", "unknown"]) {
  test(`consumer ACK cannot suppress ${identity} background shutdown failure`, { timeout: 5000 }, async () => {
    const f = await setup({ after: () => {} });
    try {
      await prepare(f, "background");
      f.transport.emit({ type: "system", subtype: "task_started", task_id: "unconfirmed",
        ...(identity === "unknown" ? {} : { tool_use_id: identity === "unrelated" ? "another" : "background" }),
        session_id: identity === "wrong-session" ? "another-session" : "session", uuid: "start", description: "fixture" });
      f.transport.emit({ type: "system", subtype: "background_tasks_changed", tasks: [
        { task_id: "unconfirmed", task_type: "bash", description: "fixture" },
      ], session_id: "session", uuid: "snapshot" });
      await retire(f, "background");
      const stopped = [];
      f.resident.query.stopTask = async id => { stopped.push(id); throw new Error("native task unconfirmed"); };
      await assert.rejects(f.query.close(), /cleanup incomplete/);
      assert.deepEqual(stopped, ["unconfirmed"]);
      assert.notEqual(f.scope.closed, true, "native task uncertainty must retain the scope");
      assert.equal(f.transport.closed, true);
    } finally { await f.resident.close().catch(() => {}); }
  });
}

test("background task correlation is reclaimed with the live snapshot", { timeout: 5000 }, async t => {
  const f = await setup(t);
  await prepare(f, "background");
  f.transport.emit({ type: "system", subtype: "task_progress", task_id: "owned",
    tool_use_id: "background", session_id: "session", uuid: "progress", description: "fixture",
    usage: { tool_uses: 0, duration_ms: 1 } });
  for (const tasks of [[{ task_id: "owned", task_type: "bash", description: "fixture" }], []]) {
    f.transport.emit({ type: "system", subtype: "background_tasks_changed", tasks,
      session_id: "session", uuid: "snapshot" });
    await pause();
    assert.equal(f.resident.nativeCommands.backgroundConsumers.size, tasks.length);
  }
  await retire(f, "background");
});

test("first background snapshot may follow consumer ACK and identity retirement", { timeout: 5000 }, async t => {
  const f = await setup(t);
  await prepare(f, "background");
  f.transport.emit({ type: "system", subtype: "task_started", task_id: "late",
    tool_use_id: "background", session_id: "session", uuid: "start", description: "fixture" });
  await retire(f, "background");
  assert.equal(f.resident.nativeCommands.records.size, 0);
  assert.equal(f.resident.nativeCommands.backgroundConsumers.size, 1);
  f.transport.emit({ type: "system", subtype: "background_tasks_changed", tasks: [
    { task_id: "late", task_type: "local_bash", description: "fixture" },
  ], session_id: "session", uuid: "first-snapshot" });
  await pause();
  assert.equal(f.resident.nativeCommands.acknowledgedTask("late"), true);
  f.resident.query.stopTask = async () => { throw new Error("finite EOF closed native transport"); };
  await f.query.close();
  assert.equal(f.scope.closed, true);
  assert.equal(f.resident.nativeCommands.backgroundConsumers.size, 0);
});

test("foreign-session background snapshots cannot establish consumer ownership", { timeout: 5000 }, async t => {
  const f = await setup(t);
  await prepare(f, "background");
  f.transport.emit({ type: "system", subtype: "task_started", task_id: "owned",
    tool_use_id: "background", session_id: "session", uuid: "start", description: "fixture" });
  await retire(f, "background");
  f.transport.emit({ type: "system", subtype: "background_tasks_changed", tasks: [
    { task_id: "owned", task_type: "local_bash", description: "fixture" },
  ], session_id: "foreign-session", uuid: "foreign-snapshot" });
  await pause();
  assert.match(String(f.resident.failure), /background task session identity changed/);
  assert.equal(f.resident.background.size, 0, "rejected snapshot changed native shutdown identities");
  await assert.rejects(f.query.next(), /background task session identity changed/);
});

test("terminal native task receipts reclaim pre-snapshot owners across long sessions", { timeout: 15000 }, async t => {
  const f = await setup(t);
  for (let index = 0; index < 600; index++) {
    const id = `completed-${index}`;
    await prepare(f, id);
    f.transport.emit({ type: "system", subtype: "task_started", task_id: id,
      tool_use_id: id, session_id: "session", uuid: `start-${index}`, description: "fixture" });
    await retire(f, id);
    assert.equal(f.resident.nativeCommands.backgroundConsumers.size, 1);
    if (index === 0) {
      for (const receipt of [{ session_id: "foreign", status: "completed" },
        { session_id: "session", tool_use_id: "foreign", status: "completed" },
        { session_id: "session", status: "running" }]) {
        f.transport.emit({ type: "system", subtype: "task_notification", task_id: id,
          ...receipt, uuid: "invalid", summary: "fixture", output_file: "fixture" });
        await pause();
        assert.equal(f.resident.nativeCommands.backgroundConsumers.size, 1);
      }
    }
    f.transport.emit({ type: "system", subtype: "task_notification", task_id: id,
      session_id: "session", status: "completed", uuid: `done-${index}`, summary: "fixture", output_file: "fixture" });
    await pause();
    assert.equal(f.resident.nativeCommands.backgroundConsumers.size, 0);
  }
});

test("unresolved native task announcements use bounded concurrent capacity", { timeout: 5000 }, async t => {
  const f = await setup(t);
  for (let index = 0; index < f.scope.invocationLimit; index++) {
    const id = `unresolved-${index}`;
    await prepare(f, id);
    f.transport.emit({ type: "system", subtype: "task_started", task_id: id,
      tool_use_id: id, session_id: "session", uuid: id, description: "fixture" });
    await retire(f, id);
  }
  assert.equal(f.resident.nativeCommands.backgroundConsumers.size, f.scope.invocationLimit);
  await prepare(f, "overflow");
  f.transport.emit({ type: "system", subtype: "task_started", task_id: "overflow",
    tool_use_id: "overflow", session_id: "session", uuid: "overflow", description: "fixture" });
  await pause();
  assert(f.resident.nativeCommands.backgroundConsumers.size <= f.scope.invocationLimit);
  assert.match(String(f.resident.failure), /native command execution failed/);
  await assert.rejects(f.query.next(), /native command execution failed/);
});

test("finite prompt EOF retains stdin while an automatic background consumer settles", { timeout: 5000 }, async t => {
  const entered = deferred(), joined = deferred(), postEntered = deferred(), postDone = deferred();
  const events = [];
  const source = (async function* () { yield { type: "user", message: { role: "user", content: "fixture" }, parent_tool_use_id: null }; })();
  const f = await setup(t, { awNativeExecutionFactory: async () => ({
    ...lease(async () => events.push("released")), settle: async ({ signal }) => {
      assert.equal(signal.aborted, false);
      entered.resolve();
      await joined.promise;
      events.push("consumer-ACK");
    },
  }) }, async input => {
    if (input.hook_event_name === "PostToolUse") {
      try { postEntered.resolve(); await postDone.promise; }
      finally { assert(!events.includes("released")); events.push("Post-finally"); }
    }
    return {};
  }, source);
  try {
    const prepared = await prepare(f, "automatic", { command: "echo delayed", timeout: 1000 });
    f.hook("automatic", "automatic-post", "PostToolUse", prepared.hookSpecificOutput.updatedInput);
    await postEntered.promise;
    f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: "automatic" }] } });
    f.transport.emit({ type: "result", subtype: "success", session_id: "session", uuid: "result", is_error: false, result: "backgrounded" });
    await entered.promise;
    assert.equal(f.transport.inputEnds ?? 0, 0, "SDK finite-source EOF closed the consumer watchdog");
    assert.equal(f.transport.closed, false);
    joined.resolve();
    for (let count = 0; !f.transport.inputEnds && count < 100; count++) await pause();
    assert.equal(f.transport.inputEnds, 1);
    assert.deepEqual(events, ["consumer-ACK"]);
    const closing = f.query.close();
    for (let count = 0; !f.transport.closed && count < 100; count++) await pause();
    assert.equal(f.transport.closed, true);
    assert(!events.includes("released"));
    postDone.resolve();
    await closing;
    assert.deepEqual(events, ["consumer-ACK", "Post-finally", "released"]);
  } finally { joined.resolve(); postDone.resolve(); }
});
