/** Trusted deployment hints bind bytes; their hash does not authenticate a publisher. */
import { constants, promises as fs } from "node:fs";
import { open, lstat } from "node:fs/promises";
import { createHash } from "node:crypto";
import { dirname, isAbsolute } from "node:path";
import { AwError } from "./transport.mjs";

const limit = 4 * 1024 * 1024;
const identity = (info) =>
  [
    info.dev,
    info.ino,
    info.mode,
    info.uid,
    info.gid,
    info.nlink,
    info.size,
    info.mtimeNs,
  ].join(":");
const ownedDirectory = (info) =>
  info.uid === BigInt(process.geteuid()) && (info.mode & 0o022n) === 0n;

// These Linux filesystems use the POSIX mode/ACL mask for peer write access.
// Refuse filesystems whose native ACLs cannot be inspected by Node's standard API.
const supportedFilesystems = new Set([
  0xef53n,
  0x58465342n,
  0x9123683en,
  0x01021994n,
  0x794c7630n,
]);
async function checkFilesystem(file) {
  const info = await fs.statfs(`/proc/self/fd/${file.fd}`, { bigint: true });
  if (!supportedFilesystems.has(info.type)) throw new Error();
}

async function readFile(path) {
  if (
    typeof path !== "string" ||
    !isAbsolute(path) ||
    path.split("/").includes("..")
  )
    throw new AwError("sdk_descriptor_file_invalid");
  let file, parent;
  try {
    const parts = path.split("/").filter((part) => part && part !== ".");
    if (!parts.length) throw new Error();
    const expectedParent = await fs.stat(dirname(path), { bigint: true });
    // Node does not expose Linux O_PATH. Pin directories without requiring read access.
    const flags = 0x200000 | constants.O_DIRECTORY | constants.O_NOFOLLOW;
    parent = await open("/", flags);
    for (const part of parts.slice(0, -1)) {
      const child = await open(`/proc/self/fd/${parent.fd}/${part}`, flags);
      await parent.close();
      parent = child;
    }
    const parentInfo = await parent.stat({ bigint: true });
    if (
      parentInfo.dev !== expectedParent.dev ||
      parentInfo.ino !== expectedParent.ino ||
      !ownedDirectory(parentInfo)
    )
      throw new Error();
    await checkFilesystem(parent);
    const anchored = `/proc/self/fd/${parent.fd}/${parts.at(-1)}`;
    file = await open(
      anchored,
      constants.O_RDONLY | constants.O_NONBLOCK | constants.O_NOFOLLOW,
    );
    await checkFilesystem(file);
    const info = await file.stat({ bigint: true });
    if (
      !info.isFile() ||
      info.size > BigInt(limit) ||
      info.uid !== BigInt(process.geteuid()) ||
      (info.mode & 0o7022n) !== 0n ||
      info.nlink !== 1n
    )
      throw new Error();
    const bytes = Buffer.allocUnsafe(limit + 1);
    let count = 0;
    while (count <= limit) {
      const { bytesRead } = await file.read(
        bytes,
        count,
        bytes.length - count,
        null,
      );
      if (!bytesRead) break;
      count += bytesRead;
    }
    if (
      count > limit ||
      identity(info) !== identity(await file.stat({ bigint: true })) ||
      identity(info) !== identity(await lstat(anchored, { bigint: true })) ||
      !ownedDirectory(await parent.stat({ bigint: true }))
    )
      throw new Error();
    return bytes.subarray(0, count);
  } catch {
    throw new AwError("sdk_descriptor_file_invalid");
  } finally {
    await file?.close();
    await parent?.close();
  }
}

// The descriptor grammar needs only objects, strings and booleans. Parse keys
// before building objects so JSON.parse cannot erase duplicate declarations.
function parse(text) {
  let position = 0;
  const whitespace = () => {
    while (/[\x20\t\r\n]/.test(text[position] ?? "x")) position++;
  };
  const string = () => {
    whitespace();
    const token =
      /"(?:[^"\\\u0000-\u001f]|\\(?:["\\/bfnrt]|u[0-9a-fA-F]{4}))*"/y;
    token.lastIndex = position;
    const found = token.exec(text);
    if (!found) throw new Error();
    position = token.lastIndex;
    const value = JSON.parse(found[0]);
    if (/[\uD800-\uDFFF]/u.test(value)) throw new Error();
    return value;
  };
  const object = (depth) => {
    whitespace();
    if (depth > 2 || text[position++] !== "{") throw new Error();
    const result = Object.create(null);
    const keys = new Set();
    whitespace();
    if (text[position] !== "}") {
      while (true) {
        const key = string();
        if (keys.has(key)) throw new Error();
        keys.add(key);
        whitespace();
        if (text[position++] !== ":") throw new Error();
        whitespace();
        let value;
        if (text[position] === '"') value = string();
        else if (text[position] === "{") value = object(depth + 1);
        else if (text.startsWith("true", position)) {
          value = true;
          position += 4;
        } else if (text.startsWith("false", position)) {
          value = false;
          position += 5;
        } else throw new Error();
        result[key] = value;
        whitespace();
        if (text[position] !== ",") break;
        position++;
      }
    }
    if (text[position++] !== "}") throw new Error();
    return result;
  };
  const value = object(1);
  whitespace();
  if (position !== text.length) throw new Error();
  return value;
}

const fields = (value, names) =>
  value &&
  typeof value === "object" &&
  Object.keys(value).length === names.length &&
  names.every((key) => Object.hasOwn(value, key));

export async function loadDescriptor({
  configFile,
  descriptorFile,
  profile,
  target,
  requiresService = false,
}) {
  const bytes = await readFile(descriptorFile);
  let value;
  try {
    value = parse(
      new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes),
    );
    if (
      !fields(value, [
        "version",
        "configuration_sha256",
        "target",
        "profile",
        "endpoint",
        "wait_for_input_submit",
        ...(Object.hasOwn(value, "observe_session_start") ? ["observe_session_start"] : []),
        "availability",
      ]) ||
      !fields(value.profile, ["adapter", "version", "entrypoint"]) ||
      value.version !== "aw-deployment/v1alpha1" ||
      typeof value.configuration_sha256 !== "string" ||
      !/^[a-f0-9]{64}$/.test(value.configuration_sha256) ||
      typeof value.target !== "string" ||
      !/^[a-zA-Z0-9._-]{1,128}$/.test(value.target) ||
      typeof value.endpoint !== "string" ||
      !value.endpoint.startsWith("/") ||
      value.endpoint.includes("\0") ||
      typeof value.wait_for_input_submit !== "boolean" ||
      (value.observe_session_start !== undefined &&
        typeof value.observe_session_start !== "boolean") ||
      !["required", "native_fallback"].includes(value.availability) ||
      Object.values(value.profile).some(
        (text) => typeof text !== "string" || !/\P{White_Space}/u.test(text),
      )
    )
      throw new Error();
  } catch {
    throw new AwError("sdk_descriptor_invalid");
  }
  if (
    !fields(profile, ["adapter", "version", "entrypoint"]) ||
    Object.keys(value.profile).some(
      (key) => value.profile[key] !== profile[key],
    ) ||
    (requiresService && value.availability !== "required")
  )
    throw new AwError("sdk_descriptor_profile_mismatch");
  if (target != null && target !== value.target)
    throw new AwError("sdk_descriptor_target_mismatch");
  if (
    createHash("sha256")
      .update(await readFile(configFile))
      .digest("hex") !== value.configuration_sha256
  )
    throw new AwError("sdk_descriptor_stale");
  return {
    target: value.target,
    endpoint: value.endpoint,
    inputEnabled: value.wait_for_input_submit,
    ...(value.observe_session_start ? { sessionStartEnabled: true } : {}),
    availability: value.availability,
  };
}
