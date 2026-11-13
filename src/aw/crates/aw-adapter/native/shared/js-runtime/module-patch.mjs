/** Verify one published Node module before installing a launch-scoped patch. */
import { createHash } from "node:crypto";
import { registerHooks } from "node:module";

export function installVerifiedPatch({ target, digest, anchor, replacement, onPatched }) {
  if (typeof registerHooks !== "function")
    throw Error("AW final result gate requires synchronous Node module hooks");
  if (!target.startsWith("/node_modules/") || !/^[a-f0-9]{64}$/.test(digest) ||
      !anchor || typeof replacement !== "string" || anchor === replacement)
    throw Error("AW final result gate profile is invalid");
  let matched = false;
  return registerHooks({
    load(url, context, nextLoad) {
      const loaded = nextLoad(url, context);
      if (!url.endsWith(target)) return loaded;
      if (matched) throw Error("AW final result gate target loaded more than once");
      const source = loaded.source?.toString();
      if (!source || createHash("sha256").update(source).digest("hex") !== digest ||
          source.split(anchor).length !== 2)
        throw Error("AW final result gate does not support this module build");
      matched = true;
      onPatched?.({ url, digest });
      return { ...loaded, source: source.replace(anchor, replacement) };
    },
  });
}

/** Prevent the preload from propagating to ordinary tools and Providers. */
export function removeOwnPreload(url) {
  const option = `--import=${url}`;
  const options = process.env.NODE_OPTIONS;
  if (options === option) delete process.env.NODE_OPTIONS;
  else if (options?.endsWith(` ${option}`)) {
    const original = options.slice(0, -option.length - 1);
    if (original.includes(option)) throw Error("AW final result preload is duplicated");
    process.env.NODE_OPTIONS = original;
  }
  else throw Error("AW final result preload option is missing from NODE_OPTIONS");
}
