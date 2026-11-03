import { createHash } from "node:crypto";

type UnknownRecord = Record<string, unknown>;

function asRecord(value: unknown): UnknownRecord | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return undefined;
  }
  return value as UnknownRecord;
}

function safeString(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function firstNonEmptyString(...values: unknown[]): string {
  for (const value of values) {
    const text = safeString(value);
    if (text.trim()) {
      return text;
    }
  }
  return "";
}

export function inboundPiiScanText(event: unknown): string {
  const record = asRecord(event);
  return firstNonEmptyString(
    record?.content,
    record?.body,
    record?.userInput,
    record?.user_input,
    record?.userPrompt,
    record?.user_prompt,
    record?.prompt,
    record?.llmInput,
    record?.llm_input,
  );
}

/** Text exposed at the model-entry gate; omit media payloads and message metadata. */
export function modelInputPiiScanText(event: unknown): string {
  const record = asRecord(event);
  const parts = [safeString(record?.systemPrompt), safeString(record?.prompt)];
  for (const message of Array.isArray(record?.messages)
    ? record.messages
    : []) {
    const content = asRecord(message)?.content;
    if (typeof content === "string") {
      parts.push(content);
      continue;
    }
    for (const item of Array.isArray(content) ? content : []) {
      const block = asRecord(item);
      if (block?.type === "text") {
        parts.push(safeString(block.text));
      } else if (block?.type === "thinking") {
        parts.push(safeString(block.thinking));
      } else if (block?.type === "toolCall") {
        parts.push(valueToText(block.arguments));
      }
    }
  }
  return parts.filter((text) => text.trim()).join("\n\n");
}

export function valueToText(value: unknown): string {
  if (value === undefined || value === null) {
    return "";
  }
  if (typeof value === "string") {
    return value;
  }
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

export function afterToolCallPiiScanText(event: unknown): string {
  const record = asRecord(event);
  const result = valueToText(record?.result);
  if (result.trim()) {
    return result;
  }
  return safeString(record?.error);
}

export function textSha256(text: string): string {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

export function piiScanInputSha256(text: string): string | undefined {
  if (!text.trim()) {
    return undefined;
  }
  return textSha256(text);
}
