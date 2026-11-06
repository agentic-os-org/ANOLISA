/** Fix the Coding Agent's selected Skill before input checks and native submission. */
import { open, realpath, stat } from "node:fs/promises";
import { constants } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join } from "node:path";

const limit = 1024 * 1024;
const failure = () => new Error("AW Pi Skill source could not be validated");
const identity = value => [value.dev, value.ino, value.size, value.mtimeNs, value.ctimeNs].join(":");

export function expandsSkill(text, options) {
  return typeof text === "string" && (options?.expandPromptTemplates ?? true) && text.startsWith("/skill:");
}

export async function prepareSkill(session, sdk, text, options) {
  const empty = { text, options, metadata: { aw_skill: null, aw_skill_request: null }, verify: async () => {} };
  if (!expandsSkill(text, options)) return empty;
  const space = text.indexOf(" ");
  const name = space === -1 ? text.slice(7) : text.slice(7, space);
  const args = space === -1 ? "" : text.slice(space + 1).trim();
  const failed = { ...empty, metadata: { aw_skill: { error_code: "skill_resolution_error" }, aw_skill_request: name },
    verify: async () => { throw failure(); } };
  try {
    if (!name || name.length > 1024 || session.extensionRunner.getCommand("skill:" + name)) return failed;
    const selected = session.resourceLoader.getSkills().skills.find(skill => skill.name === name);
    if (!selected || typeof selected.filePath !== "string" || typeof selected.baseDir !== "string") return failed;
    const { filePath, baseDir } = selected;
    const path = await realpath(filePath);
    const directory = await realpath(baseDir);
    if (directory === dirname(directory) || path !== join(directory, "SKILL.md")) return failed;
    const before = await stat(path, { bigint: true });
    if (!before.isFile() || before.size > BigInt(limit)) return failed;
    const file = await open(path, constants.O_RDONLY | constants.O_NONBLOCK);
    let content, digest;
    try {
      if (identity(await file.stat({ bigint: true })) !== identity(before)) return failed;
      const buffer = Buffer.alloc(Number(before.size) + 1);
      const { bytesRead } = await file.read(buffer, 0, buffer.length, 0);
      if (BigInt(bytesRead) !== before.size || identity(await file.stat({ bigint: true })) !== identity(before)) return failed;
      const bytes = buffer.subarray(0, bytesRead);
      digest = "sha256:" + createHash("sha256").update(bytes).digest("hex");
      content = bytes.toString("utf8");
    } finally { await file.close(); }
    const verify = async () => {
      if (await realpath(filePath) !== path || await realpath(baseDir) !== directory ||
          identity(await stat(path, { bigint: true })) !== identity(before)) throw failure();
    };
    await verify();
    // Use the SDK parser and native block format, then disable its second expansion.
    const body = sdk.stripFrontmatter(content).trim();
    const block = `<skill name="${name}" location="${filePath}">\nReferences are relative to ${baseDir}.\n\n${body}\n</skill>`;
    return { text: args ? `${block}\n\n${args}` : block, options: { ...options, expandPromptTemplates: false },
      metadata: { aw_skill: { name, directory, source: "pi-resource-loader", digest }, aw_skill_request: name }, verify };
  } catch { return failed; }
}
