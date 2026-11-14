/** Verify one published Node module before installing a launch-scoped patch. */
import { createHash } from "node:crypto";
import { registerHooks } from "node:module";

export function installVerifiedPatch({ target, digest, anchor, replacement, patches, variants, onPatched }) {
  if (typeof registerHooks !== "function")
    throw Error("AW final result gate requires synchronous Node module hooks");
  const candidates = variants ?? [{ digest, anchor, replacement, patches }];
  if (!Array.isArray(candidates)) throw Error("AW final result gate profile is invalid");
  const profiles = candidates.map(profile => ({
    digest: profile.digest, patches: profile.patches ?? [{ anchor: profile.anchor, replacement: profile.replacement }],
  }));
  if (!target.startsWith("/node_modules/") || !Array.isArray(profiles) || !profiles.length ||
      new Set(profiles.map(profile => profile.digest)).size !== profiles.length ||
      !profiles.every(profile => /^[a-f0-9]{64}$/.test(profile.digest) &&
        Array.isArray(profile.patches) && profile.patches.length > 0 && profile.patches.length <= 16 &&
        profile.patches.every(patch => typeof patch.anchor === "string" && patch.anchor.length > 0 && typeof patch.replacement === "string" &&
          patch.anchor !== patch.replacement)))
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
      if (!profile || profile.patches.some(patch => source.split(patch.anchor).length !== 2))
        throw Error(`AW final result gate does not support ${target} build`);
      const edits = profile.patches.map(patch => ({ ...patch, start: source.indexOf(patch.anchor) }))
        .sort((a, b) => a.start - b.start);
      let patched = '', offset = 0;
      for (const edit of edits) {
        if (edit.start < offset) throw Error('AW final result gate patches overlap');
        patched += source.slice(offset, edit.start) + edit.replacement;
        offset = edit.start + edit.anchor.length;
      }
      matched = true;
      onPatched?.({ url, digest: sourceDigest });
      return { ...loaded, source: patched + source.slice(offset) };
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
