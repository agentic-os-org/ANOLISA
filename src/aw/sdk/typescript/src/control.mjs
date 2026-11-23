/** Single application-owned control loop; intents never confer CLI ownership. */
import { randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { AwError, rpc, sameIdentity } from "./transport.mjs";

const VERSION = "aw-sdk-control/v1";
export class Controller {
  constructor(instance, automatic) {
    this.instance = instance;
    this.enabled = automatic;
    this.stopped = false;
    this.running = this.loop();
  }
  stop() {
    this.stopping ??= this.finish().finally(() => {
      this.finished = true;
    });
    return this.stopping;
  }
  async finish() {
    this.stopped = true;
    clearTimeout(this.timer);
    this.wake?.();
    await this.running;
    if (this.identity && this.instance.controlKey) {
      try {
        await this.call({ action: "withdraw", key: this.instance.controlKey });
      } catch (error) {
        if (!error.unavailable && error.code !== "stale_service") throw error;
      }
    }
  }
  async loop() {
    while (!this.stopped) {
      try {
        await this.cycle();
      } catch (error) {
        if (error.unavailable && this.instance.connection)
          this.instance.lost(this.instance.connection, error);
        if (!error.unavailable && error.code !== "stale_service") {
          this.instance.lastError = error;
          this.enabled = false;
          if (error.code === "sdk_control_unsupported") this.stopped = true;
        }
      }
      if (!this.stopped)
        await new Promise((resolve) => {
          this.wake = resolve;
          this.timer = setTimeout(
            resolve,
            Math.min(this.instance.options.reconnectMs, 1000),
          );
          this.timer.unref();
        });
    }
  }
  async call(command) {
    const o = this.instance.options;
    return (
      await rpc(
        o.endpoint,
        this.identity,
        { method: "sdk_control", command },
        o.timeoutMs,
      )
    ).result;
  }
  async cycle() {
    const instance = this.instance,
      o = instance.options;
    const discovery = await rpc(
      o.endpoint,
      null,
      { method: "status" },
      o.timeoutMs,
    );
    if (discovery.result.sdk_control_protocol !== VERSION)
      throw new AwError("sdk_control_unsupported");
    if (!sameIdentity(this.identity, discovery.identity)) {
      if (
        instance.connection &&
        !sameIdentity(instance.connection.identity, discovery.identity)
      ) {
        instance.lost(instance.connection);
      }
      this.active = undefined;
      this.identity = discovery.identity;
    }
    if (!instance.controlKey) {
      const stat = await readFile("/proc/self/stat", "utf8");
      instance.controlKey = {
        request_id: randomUUID(),
        owner: {
          pid: process.pid,
          start_ticks: stat
            .slice(stat.lastIndexOf(")") + 1)
            .trim()
            .split(/\s+/)[19],
        },
      };
    }
    const snapshot = await this.call({
      action: "poll",
      key: instance.controlKey,
      target: o.target,
      availability: o.availability,
      handle: instance.connection?.handle ?? null,
    });
    instance.applicationId = snapshot.application_id;
    const request = snapshot.request;
    if (this.stopped) return;
    if (this.active && this.active.request_id !== request?.request_id) {
      if (!this.active.wasReady) await instance.detachControl();
      this.enabled = this.active.wasEnabled;
      this.active = undefined;
    }
    if (
      request?.state === "pending" ||
      (this.active?.request_id === request?.request_id &&
        request?.state === "completed")
    ) {
      this.active ??= {
        ...request,
        wasReady: instance.state === "ready",
        wasEnabled: this.enabled,
      };
      await this.apply(this.active);
    } else if (this.active) {
      if (!this.active.wasReady) await instance.detachControl();
      this.enabled = this.active.wasEnabled;
      this.active = undefined;
    } else if (this.enabled) {
      await instance.connect();
    }
  }
  async acknowledge(request, handle = null, error = null) {
    return this.call({
      action: "acknowledge",
      key: this.instance.controlKey,
      request_id: request.request_id,
      handle,
      error,
    });
  }
  async apply(request) {
    const instance = this.instance;
    try {
      if (request.intent === "attach") {
        instance.confirmControl = (connection) =>
          this.acknowledge(request, connection.handle);
        if (instance.state === "ready")
          await this.acknowledge(request, instance.connection.handle);
        else if (!(await instance.connect())) return;
        this.enabled = true;
      } else {
        if ([...instance.tasks].some((task) => !task.closed && !task.fallback))
          throw new AwError("sdk_control_busy");
        await instance.detachControl();
        await this.acknowledge(request);
        this.enabled = false;
      }
      this.active = undefined;
    } catch (error) {
      if (error.unavailable || error.code === "stale_service") return;
      if (!request.wasReady) await instance.detachControl();
      if (!error.code?.startsWith("sdk_control_")) instance.lastError = error;
      this.enabled = request.wasEnabled;
      try {
        await this.acknowledge(
          request,
          null,
          /^[A-Za-z0-9_.:-]{1,128}$/.test(error.code ?? "")
            ? error.code
            : "sdk_control_failed",
        );
      } catch (failure) {
        if (failure.unavailable) throw failure;
        if (
          ![
            "sdk_control_request_expired",
            "sdk_control_request_unknown",
          ].includes(failure.code)
        )
          throw failure;
      }
      this.active = undefined;
    } finally {
      instance.confirmControl = undefined;
    }
  }
}
