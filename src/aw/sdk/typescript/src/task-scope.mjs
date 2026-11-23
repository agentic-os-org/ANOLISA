/**
 * Carry the shared resident Scope protocol over one admitted SDK Task.
 *
 * Managed resident bridges reach the Service through the `aw scope` command.
 * A cooperative worker keeps the same native invocation ledger, drain and
 * result adoption; the SDK Task owns admission, acknowledgement and release.
 */
import { Scope } from "../../../crates/aw-adapter/native/shared/scope.mjs";
import { Admission } from "./admission.mjs";
import { AwError } from "./transport.mjs";

const STDERR_LIMIT = 64 * 1024;

export class TaskScope extends Scope {
  constructor(instance, { timeoutMs = 5000 } = {}) {
    super({});
    this.instance = instance;
    this.timeoutMs = timeoutMs;
  }

  // The SDK Task owns registration and release; there is no CLI reservation.
  async reserve() {}
  async release() {}

  async open(cwd, environment) {
    this.admission = new Admission(this.instance, {
      events: Object.keys(this.instance.options.capabilities.events),
      timeoutMs: this.timeoutMs,
      cwd,
      environment,
      deferAcknowledgement: true,
    });
    await this.admission.prepare();
    this.task = this.admission.task;
    this.handle = this.task.binding;
    this.check();
  }

  check() {
    if (this.task?.connectionError) this.invalidate(this.task.connectionError);
    super.check();
  }

  async policy() {
    this.check();
    return this.task.routes;
  }

  async acknowledge() {
    await this.task.acknowledge();
    const { invocations_per_binding } = await this.call("enable_invocations");
    this.invocationLimit = invocations_per_binding;
    await this.watch(await this.call("watch_scope", { pid: this.pid }));
  }

  async emit(event, payload, signal) {
    this.check();
    await this.validate();
    return this.task.emit(event, payload, signal);
  }

  call(method, fields, signal, timeoutMs = 6000) {
    return this.task.call(
      { method, instance_id: this.handle.instance_id, ...fields },
      signal,
      timeoutMs,
    );
  }

  /** Map each shared Scope action to its SDK wire method. */
  async command(action, value, timeout, hook, signal) {
    switch (action) {
      case "check":
        await this.call(
          "check_scope",
          { pid: this.pid, policy_revision: value.policy_revision, valid: value.valid },
          signal,
          timeout,
        );
        break;
      case "enable-input":
        await this.call("enable_submissions", undefined, signal, timeout);
        break;
      case "register":
      case "finish": {
        const { sequence, call_id, tool_name, input } = value;
        await this.call(
          action === "register" ? "register_invocation" : "finish_invocation",
          { sequence, call_id, tool_name, input },
          action === "finish" ? null : signal,
          timeout,
        );
        break;
      }
      case "invoke": {
        const payload = JSON.parse(value.native_input);
        if (hook.close_event) {
          await this.task.closeEvent(hook, payload);
          break;
        }
        const response = await this.task.invokeStep(hook, payload, signal);
        if (response.stderr.length > STDERR_LIMIT)
          throw new AwError("callback_stderr_limit");
        if (response.stderr.length) process.stderr.write(response.stderr);
        return response;
      }
      default:
        throw new AwError("unsupported_scope_operation");
    }
    return { code: 0, stdout: Buffer.alloc(0) };
  }

  async closeOwned() {
    this.closed = true;
    clearTimeout(this.monitorTimer);
    this.monitorAbort?.abort();
    await this.monitor;
    await this.admission?.close();
  }
}
