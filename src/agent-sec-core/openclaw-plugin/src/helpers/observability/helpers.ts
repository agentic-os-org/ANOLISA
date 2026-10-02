import type { UnknownRecord } from "./types.js";

export function compactRecord(record: UnknownRecord): UnknownRecord {
  const compacted: UnknownRecord = {};
  for (const [key, value] of Object.entries(record)) {
    if (value !== undefined) {
      compacted[key] = value;
    }
  }
  return compacted;
}

export function asRecord(value: unknown): UnknownRecord | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return undefined;
  }
  return value as UnknownRecord;
}

export function rawString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

export function firstString(...values: unknown[]): string | undefined {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) {
      return value.trim();
    }
  }
  return undefined;
}

export function getNumber(record: UnknownRecord | undefined, key: string): number | undefined {
  const value = record?.[key];
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

export function getBoolean(record: UnknownRecord | undefined, key: string): boolean | undefined {
  const value = record?.[key];
  return typeof value === "boolean" ? value : undefined;
}

export function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

export function countHistoryMessages(value: unknown): number | undefined {
  const messages = getArray(value);
  return messages === undefined ? undefined : messages.length;
}

export function getArray(value: unknown): unknown[] | undefined {
  return Array.isArray(value) ? value : undefined;
}

export function jsonByteLength(value: unknown): number | undefined {
  if (value === undefined) {
    return undefined;
  }
  try {
    return Buffer.byteLength(JSON.stringify(value), "utf8");
  } catch {
    return undefined;
  }
}

export function formatSafeError(error: unknown): string {
  if (error instanceof Error) {
    return error.name || "Error";
  }
  return typeof error;
}
