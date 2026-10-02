import { describe, it, expect } from "vitest";
import { SnapshotStore } from "../snapshot-store.js";
import type { SnapshotInfo } from "../types.js";

function snap(id: string, date: string): SnapshotInfo {
  return { snapshot: id, createdAt: date, message: `msg-${id}` };
}

describe("SnapshotStore", () => {
  it("starts empty", () => {
    const store = new SnapshotStore();
    expect(store.count).toBe(0);
    expect(store.getAll()).toEqual([]);
  });

  it("add increases count", () => {
    const store = new SnapshotStore();
    store.add(snap("a", "2024-01-01T00:00:00Z"));
    expect(store.count).toBe(1);
  });

  it("add replaces duplicate", () => {
    const store = new SnapshotStore();
    store.add(snap("a", "2024-01-01T00:00:00Z"));
    store.add({ snapshot: "a", createdAt: "2024-01-02T00:00:00Z", message: "updated" });
    expect(store.count).toBe(1);
    expect(store.getAll()[0].message).toBe("updated");
  });

  it("setAll replaces everything", () => {
    const store = new SnapshotStore();
    store.add(snap("old", "2024-01-01T00:00:00Z"));
    store.setAll([snap("x", "2024-06-01T00:00:00Z"), snap("y", "2024-06-02T00:00:00Z")]);
    expect(store.count).toBe(2);
  });

  it("getAll returns newest first", () => {
    const store = new SnapshotStore();
    store.add(snap("old", "2024-01-01T00:00:00Z"));
    store.add(snap("new", "2024-12-31T00:00:00Z"));
    store.add(snap("mid", "2024-06-01T00:00:00Z"));
    const all = store.getAll();
    expect(all[0].snapshot).toBe("new");
    expect(all[1].snapshot).toBe("mid");
    expect(all[2].snapshot).toBe("old");
  });

  it("getAll returns a copy", () => {
    const store = new SnapshotStore();
    store.add(snap("a", "2024-01-01T00:00:00Z"));
    const a = store.getAll();
    const b = store.getAll();
    expect(a).not.toBe(b);
  });

  it("remove returns true and removes", () => {
    const store = new SnapshotStore();
    store.add(snap("a", "2024-01-01T00:00:00Z"));
    expect(store.remove("a")).toBe(true);
    expect(store.count).toBe(0);
  });

  it("remove returns false for missing", () => {
    const store = new SnapshotStore();
    expect(store.remove("nonexistent")).toBe(false);
  });

  it("setAll makes a defensive copy", () => {
    const store = new SnapshotStore();
    const input = [snap("a", "2024-01-01T00:00:00Z")];
    store.setAll(input);
    input.push(snap("b", "2024-02-01T00:00:00Z"));
    expect(store.count).toBe(1);
  });
});
