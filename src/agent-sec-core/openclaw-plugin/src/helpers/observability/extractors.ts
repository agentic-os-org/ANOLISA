import type { UnknownRecord } from "./types.js";
import {
  asRecord,
  getArray,
  getNumber,
  rawString,
} from "./helpers.js";

export function deriveToolResultError(result: unknown, isError?: boolean): string | undefined {
  const resultRecord = asRecord(result);
  const details = asRecord(resultRecord?.details);
  const status = rawString(details?.status) ?? rawString(resultRecord?.status);
  const exitCode = getNumber(details, "exitCode") ?? getNumber(details, "exit_code") ?? getNumber(resultRecord, "exitCode");
  const hasErrorStatus =
    isError === true ||
    status === "error" ||
    status === "failed" ||
    (exitCode !== undefined && exitCode !== 0);
  if (!hasErrorStatus) {
    return undefined;
  }
  return (
    rawString(details?.error) ??
    rawString(resultRecord?.error) ??
    rawString(details?.aggregated) ??
    extractToolResultContentText(resultRecord)
  );
}

function extractToolResultContentText(result: UnknownRecord | undefined): string | undefined {
  const direct = rawString(result?.content);
  if (direct !== undefined) {
    return direct;
  }
  const content = getArray(result?.content);
  if (content === undefined) {
    return undefined;
  }
  const text = content
    .map((item) => {
      const record = asRecord(item);
      return rawString(record?.text) ?? rawString(record?.content);
    })
    .filter((item): item is string => item !== undefined)
    .join("\n")
    .trim();
  return text || undefined;
}
