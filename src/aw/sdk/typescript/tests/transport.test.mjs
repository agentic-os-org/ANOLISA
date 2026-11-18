import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import { mkdtemp, chmod, rm, readFile } from "node:fs/promises";
import { once } from "node:events";
import { Instance } from "../src/client.mjs";
import { rpc } from "../src/transport.mjs";

const contract = JSON.parse(
  await readFile(
    new URL("../../tests/fixtures/contract.json", import.meta.url),
  ),
);
const identity = {
  generation: "test-generation",
  config_revision: "test-revision",
};
async function server(handler) {
  const directory = await mkdtemp("/tmp/aw-sdk-wire-");
  const endpoint = `${directory}/aw.sock`;
  const sockets = new Set();
  const listener = net.createServer((socket) => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    socket.on("error", () => {});
    let data = Buffer.alloc(0);
    socket.on("data", (chunk) => {
      data = Buffer.concat([data, chunk]);
      if (data.length < 4 || data.length < data.readUInt32BE(0) + 4) return;
      const request = JSON.parse(data.subarray(4));
      const reply = (result, error = null) => {
        const body = Buffer.from(
          JSON.stringify({
            api_version: contract.service_protocol,
            identity,
            result,
            error,
          }),
        );
        const size = Buffer.alloc(4);
        size.writeUInt32BE(body.length);
        if (!socket.destroyed) socket.end(Buffer.concat([size, body]));
      };
      handler(request.operation, reply, socket, request);
    });
  });
  listener.listen(endpoint);
  await once(listener, "listening");
  await chmod(endpoint, 0o600);
  return {
    endpoint,
    async close() {
      for (const socket of sockets) socket.destroy();
      listener.close();
      await once(listener, "close");
      await rm(directory, { recursive: true });
    },
  };
}

test("missing identity is encoded as null in a valid request frame", async () => {
  const identities = [];
  const peer = await server((_operation, reply, _socket, request) => {
    identities.push(request.identity);
    reply({ available: true });
  });
  try {
    for (const supplied of [undefined, null, identity]) {
      const response = await rpc(
        peer.endpoint,
        supplied,
        { method: "status" },
        500,
      );
      assert.deepEqual(response.result, { available: true });
    }
    assert.deepEqual(identities, [null, null, identity]);
  } finally {
    await peer.close();
  }
});

test("remote policy and Provider failures cannot impersonate transport loss", async () => {
  for (const code of [...contract.never_fallback, "ECONNRESET"]) {
    const peer = await server((_operation, reply) => reply(null, code));
    try {
      await assert.rejects(
        rpc(peer.endpoint, identity, { method: "status" }, 500),
        (error) => error.code === code && !error.unavailable,
      );
    } finally {
      await peer.close();
    }
  }
});

test("lost registration response is recovered by query without a second registration", async () => {
  const operations = [];
  const handle = { binding: { instance_id: "test-instance" } };
  const peer = await server((operation, reply, socket) => {
    if (operation.method === "status")
      return reply({ sdk_protocol: contract.sdk_protocol });
    const action = operation.command.action;
    operations.push(action);
    if (action === "register_sdk") return socket.destroy();
    if (action === "query")
      return reply({
        state: "pending_ready",
        handle,
        policy_revision: "r",
        routes: { events: {}, hooks: [] },
      });
    reply({ state: action === "ready" ? "ready" : "released" });
  });
  const instance = new Instance({
    endpoint: peer.endpoint,
    target: "app",
    capabilities: {},
    availability: "native_fallback",
    install: () => true,
  });
  try {
    assert.equal(await instance.connect(), false);
    assert.equal(await instance.connect(), true);
    assert.deepEqual(operations, ["register_sdk", "query", "ready"]);
    assert.throws(() => {
      instance.options.availability = "required";
    }, TypeError);
  } finally {
    await instance.close();
    await peer.close();
  }
});

test("timeout sends once and ignores late replies; cancellation remains cancellation", async () => {
  let requests = 0;
  const timers = [];
  const peer = await server((_operation, reply) => {
    requests++;
    timers.push(setTimeout(() => reply({ late: true }), 100));
  });
  try {
    await assert.rejects(
      rpc(peer.endpoint, identity, { method: "status" }, 20),
      (error) => error.unavailable,
    );
    const controller = new AbortController();
    const reason = new Error("user cancelled");
    const pending = rpc(
      peer.endpoint,
      identity,
      { method: "status" },
      500,
      controller.signal,
    );
    const timer = setTimeout(() => controller.abort(reason), 20);
    await assert.rejects(pending, (error) => error === reason);
    clearTimeout(timer);
    await new Promise((resolve) => setTimeout(resolve, 120));
    assert.equal(requests, 2);
  } finally {
    timers.forEach(clearTimeout);
    await peer.close();
  }
});

test("deferred Task release tolerates unacknowledged lease expiry and rejects post-ack loss", async () => {
  let nextId = 0;
  const peer = await server((operation, reply) => {
    if (operation.method === "status")
      return reply({ sdk_protocol: contract.sdk_protocol });
    if (operation.method === "registration") {
      if (operation.command.action === "register_sdk")
        return reply({
          state: "pending_ready",
          handle: { binding: { instance_id: "root" } },
          policy_revision: "r",
          routes: { events: {}, hooks: [] },
        });
      return reply({ state: "ready" });
    }
    if (operation.method === "open_scope")
      return reply({ instance_id: `scope-${++nextId}` });
    if (operation.method === "scope_policy")
      return reply({ events: {}, hooks: [] });
    if (operation.method === "acknowledge_scope") return reply({});
    if (operation.method === "release_instance") {
      if (operation.instance_id === "root") return reply({});
      return reply(null, "unknown_instance");
    }
    reply({});
  });
  const instance = new Instance({
    endpoint: peer.endpoint,
    target: "app",
    capabilities: {},
    availability: "required",
    install: () => true,
  });
  try {
    assert.equal(await instance.connect(), true);
    const unacknowledged = await instance.task({ deferAcknowledgement: true });
    await unacknowledged.close();

    const acknowledged = await instance.task({ deferAcknowledgement: true });
    await acknowledged.acknowledge();
    await assert.rejects(
      acknowledged.close(),
      (error) => error.code === "unknown_instance",
    );
    instance.tasks.delete(acknowledged);
  } finally {
    instance.tasks.clear();
    await instance.close().catch(() => {});
    await peer.close();
  }
});
