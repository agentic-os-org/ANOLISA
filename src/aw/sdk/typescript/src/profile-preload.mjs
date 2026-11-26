/** Private, bounded bootstrap for NODE_OPTIONS=--import @anolisa/aw-sdk/profile-preload. */
import { constants } from "node:fs";
import { lstat, open } from "node:fs/promises";
import { isAbsolute, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { PROFILE_MAX_BYTES, parseProfileJson } from "./profile-eval.mjs";
import { ProfileSource } from "./profile-source.mjs";
import { AwError } from "./transport.mjs";

const sameFile = (a, b) =>
  ["dev", "ino", "uid", "mode", "size", "mtimeNs", "ctimeNs"].every(
    (field) => a[field] === b[field],
  );

async function readBootstrap(path) {
  let parent;
  let file;
  try {
    const parts = path.split("/").filter(Boolean);
    if (
      !isAbsolute(path) ||
      !parts.length ||
      parts.some((part) => [".", ".."].includes(part))
    )
      throw new Error();
    // Linux O_PATH pins each directory. O_NOFOLLOW then covers each component,
    // even when a parent directory is renamed concurrently.
    const directories = 0x200000 | constants.O_DIRECTORY | constants.O_NOFOLLOW;
    parent = await open("/", directories);
    for (const part of parts.slice(0, -1)) {
      const child = await open(
        `/proc/self/fd/${parent.fd}/${part}`,
        directories,
      );
      await parent.close();
      parent = child;
    }
    const name = `/proc/self/fd/${parent.fd}/${parts.at(-1)}`;
    file = await open(
      name,
      constants.O_RDONLY | constants.O_NONBLOCK | constants.O_NOFOLLOW,
    );
    const info = await file.stat({ bigint: true });
    if (
      !info.isFile() ||
      info.uid !== BigInt(process.getuid()) ||
      ![0o600n, 0o400n].includes(info.mode & 0o7777n) ||
      info.size > PROFILE_MAX_BYTES
    )
      throw new Error();
    const bytes = Buffer.alloc(PROFILE_MAX_BYTES + 1);
    let count = 0;
    while (count < bytes.length) {
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
      count > PROFILE_MAX_BYTES ||
      count !== Number(info.size) ||
      !sameFile(info, await file.stat({ bigint: true })) ||
      !sameFile(info, await lstat(name, { bigint: true }))
    )
      throw new Error();
    return bytes.subarray(0, count);
  } catch {
    throw new AwError("source_profile_bootstrap_invalid");
  } finally {
    await file?.close();
    await parent?.close();
  }
}

function ownPreload(specifier) {
  return (
    specifier === "@anolisa/aw-sdk/profile-preload" ||
    specifier === import.meta.url ||
    ((specifier?.startsWith(".") || specifier?.startsWith("/")) &&
      pathToFileURL(resolve(specifier)).href === import.meta.url)
  );
}

function removePreload(raw, values) {
  const kept = [];
  for (let i = 0; i < values.length; i++) {
    if (values[i] === "--import" && ownPreload(values[i + 1])) {
      i++;
    } else if (
      !(values[i].startsWith("--import=") && ownPreload(values[i].slice(9)))
    ) {
      kept.push(raw[i]);
    }
  }
  return kept;
}

function consumeBootstrap() {
  const path = process.env.AW_PROFILE_BOOTSTRAP;
  if (path === undefined) return;
  delete process.env.AW_PROFILE_BOOTSTRAP;
  // NODE_OPTIONS uses space-delimited words with double quotes and escapes
  // inside quotes. Keep the application's original spelling for child Node.
  const words =
    process.env.NODE_OPTIONS?.match(/(?:[^ "]+|"(?:\\.|[^"\\])*")+/gs) ?? [];
  const values = words.map((word) =>
    word.replace(/"((?:\\.|[^"\\])*)"/gs, (_, text) =>
      text.replace(/\\(.)/gs, "$1"),
    ),
  );
  const kept = removePreload(words, values);
  if (kept.length !== words.length) {
    if (kept.length) process.env.NODE_OPTIONS = kept.join(" ");
    else delete process.env.NODE_OPTIONS;
  }
  const argv = removePreload(process.execArgv, process.execArgv);
  process.execArgv.splice(0, process.execArgv.length, ...argv);
  return path;
}

async function bootstrap(path) {
  const value = parseProfileJson(await readBootstrap(path));
  if (!value || typeof value !== "object" || !Object.hasOwn(value, "profile"))
    throw new AwError("source_profile_bootstrap_invalid");
  const { profile, availability, resolveFrom, ...connection } = value;
  const source = await ProfileSource.probe(profile, {
    availability,
    resolveFrom: resolveFrom ?? process.argv[1],
  });
  source.install();
  try {
    await source.verify();
    if (
      connection.endpoint !== undefined ||
      connection.descriptorFile !== undefined
    )
      await source.activate(connection);
    return source;
  } catch (error) {
    await source.teardown();
    throw error;
  }
}

const path = consumeBootstrap();
export const preloadedProfileSource =
  path === undefined ? undefined : await bootstrap(path);
