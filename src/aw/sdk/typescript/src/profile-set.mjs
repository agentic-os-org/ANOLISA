/** One patch owner per package and one capability ceiling per process. */
import { createHash } from "node:crypto";
import { versionMatches } from "./profile-eval.mjs";
import { ProfilePatches } from "./profile-patch.mjs";
import { AwError } from "./transport.mjs";

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value === null || typeof value !== "object") return value;
  return Object.fromEntries(
    Object.keys(value).sort().map((key) => [key, canonical(value[key])]),
  );
}

export function verifyProfileBinding(binding, documents, capabilities) {
  if (!binding) return;
  const digests = Object.fromEntries(
    documents.map((doc) => [
      doc.id,
      createHash("sha256").update(JSON.stringify(canonical(doc))).digest("hex"),
    ]),
  );
  const equal = (a, b) => JSON.stringify(canonical(a)) === JSON.stringify(canonical(b));
  const normalized = (value) => Object.fromEntries(
    Object.entries(value).map(([event, effects]) => [event, [...effects].sort()]),
  );
  if (
    binding.runtime !== "js" ||
    documents.some((doc) => doc.origin !== "builtin") ||
    !equal(binding.profiles, digests) ||
    !equal(normalized(binding.events), normalized(capabilities.events)) ||
    !equal(binding.failure_actions, capabilities.failure_actions ?? {})
  ) throw new AwError("source_profile_binding_mismatch");
}

export class ProfileSet {
  constructor(documents, targets, options) {
    this.documents = documents;
    this.targets = targets;
    const ids = new Set(this.documents.map(({ id }) => id));
    const seams = new Set(this.targets.map(({ module, qualname }) => `${module}:${qualname}`));
    if (ids.size !== this.documents.length || seams.size !== this.targets.length)
      throw new AwError("source_profile_binding_mismatch");
    const groups = Map.groupBy(this.documents, (doc) => doc.package.name);
    this.owners = new Map();
    this.patches = [];
    for (const docs of groups.values()) {
      const targets = docs.flatMap((doc) => doc.targets);
      const patches = new ProfilePatches({ ...docs[0], targets }, options);
      if (docs.some(({ package: spec }) => !versionMatches(patches.packageVersion, spec.versions)))
        throw new AwError("source_probe_package_mismatch");
      this.patches.push(patches);
      for (const target of targets) this.owners.set(target, patches);
    }
    this.packageVersion = this.patches[0].packageVersion;
  }

  async probe() {
    for (const patches of this.patches) await patches.probe();
  }

  install(invoke, onFailure) {
    this.installed = [];
    try {
      for (const patches of this.patches) {
        patches.install(invoke, onFailure);
        this.installed.push(patches);
      }
    } catch (error) {
      this.restore();
      throw error;
    }
  }

  async verify() {
    for (const patches of this.patches) await patches.verify();
  }

  verifyCurrent(target, ...args) {
    this.owners.get(target).verifyCurrent(target, ...args);
  }

  restore() {
    let failure;
    for (const patches of this.installed?.splice(0).reverse() ?? []) {
      try {
        patches.restore();
      } catch (error) {
        failure ??= error;
      }
    }
    if (failure) throw failure;
  }
}
