/** Verify one published Node module before installing a launch-scoped patch. */
import { createHash } from "node:crypto";
import { registerHooks } from "node:module";

export function installVerifiedPatch({ target, digest, anchor, replacement, variants, onPatched }) {
  if (typeof registerHooks !== "function")
    throw Error("AW final result gate requires synchronous Node module hooks");
  const profiles = variants ?? [{ digest, anchor, replacement }];
  if (!target.startsWith("/node_modules/") || !Array.isArray(profiles) || !profiles.length ||
      new Set(profiles.map(profile => profile.digest)).size !== profiles.length ||
      !profiles.every(profile => /^[a-f0-9]{64}$/.test(profile.digest) && profile.anchor &&
        typeof profile.replacement === "string" && profile.anchor !== profile.replacement))
    throw Error("AW final result gate profile is invalid");
  let matched = false;
  return registerHooks({
    load(url, context, nextLoad) {
      const loaded = nextLoad(url, context);
      if (!url.endsWith(target)) return loaded;
      if (matched) throw Error("AW final result gate target loaded more than once");
      const source = loaded.source?.toString();
      const sourceDigest = source && createHash("sha256").update(source).digest("hex");
      const profile = profiles.find(item => item.digest === sourceDigest);
      if (!profile || source.split(profile.anchor).length !== 2)
        throw Error(`AW final result gate does not support ${target} build`);
      matched = true;
      onPatched?.({ url, digest: sourceDigest });
      return { ...loaded, source: source.replace(profile.anchor, profile.replacement) };
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
