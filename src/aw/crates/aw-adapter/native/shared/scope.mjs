/** Acknowledged task handles remain private to one managed session. */
import { readFileSync, realpathSync } from "node:fs";
import { run } from "./transport.mjs";
import { randomUUID } from "node:crypto";

export class Scope {
  constructor(config, adapter = "pi") {
    this.config = config ?? JSON.parse(readFileSync(process.env.AW_NATIVE_CONFIG, "utf8"));
    this.adapter = adapter;
    this.pid = process.pid;
    this.handle = null;
    this.reservation = null;
    this.closed = false;
    this.failed = false;
    this.environment = undefined;
    this.releaseError = null;
    this.sequence = 0;
    this.invocations = new Map();
    this.issuing = Promise.resolve();
    this.invocationLimit = 0;
    this.execution = 0;
    this.running = false;
    this.closing = false;
  }

  async command(action, value, timeout, hook, signal) {
    const args = ["scope", "--adapter", this.adapter, "--binding", this.config.binding, "--action", action];
    if (hook) args.push("--event", hook.event, "--step", hook.step, "--on-error", hook.on_error);
    return await run(this.config.executable, args, Buffer.from(JSON.stringify(value)), timeout, signal, this.environment);
  }

  check() {
    if (!this.handle || this.closed || this.failed || this.pid !== process.pid)
      throw new Error("AW task is closed, unbound or inherited across fork");
  }

  checkExecution(execution = this.execution) {
    this.check();
    if (!this.running || this.closing || execution !== this.execution)
      throw new Error("AW execution is unavailable");
  }

  begin() {
    this.check();
    if (this.running || this.closing || this.invocations.size)
      throw new Error("AW previous execution has not drained");
    this.execution++;
    this.running = true;
  }

  async ready() {
    if (this.reservation || this.handle || this.closed || this.pid !== process.pid)
      throw new Error("AW worker readiness requires an unused scope");
    const reply = await this.command("ready", { pid: this.pid }, 6000);
    if (reply.code !== 0) throw new Error("AW worker readiness failed");
  }

  async reserve() {
    if (this.reservation || this.handle || this.closed || this.pid !== process.pid)
      throw new Error("AW task cannot reserve admission");
    // Retain the identity before transport: a lost response may already be busy.
    this.reservation = { pid: this.pid, key: randomUUID() };
    const reply = await this.command("reserve", this.reservation, 6000);
    if (reply.code !== 0) throw new Error("AW task reservation failed");
  }

  async release() {
    if (!this.reservation) return;
    if (!this.closed || this.releaseError || this.pid !== process.pid)
      throw new Error("AW task cleanup is not confirmed");
    const reply = await this.command("release", this.reservation, 6000);
    if (reply.code !== 0) throw new Error("AW task reservation release failed");
    this.reservation = null;
  }

  async open(cwd, environment, { deferAcknowledgement = false } = {}) {
    if (this.handle || this.closed || this.pid !== process.pid)
      throw new Error("AW task cannot be reopened");
    this.environment = { ...environment };
    const result = await this.command("open", { pid: this.pid, cwd: realpathSync(cwd), environment }, 33000);
    if (result.code !== 0) throw new Error("AW task admission failed");
    this.handle = JSON.parse(result.stdout);
    if (!deferAcknowledgement) await this.acknowledge();
  }

  async policy() {
    this.check();
    const reply = await this.command("policy", this.handle, 6000);
    if (reply.code !== 0) throw new Error("AW task policy handoff failed");
    return JSON.parse(reply.stdout);
  }

  async acknowledge() {
    this.check();
    try {
      const reply = await this.command("acknowledge", this.handle, 6000);
      if (reply.code !== 0) throw new Error("AW task handoff acknowledgement failed");
      const enabled = await this.command("enable", this.handle, 6000);
      if (enabled.code !== 0) throw new Error("AW invocation protocol admission failed");
      this.invocationLimit = JSON.parse(enabled.stdout).invocations_per_binding;
    } catch (error) {
      await this.close();
      throw error;
    }
  }

  async invoke(hook, payload, signal) {
    this.check();
    return await this.command("invoke", {
      scope: this.handle, native_input: JSON.stringify(payload),
    }, hook.budget_ms + 3000, hook, signal);
  }

  async register(callId, name, input) {
    const execution = this.execution;
    const registration = this.issuing.then(async () => {
      this.checkExecution(execution);
      if (this.invocations.size >= this.invocationLimit || this.sequence >= Number.MAX_SAFE_INTEGER)
        throw new Error("AW invocation admission capacity exceeded");
      const sequence = ++this.sequence;
      const reply = await this.command("register", {
        scope: this.handle, sequence, call_id: callId, tool_name: name, input,
      }, 6000);
      if (reply.code !== 0) throw new Error("AW invocation registration rejected");
      this.invocations.set(callId, sequence);
    });
    this.issuing = registration.then(() => undefined, () => undefined);
    return await registration;
  }

  async finish(callId) {
    try {
      const sequence = this.invocations.get(callId);
      if (sequence === undefined) throw new Error("AW invocation already retired");
      const reply = await this.command("finish", { scope: this.handle, sequence, call_id: callId }, 6000);
      if (reply.code !== 0) throw new Error("AW invocation drain failed");
      this.invocations.delete(callId);
    } catch (error) { this.failed = true; throw error; }
  }

  async drain() {
    this.closing = true;
    try {
      await this.issuing;
      for (const callId of this.invocations.keys()) await this.finish(callId);
    } catch (error) { this.failed = true; throw error; }
    finally { this.running = false; this.closing = false; }
  }

  async close() {
    if (this.closed) {
      if (this.releaseError) throw this.releaseError;
      return;
    }
    if (this.pid !== process.pid) throw new Error("Cannot close an inherited AW task");
    this.closed = true;
    if (this.handle) {
      try {
        const reply = await this.command("close", this.handle, 6000);
        if (reply.code !== 0) throw new Error("AW task release failed; worker restart required");
      } catch (error) { this.releaseError = error; throw error; }
      finally { this.environment = undefined; }
    }
  }
}
