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
