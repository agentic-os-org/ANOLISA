import { LanguageSource } from "../src/sources.mjs";

export function fakeSource(handler, { events, availability = "required", fallback = false } = {}) {
  const source = LanguageSource.create({
    endpoint: "/tmp/nonexistent-aw.sock",
    target: "app",
    availability,
    ...(events ? { events } : {}),
  });
  source.instance.state = fallback ? "detached" : "ready";
  const tasks = [];
  source.instance.task = async () => {
    const task = {
      fallback,
      closed: false,
      routes: { events: {}, hooks: Object.keys(source.instance.options.capabilities.events).map((event) => ({ event })) },
      events: [],
      async emit(event, payload, signal) {
        signal?.throwIfAborted();
        this.events.push([event, payload]);
        return handler(event, payload, signal);
      },
      async close() {
        this.closed = true;
      },
    };
    tasks.push(task);
    return task;
  };
  return { source, tasks };
}
