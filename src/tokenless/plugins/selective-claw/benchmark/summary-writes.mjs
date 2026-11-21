// Run with: node benchmark/summary-writes.mjs (after npm install).
import { buildSync } from "esbuild";
import { DatabaseSync } from "node:sqlite";
import { performance } from "node:perf_hooks";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const pluginRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const build = buildSync({
  stdin: {
    contents:
      'export { runMigrations } from "./src/db/migration.ts"; export { MessageStore } from "./src/store/message-store.ts";',
    resolveDir: pluginRoot,
  },
  bundle: true,
  platform: "node",
  format: "esm",
  write: false,
});
const encoded = Buffer.from(build.outputFiles[0].text).toString("base64");
const { runMigrations, MessageStore } = await import(
  `data:text/javascript;base64,${encoded}`
);
const legacyTrigger = `
  DROP TRIGGER messages_au;
  CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.message_id, old.content);
    INSERT INTO messages_fts(rowid, content) VALUES (new.message_id, new.content);
  END;
`;
const totalChanges = (db) => db.prepare("SELECT total_changes() AS n").get().n;
const median = (values) =>
  [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];
const samples = [];

for (const bytes of [128, 16 * 1024, 1024 * 1024]) {
  const content =
    "deployment build artifact: synthetic payload retained for search\n"
      .repeat(Math.ceil(bytes / 63))
      .slice(0, bytes);
  const variants = [true, false].map((legacy) => {
    const db = new DatabaseSync(":memory:");
    runMigrations(db);
    if (legacy) db.exec(legacyTrigger);
    const store = new MessageStore(db);
    for (let seq = 1; seq <= 10; seq++) {
      store.createMessage({
        sessionId: "session",
        seq,
        turnSeq: seq,
        role: "user",
        content,
        tokenCount: bytes / 4,
      });
    }
    return { db, store, times: [], changes: [] };
  });
  const rounds = 7;
  const updates = 40;
  try {
    // One warm-up, then alternate order so one implementation is not always first.
    for (let round = -1; round < rounds; round++) {
      for (const index of round % 2 === 0 ? [0, 1] : [1, 0]) {
        const variant = variants[index];
        const before = totalChanges(variant.db);
        const start = performance.now();
        for (let i = 0; i < updates; i++) {
          variant.store.setTurnSummary(
            "session",
            (i % 10) + 1,
            `round ${round} summary ${i}`,
          );
        }
        const elapsed = performance.now() - start;
        if (round >= 0) {
          variant.times.push(elapsed / updates);
          variant.changes.push((totalChanges(variant.db) - before) / updates);
        }
        if (
          variant.store
            .getMessages("session")
            .some((row) => row.content !== content)
        ) {
          throw new Error("Original message content changed");
        }
        if (
          variant.store.searchMessages("session", "deployment", 20).length !==
          10
        ) {
          throw new Error("Search results changed");
        }
        variant.db.exec(
          "INSERT INTO messages_fts(messages_fts, rank) VALUES('integrity-check', 1)",
        );
      }
    }
    const row = {
      bytes,
      rows: 10,
      rounds,
      updatesPerRound: updates,
      baselineMedianMs: median(variants[0].times),
      fixedMedianMs: median(variants[1].times),
      baselineChangesPerUpdate: median(variants[0].changes),
      fixedChangesPerUpdate: median(variants[1].changes),
    };
    samples.push(row);
  } finally {
    for (const { db } of variants) db.close();
  }
}
console.log(
  JSON.stringify(
    {
      node: process.version,
      platform: process.platform,
      sqlite: process.versions.sqlite,
      scope:
        "In-memory synthetic summary updates only; excludes model calls, initial indexing and whole-session latency",
      samples,
    },
    null,
    2,
  ),
);
