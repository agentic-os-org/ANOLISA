import test from "node:test";
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";

import { fixture, before, delay } from "./qoder-bridge-fixture.mjs";
import { Task } from "../src/client.mjs";

test(
  "text replies and native initialization finish without AW or input Hooks",
  { timeout: 8000 },
  async () => {
    const f = await fixture();
    try {
      const bridge = await f.bridge();
      const hooks = {
        PreToolUse: [{ matcher: "Bash", hooks: [async () => ({})] }],
      };
      const native = { model: "original", hooks, cwd: f.root + "/work" };
      const session = bridge.query({ prompt: "text", options: native });
      assert.equal(await session.initializationResult(), "native-ready");
      for await (const message of session) assert.equal(message.type, "model");
      assert.deepEqual(f.calls, ["model"]);
      assert.equal(f.options[0].model, "original");
      assert.equal(f.options[0].hooks.UserPromptSubmit, undefined);
      assert.equal(hooks.PreToolUse.length, 1);
      assert.equal(
        f.options[0].hooks.PreToolUse[0].hooks[0],
        hooks.PreToolUse[0].hooks[0],
      );
      assert.equal(bridge.state, "detached");
    } finally {
      await f.close();
    }
  },
);

test(
  "a model already running acquires Scope at its first tool after PID attach",
  { timeout: 15000 },
  async () => {
    const f = await fixture();
    try {
      const bridge = await f.bridge({ attachTimeoutMs: 5000 });
      const session = bridge.query({
        prompt: "tool",
        options: { cwd: f.root + "/work" },
      });
      const iterator = session[Symbol.asyncIterator]();
      assert.equal((await iterator.next()).value.type, "model");
      const result = iterator.next();
      await delay(30);
      assert.deepEqual(f.calls, ["model"]);
      await f.attach();
      assert.equal((await result).value.type, "result");
      assert((await iterator.next()).done);
      assert.deepEqual(f.calls, ["model", "tool"]);
    } finally {
      await f.close();
    }
  },
);

for (const required of [true, false]) {
  test(
    `configured input waits for attachment before model dispatch (required=${required})`,
    { timeout: 15000 },
    async () => {
      const f = await fixture(required, true);
      try {
        const bridge = await f.bridge({ attachTimeoutMs: 5000 });
        const iterator = bridge
          .query({ prompt: "text" })
          [Symbol.asyncIterator]();
        let settled = false;
        const first = iterator.next().then((result) => {
          settled = true;
          return result;
        });
        await delay(30);
        assert(!settled);
        assert.deepEqual(f.calls, ["input"]);
        await f.attach();
        assert.equal((await first).value.type, "model");
        assert.deepEqual(f.calls, ["input", "model"]);
        assert((await iterator.next()).done);
      } finally {
        await f.close();
      }
    },
  );
}

test(
  "input denial stops native model dispatch",
  { timeout: 15000 },
  async () => {
    const f = await fixture(true, true);
    try {
      const bridge = await f.bridge();
      await writeFile(f.root + "/deny", "deny");
      await f.attach();
      for await (const _ of bridge.query({ prompt: "text" })) {
      }
      assert.deepEqual(f.calls, ["input", "input-denied"]);
    } finally {
      await f.close();
    }
  },
);

test(
  "required tool timeout and explicit policy denial prevent native dispatch",
  { timeout: 15000 },
  async () => {
    const f = await fixture();
    try {
      const bridge = await f.bridge();
      await assert.rejects(async () => {
        for await (const _ of bridge.query({ prompt: "tool" })) {
        }
      });
      assert.deepEqual(f.calls, ["model", "denied"]);
      await writeFile(f.root + "/deny", "deny");
      await f.attach();
      for await (const _ of bridge.query({ prompt: "tool" })) {
      }
      assert.deepEqual(f.calls, ["model", "denied", "model", "denied"]);
    } finally {
      await f.close();
    }
  },
);

test(
  "optional no-attach task falls back once and preserves native hooks",
  { timeout: 8000 },
  async () => {
    const f = await fixture(false);
    try {
      const bridge = await f.bridge({ attachTimeoutMs: 5000 });
      const begin = performance.now();
      let native = 0;
      const hooks = {
        PreToolUse: [
          {
            hooks: [
              async () => {
                native++;
                return {};
              },
            ],
          },
        ],
      };
      for await (const _ of bridge.query({
        prompt: "tool",
        options: { hooks },
      })) {
      }
      assert(
        performance.now() - begin < 1000,
        "fallback must not wait for attachment",
      );
      assert.equal(native, 1);
      assert.deepEqual(f.calls, ["model", "tool"]);
    } finally {
      await f.close();
    }
  },
);

test(
  "explicit close cancels pending admission and closes the native Query",
  { timeout: 8000 },
  async () => {
    const f = await fixture();
    try {
      const bridge = await f.bridge({ attachTimeoutMs: 5000 });
      const session = bridge.query({ prompt: "tool" });
      const iterator = session[Symbol.asyncIterator]();
      await iterator.next();
      const next = iterator.next();
      const observed = assert.rejects(next);
      await delay(30);
      await session.close();
      await observed;
      assert(session.closed);
      assert(!f.calls.includes("tool"));
    } finally {
      await f.close();
    }
  },
);

test(
  "one config selects the target and rejects implicit service paths",
  { timeout: 8000 },
  async () => {
    const f = await fixture();
    try {
      f.configuration.spec.daemon.state_dir = "auto";
      await writeFile(f.configFile, JSON.stringify(f.configuration));
      await assert.rejects(f.bridge(), /explicit_service_path/);
    } finally {
      await f.close();
    }
  },
);

test(
  "per-Query workspace/environment are copied and authentication stays native",
  { timeout: 15000 },
  async () => {
    const f = await fixture();
    try {
      await mkdir(f.root + "/other");
      const provider = f.root + "/context.py";
      await writeFile(
        provider,
        `import json,os,sys
from pathlib import Path
x=json.load(sys.stdin)
r={'api_version':'aw-provider/v1alpha1','request_id':x['request_id'],'status':'ok'}
if x['method']=='describe':r['operations']=[{'name':'check','events':['tool.before'],'effects':['observe','block']}]
elif x['method']=='invoke':
 r['input_digest']=x['input_digest'];r['effects']=[]
 Path(${JSON.stringify(
   f.root + "/context.json",
 )}).write_text(json.dumps({'mark':os.getenv('BRIDGE_TEST_MARK'),'auth':os.getenv('QODER_PERSONAL_ACCESS_TOKEN'),'cwd':os.getcwd()}))
print(json.dumps(r))
`,
      );
      f.configuration.spec.providers.policy.transport.argv = [
        "/usr/bin/python3",
        provider,
      ];
      await writeFile(f.configFile, JSON.stringify(f.configuration));
      const bridge = await f.bridge();
      await f.attach();
      const env = {
        BRIDGE_TEST_MARK: "original",
        QODER_PERSONAL_ACCESS_TOKEN: "synthetic-native-token",
      };
      const session = bridge.query({
        prompt: "tool",
        options: { cwd: f.root + "/other", env },
      });
      env.BRIDGE_TEST_MARK = "changed-after-query";
      for await (const _ of session) {
      }
      const { readFile } = await import("node:fs/promises");
      const recorded = JSON.parse(
        await readFile(f.root + "/context.json", "utf8"),
      );
      assert.deepEqual(recorded, {
        mark: "original",
        auth: null,
        cwd: f.root + "/other",
      });
    } finally {
      await f.close();
    }
  },
);

for (const method of ["return", "throw"]) {
  test(
    `pre-start iterator ${method} closes its Query once`,
    { timeout: 5000 },
    async () => {
      const f = await fixture();
      let closes = 0;
      try {
        const bridge = await f.bridge({
          query: () => ({
            async close() {
              closes++;
            },
            async interrupt() {},
            async *[Symbol.asyncIterator]() {
              throw new Error("must not start");
            },
          }),
        });
        const iterator = bridge
          .query({ prompt: "text" })
          [Symbol.asyncIterator]();
        if (method === "throw")
          await assert.rejects(
            iterator.throw(new Error("cancelled")),
            /cancelled/,
          );
        else
          assert.deepEqual(await iterator.return("cancelled"), {
            done: true,
            value: "cancelled",
          });
        assert.equal(closes, 1);
        assert.deepEqual(f.calls, []);
        await bridge.close();
        assert.equal(closes, 1);
      } finally {
        await f.close();
      }
    },
  );
}

test("default fallback rejects required policy; required mode is explicit", async () => {
  const f = await fixture();
  try {
    await assert.rejects(
      f.bridge({ availability: undefined }),
      /sdk_required_policy_conflict/,
    );
    const bridge = await f.bridge({ availability: "required" });
    assert.equal(bridge.state, "detached");
  } finally {
    await f.close();
  }
});

test("explicit required availability also waits with optional policy", async () => {
  const f = await fixture(false);
  try {
    const bridge = await f.bridge({ availability: "required" });
    await assert.rejects(async () => {
      for await (const _ of bridge.query({ prompt: "tool" })) {
      }
    });
    assert.deepEqual(f.calls, ["model", "denied"]);
  } finally {
    await f.close();
  }
});

test(
  "default fallback keeps active and new Queries native after AW loss",
  { timeout: 10000 },
  async () => {
    const f = await fixture(false);
    try {
      f.configuration.spec.events["tool.after"] = {
        enabled: true,
        required: false,
        steps: [],
      };
      await writeFile(f.configFile, JSON.stringify(f.configuration));
      const bridge = await f.bridge({ availability: undefined });
      await f.attach();
      let beforeCount = 0,
        afterCount = 0;
      const hooks = {
        PreToolUse: [
          {
            hooks: [
              async () => {
                beforeCount++;
                return {};
              },
            ],
          },
        ],
        PostToolUse: [
          {
            hooks: [
              async () => {
                afterCount++;
                await f.stopService();
                return {};
              },
            ],
          },
        ],
      };
      for (let index = 0; index < 2; index++) {
        const session = bridge.query({ prompt: "tool", options: { hooks } });
        const messages = [];
        for await (const message of session) messages.push(message.type);
        assert.deepEqual(messages, ["model", "result"]);
        assert(session.closed);
      }
      assert.equal(beforeCount, 2);
      assert.equal(afterCount, 2);
      assert.equal(bridge.state, "unavailable");
      assert.deepEqual(f.calls, ["model", "tool", "model", "tool"]);
    } finally {
      await f.close();
    }
  },
);

for (const failure of ["denial", "provider error"]) {
  test(`default fallback preserves ${failure} while AW is reachable`, async () => {
    const f = await fixture(false);
    try {
      if (failure === "denial") await writeFile(f.root + "/deny", "deny");
      else {
        // The fixture describes successfully and fails in invoke when Path receives null.
        f.configuration.spec.providers.policy.config.deny_file = null;
        await writeFile(f.configFile, JSON.stringify(f.configuration));
      }
      const bridge = await f.bridge({ availability: undefined });
      await f.attach();
      const consume = async () => {
        for await (const _ of bridge.query({ prompt: "tool" })) {
        }
      };
      if (failure === "provider error")
        await assert.rejects(consume, /sdk_step_failed/);
      else await consume();
      assert.deepEqual(f.calls, ["model", "denied"]);
      assert.equal(bridge.state, "ready");
    } finally {
      await f.close();
    }
  });
}

test(
  "bridge close waits for every native Query after one close fails",
  { timeout: 10000 },
  async () => {
    const f = await fixture();
    const tasks = [],
      options = [],
      nativeCloses = [];
    const open = Task.prototype.open;
    let drain;
    const draining = new Promise((resolve) => {
      drain = resolve;
    });
    Task.prototype.open = async function () {
      await open.call(this);
      tasks.push(this);
    };
    let closing;
    try {
      const bridge = await f.bridge({
        query(input) {
          options.push(input.options);
          return {
            async close() {
              nativeCloses.push(input.prompt);
              if (input.prompt === "fail")
                throw new Error("native close failed");
              await draining;
            },
            async interrupt() {},
            async *[Symbol.asyncIterator]() {},
          };
        },
      });
      await f.attach();
      bridge.query({ prompt: "fail" });
      bridge.query({ prompt: "drain" });
      for (const option of options)
        await option.hooks.PreToolUse[0].hooks[0](
          { ...before, cwd: f.root + "/work" },
          "call",
        );
      assert.equal(tasks.length, 2);
      let finished = false;
      closing = bridge.close().then(
        () => {
          finished = true;
          return null;
        },
        (error) => {
          finished = true;
          return error;
        },
      );
      for (let i = 0; i < 200 && !tasks[0].closed; i++) await delay(10);
      assert.equal(tasks[0].closed, true);
      assert.deepEqual(nativeCloses, ["fail", "drain"]);
      assert.equal(finished, false);
      assert.equal(tasks[1].closed, false);
      const policy = await tasks[1].call(
        {
          method: "scope_policy",
          instance_id: tasks[1].binding.instance_id,
        },
        null,
      );
      assert(policy.hooks.length > 0);
      assert.equal(policy.policy_revision, tasks[1].routes.policy_revision);
      drain();
      assert.match((await closing).message, /native close failed/);
      assert(tasks.every((task) => task.closed));
      assert.equal(bridge.state, "closed");
    } finally {
      drain();
      await closing;
      Task.prototype.open = open;
      await f
        .close()
        .catch((error) => assert.match(error.message, /native close failed/));
    }
  },
);
