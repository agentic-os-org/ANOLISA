import { mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";

export function isInMemoryPath(dbPath: string): boolean {
  const normalized = dbPath.trim();
  return normalized === ":memory:" || normalized.startsWith("file::memory:");
}

function ensureDbDirectory(dbPath: string): void {
  if (isInMemoryPath(dbPath)) return;
  mkdirSync(dirname(resolve(dbPath)), { recursive: true });
}

function configureConnection(db: DatabaseSync): void {
  db.exec("PRAGMA journal_mode = WAL");
  db.exec("PRAGMA busy_timeout = 10000");
  db.exec("PRAGMA foreign_keys = ON");
  db.exec("PRAGMA synchronous = NORMAL");
  db.exec("PRAGMA temp_store = MEMORY");
}

export function createConnection(dbPath: string): DatabaseSync {
  ensureDbDirectory(dbPath);
  const db = new DatabaseSync(dbPath);
  configureConnection(db);
  return db;
}

export function closeConnection(db: DatabaseSync): void {
  try {
    db.close();
  } catch {
    // ignore close failures
  }
}
