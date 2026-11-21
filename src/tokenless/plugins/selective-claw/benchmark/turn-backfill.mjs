// After npm install: node --experimental-sqlite benchmark/turn-backfill.mjs
// Options: --rows=1000,2000,4000 --rounds=3
// Opt-in: legacy 8,000-row migrations can take a minute each. No timing CI gate.
import assert from "node:assert/strict";
import { buildSync } from "esbuild";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { performance } from "node:perf_hooks";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const build = buildSync({
  stdin: {
    contents:
      'export { runMigrations } from "./src/db/migration.ts"; export { createConnection, closeConnection } from "./src/db/connection.ts";',
    resolveDir: root,
  },
  bundle: true,
  platform: "node",
  format: "esm",
  write: false,
});
const { runMigrations, createConnection, closeConnection } = await import(
  `data:text/javascript;base64,${Buffer.from(build.outputFiles[0].text).toString("base64")}`
);
const options = Object.fromEntries(
  process.argv.slice(2).map((arg) => {
    const match = /^--(rows|rounds)=(.+)$/.exec(arg);
    if (!match) throw new Error(`Unknown argument: ${arg}`);
    return [match[1], match[2]];
  }),
);
const sizes = (options.rows ?? "1000,2000,4000").split(",").map(Number);
const rounds = Number(options.rounds ?? 3);
if (![...sizes, rounds].every((n) => Number.isSafeInteger(n) && n > 0)) {
  throw new Error("rows and rounds must be positive integers");
}
const currentUpdate =
  /UPDATE messages SET turn_seq = turn_ids\.turn_seq\s+FROM turn_ids\s+WHERE turn_ids\.message_id = messages\.message_id/;
const legacyUpdate = `UPDATE messages SET turn_seq = (
  SELECT turn_ids.turn_seq FROM turn_ids
  WHERE turn_ids.message_id = messages.message_id
)`;
const directory = mkdtempSync(join(tmpdir(), "selective-backfill-benchmark-"));
const hash = (rows) =>
  createHash("sha256").update(JSON.stringify(rows)).digest("hex");
const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2
    ? sorted[middle]
    : (sorted[middle - 1] + sorted[middle]) / 2;
};
const samples = [];

function measure(n, variant, round) {
  const db = createConnection(join(directory, `${n}-${variant}-${round}.db`));
  try {
    runMigrations(db);
    db.exec("ALTER TABLE messages DROP COLUMN turn_seq");
    const insert = db.prepare(`INSERT INTO messages
      (session_id,seq,role,content,token_count,raw_message,created_at)
      VALUES(?,?,?,?,?,?,?)`);
    const roles = [
      "user",
      "assistant",
      "tool",
      "assistant",
      "system",
      "assistant",
    ];
    const starts = ["user", "system", "assistant", "tool"];
    db.exec("BEGIN");
    for (let i = n - 1; i >= 0; i--) {
      const session = i % 8,
        position = Math.floor(i / 8);
      const role =
        position === 0
          ? starts[session % 4]
          : roles[(position - 1) % roles.length];
      insert.run(
        `session-${session}`,
        position * 2 + 1,
        role,
        `migrationmarker message ${i}: inspect the build and retain tool results.\n` +
          `Path src/session-${session}/worker.ts. The tool returned status and diagnostic context.\n`.repeat(
            4,
          ),
        143,
        JSON.stringify({ id: i }),
        "2026-01-01T00:00:00Z",
      );
    }
    db.exec("COMMIT; PRAGMA wal_checkpoint(TRUNCATE)");
    const fields =
      "message_id,session_id,seq,role,content,token_count,raw_message,created_at,summary";
    const before = hash(
      db.prepare(`SELECT ${fields} FROM messages ORDER BY message_id`).all(),
    );
    const exec = db.exec.bind(db);
    let backfillSql,
      calls = 0;
    // Change only the UPDATE shape; both variants run the real migration entrypoint.
    db.exec = (sql) => {
      if (sql.includes("WITH turn_ids AS")) {
        assert.match(sql, currentUpdate);
        backfillSql =
          variant === "legacy" ? sql.replace(currentUpdate, legacyUpdate) : sql;
        calls++;
        return exec(backfillSql);
      }
      return exec(sql);
    };
    const started = performance.now();
    runMigrations(db);
    const elapsedMs = performance.now() - started;
    db.exec = exec;
    assert.equal(calls, 1);
    assert.equal(
      hash(
        db.prepare(`SELECT ${fields} FROM messages ORDER BY message_id`).all(),
      ),
      before,
    );
    const migrated = db
      .prepare("SELECT * FROM messages ORDER BY session_id,seq")
      .all();
    let session, turn;
    for (const row of migrated) {
      const boundary = row.role === "user" || row.role === "system";
      if (row.session_id !== session) {
        session = row.session_id;
        turn = boundary ? 0 : 1;
      }
      if (boundary) turn++;
      assert.equal(row.turn_seq, turn);
    }
    assert.equal(migrated.length, n);
    assert.equal(
      db
        .prepare(
          "SELECT count(*) AS n FROM messages_fts WHERE messages_fts MATCH 'migrationmarker'",
        )
        .get().n,
      n,
    );
    db.exec(
      "INSERT INTO messages_fts(messages_fts,rank) VALUES('integrity-check',1)",
    );
    const resultHash = hash(migrated);
    const warmStarted = performance.now();
    runMigrations(db);
    const alreadyMigratedMs = performance.now() - warmStarted;
    assert.equal(
      hash(db.prepare("SELECT * FROM messages ORDER BY session_id,seq").all()),
      resultHash,
    );
    const plan = db
      .prepare(`EXPLAIN QUERY PLAN ${backfillSql}`)
      .all()
      .map((row) => row.detail);
    return {
      n,
      variant,
      round,
      elapsedMs,
      alreadyMigratedMs,
      resultHash,
      plan,
    };
  } finally {
    closeConnection(db);
  }
}

try {
  for (const n of sizes) {
    for (let round = 0; round < rounds; round++) {
      for (const variant of round % 2
        ? ["current", "legacy"]
        : ["legacy", "current"]) {
        const sample = measure(n, variant, round);
        samples.push(sample);
        console.error(
          `${n} rows, ${variant}, round ${round + 1}: ${sample.elapsedMs.toFixed(2)} ms`,
        );
      }
    }
    assert.equal(
      new Set(samples.filter((s) => s.n === n).map((s) => s.resultHash)).size,
      1,
    );
  }
  const summary = sizes.map((n) => ({
    rows: n,
    legacyMedianMs: median(
      samples
        .filter((s) => s.n === n && s.variant === "legacy")
        .map((s) => s.elapsedMs),
    ),
    currentMedianMs: median(
      samples
        .filter((s) => s.n === n && s.variant === "current")
        .map((s) => s.elapsedMs),
    ),
  }));
  console.log(
    JSON.stringify(
      {
        node: process.version,
        sqlite: process.versions.sqlite,
        platform: process.platform,
        rounds,
        scope:
          "File-backed synthetic legacy migrations with populated FTS and production connection pragmas; complete runMigrations only, excludes setup, connection creation and correctness assertions. Fresh database per sample, alternating variant order, no OS cache flush; not whole-session latency.",
        summary,
        samples,
      },
      null,
      2,
    ),
  );
} finally {
  rmSync(directory, { recursive: true, force: true });
}
