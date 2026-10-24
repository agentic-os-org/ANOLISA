/** Acknowledged task handles remain private to one managed session. */
import { readFileSync, realpathSync } from "node:fs";
import { run } from "./transport.mjs";

export class Scope {
  constructor() {
    this.config = JSON.parse(readFileSync(process.env.AW_NATIVE_CONFIG, "utf8"));
    this.pid = process.pid;
    this.handle = null;
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
    const args = ["scope", "--adapter", "pi", "--binding", this.config.binding, "--action", action];
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

  async open(cwd, environment) {
    if (this.handle || this.closed || this.pid !== process.pid)
      throw new Error("AW task cannot be reopened");
    this.environment = { ...environment };
    const result = await this.command("open", { pid: this.pid, cwd: realpathSync(cwd), environment }, 33000);
    if (result.code !== 0) throw new Error("AW task admission failed");
    this.handle = JSON.parse(result.stdout);
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
