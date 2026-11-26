/** Validation and bounded field expressions for aw-instrumentation-profile/v1alpha1. */
import { types } from "node:util";
import {
  clonePlainJson,
  isPlainObject,
  cloneStrictPlainJson,
  UNCLONEABLE,
} from "./source-helpers.mjs";
import { AwError } from "./transport.mjs";

export const PROFILE_MAX_BYTES = 1024 * 1024;
const VERSION = "aw-instrumentation-profile/v1alpha1";
const IDENTIFIER = /^[A-Za-z0-9._-]{1,128}$/;
const SEGMENT = /^(?!__)[A-Za-z_][A-Za-z0-9_]*$/;
const PACKAGE = /^(?:@[a-z0-9][a-z0-9._~-]*\/)?[a-z0-9][a-z0-9._~-]*$/;
const NUMERIC_VERSION = /^[0-9]+(?:\.[0-9]+){0,3}$/;
const COMPARATOR = /^(>=|<=|==|>|<)([0-9]+(?:\.[0-9]+){0,3})$/;
const LINE_BREAK = /[\r\n\0\u2028\u2029]/;
const CALLS = ["sync", "async", "sync_iter", "async_iter"];
const EVENTS = {
  "session.start": [],
  "input.submit": ["block"],
  "tool.before": ["block", "replace_input", "ask"],
  "tool.after": ["replace_result"],
  "permission.request": ["block", "ask"],
  "compact.before": ["block"],
  "compact.after": [],
  "subagent.start": ["block"],
  "subagent.stop": [],
  "turn.stop": ["block"],
  "session.end": [],
  "model.before_request": ["block"],
  "security.violation": ["block"],
};
const EVENT_FIELDS = {
  "input.submit": ["prompt"],
  "tool.before": ["tool_name", "call_id", "input"],
  "tool.after": ["tool_name", "call_id", "input", "result"],
  "permission.request": ["tool_name", "call_id", "input"],
  "turn.stop": ["output"],
  "model.before_request": ["request"],
  "security.violation": ["output"],
};

function check(condition, message) {
  if (!condition) throw new AwError("source_profile_invalid", { message });
}

function record(value, required = [], optional = []) {
  check(
    value !== null &&
      typeof value === "object" &&
      !Array.isArray(value) &&
      required.every((key) => Object.hasOwn(value, key)) &&
      (!required.length ||
        Object.keys(value).every(
          (key) => required.includes(key) || optional.includes(key),
        )),
    "Invalid profile object fields",
  );
}

function matches(value, pattern) {
  return (
    typeof value === "string" && !LINE_BREAK.test(value) && pattern.test(value)
  );
}

function index(value, max) {
  return matches(value, /^(0|[1-9][0-9]{0,3})$/) && Number(value) <= max;
}

export function parseFieldPath(path) {
  check(typeof path === "string" && path.length <= 256, "Invalid field path");
  const [root, ...parts] = path.split(".");
  check(
    ["receiver", "result", "arg"].includes(root),
    "Invalid field path root",
  );
  const arg = root === "arg" ? parts.shift() : undefined;
  check(root !== "arg" || index(arg, 31), "Invalid argument index");
  check(
    parts.length <= 6 &&
      parts.every(
        (part) =>
          (matches(part, SEGMENT) && part !== "constructor") ||
          index(part, 1023),
      ),
    "Invalid field path segment",
  );
  return { root, arg, parts };
}

function comparators(range) {
  check(
    typeof range === "string" && range.length <= 128,
    "Invalid package version range",
  );
  const entries = range.split(",");
  check(
    entries.length <= 8 && entries.every((entry) => matches(entry, COMPARATOR)),
    "Invalid package version comparator",
  );
  return entries.map((entry) => COMPARATOR.exec(entry).slice(1));
}

export function versionMatches(version, range) {
  if (!matches(version, NUMERIC_VERSION)) return false;
  const left = version.split(".").map(BigInt);
  return comparators(range).every(([operator, bound]) => {
    const right = bound.split(".").map(BigInt);
    const difference =
      [0, 1, 2, 3]
        .map((i) => (left[i] ?? 0n) - (right[i] ?? 0n))
        .find((value) => value !== 0n) ?? 0n;
    switch (operator) {
      case ">=":
        return difference >= 0n;
      case "<=":
        return difference <= 0n;
      case ">":
        return difference > 0n;
      case "<":
        return difference < 0n;
      case "==":
        return difference === 0n;
    }
  });
}

// JSON.parse validates the complete grammar. Walking the original tokens retains
// decoded object keys and numeric spellings that JSON.parse would discard.
export function parseProfileJson(input) {
  let source;
  try {
    if (typeof input === "string") {
      source = input;
    } else {
      check(
        input instanceof Uint8Array && input.byteLength <= PROFILE_MAX_BYTES,
        "Invalid or oversized profile JSON",
      );
      source = new TextDecoder("utf-8", {
        fatal: true,
        ignoreBOM: true,
      }).decode(input);
    }
    check(
      Buffer.byteLength(source) <= PROFILE_MAX_BYTES,
      "Oversized profile JSON",
    );
    const value = JSON.parse(source);
    const token =
      /"(?:[^"\\]|\\.)*"|[\[\]{},:]|-?[0-9][0-9.eE+-]*|true|false|null/gy;
    const whitespace = /[ \t\r\n]*/y;
    let position = 0;
    function next() {
      whitespace.lastIndex = position;
      whitespace.exec(source);
      token.lastIndex = whitespace.lastIndex;
      const match = token.exec(source);
      if (match) position = token.lastIndex;
      return match?.[0];
    }
    function walk(current, depth) {
      check(depth <= 32 && current !== undefined, "Invalid profile JSON depth");
      if (current === "{") {
        const keys = new Set();
        let keyToken = next();
        if (keyToken === "}") return;
        while (true) {
          const key = JSON.parse(keyToken);
          check(
            /^[\x00-\x7f]*$/.test(key) && !keys.has(key),
            "Duplicate or invalid profile key",
          );
          keys.add(key);
          next(); // Colon.
          walk(next(), depth + 1);
          if (next() === "}") return;
          keyToken = next();
        }
      } else if (current === "[") {
        let item = next();
        if (item === "]") return;
        while (true) {
          walk(item, depth + 1);
          if (next() === "]") return;
          item = next();
        }
      } else if (/^-?[0-9]/.test(current)) {
        check(
          /^-?[0-9]+$/.test(current) && Number.isSafeInteger(Number(current)),
          "Invalid profile integer",
        );
      } else if (current.startsWith('"')) {
        check(
          !/[\uD800-\uDFFF]/u.test(JSON.parse(current)),
          "Invalid Unicode string",
        );
      }
    }
    walk(next(), 0);
    return value;
  } catch (error) {
    if (error instanceof AwError) throw error;
    throw new AwError("source_profile_invalid", { cause: error });
  }
}

function uniqueChoices(values, allowed) {
  return (
    Array.isArray(values) &&
    new Set(values).size === values.length &&
    values.every((value) => allowed.includes(value))
  );
}

function validateHooks(hooks) {
  record(hooks);
  check(Object.keys(hooks).length > 0, "At least one target Hook is required");
  for (const [event, hook] of Object.entries(hooks)) {
    check(Object.hasOwn(EVENTS, event), "Invalid instrumentation Hook event");
    record(hook, ["mode", "effects", "failure_actions"]);
    const effects = hook.mode === "control" ? EVENTS[event] : [];
    check(
      ["control", "observe"].includes(hook.mode) &&
        uniqueChoices(hook.effects, ["observe", ...effects]) &&
        hook.effects.includes("observe"),
      "Invalid Hook effects",
    );
    const actions = ["report"];
    if (hook.effects.includes("block")) actions.push("block");
    if (hook.mode === "control" && event === "tool.after")
      actions.push("withhold_result");
    check(
      uniqueChoices(hook.failure_actions, actions),
      "Invalid Hook failure actions",
    );
  }
}

function validateFields({ hooks, fields }) {
  record(fields);
  const allowed = [
    "session_id",
    "turn_id",
    ...Object.keys(hooks).flatMap((event) => EVENT_FIELDS[event] ?? []),
  ];
  for (const [field, binding] of Object.entries(fields)) {
    check(allowed.includes(field), "Field is unavailable for the target Hooks");
    record(binding);
    if (Object.hasOwn(binding, "const")) {
      record(binding, ["const"]);
      const value = binding.const;
      check(
        field === "tool_name" &&
          typeof value === "string" &&
          [...value].length <= 128 &&
          /\P{White_Space}/u.test(value) &&
          !LINE_BREAK.test(value),
        "Invalid constant field",
      );
    } else {
      record(binding, ["from", "codec"]);
      const { root } = parseFieldPath(binding.from);
      check(
        root !== "result" || ["result", "output"].includes(field),
        "The result root is unavailable for this field",
      );
      check(
        ["text", "json", "json_string"].includes(binding.codec),
        "Invalid field codec",
      );
    }
  }
  const required = {
    "tool.before": ["tool_name", "input"],
    "tool.after": ["tool_name", "result"],
    "input.submit": ["prompt"],
    "permission.request": ["tool_name"],
    "security.violation": ["output"],
  };
  for (const event of Object.keys(hooks)) {
    check(
      (required[event] ?? []).every((field) => Object.hasOwn(fields, field)),
      "Required target Hook field is missing",
    );
  }
  if (hooks["tool.before"]?.effects.includes("replace_input"))
    check(
      ["json", "json_string"].includes(fields.input.codec),
      "Invalid replace_input codec",
    );
  if (hooks["tool.after"]?.effects.includes("replace_result"))
    check(
      parseFieldPath(fields.result.from).root === "result",
      "Invalid replace_result path",
    );
}

function validateTarget(target, profile) {
  record(
    target,
    ["id", "module", "qualname", "call", "hooks", "fields"],
    ["stream", "code_sha256"],
  );
  check(matches(target.id, IDENTIFIER), "Invalid target ID");
  const name = profile.package.name;
  check(
    typeof target.module === "string" &&
      target.module.length <= 256 &&
      (target.module === name ||
        (target.module.startsWith(name + "/") &&
          target.module
            .slice(name.length + 1)
            .split("/")
            .every(
              (part) =>
                matches(part, /^[A-Za-z0-9._-]+$/) &&
                part !== "." &&
                part !== "..",
            ))),
    "Invalid package module specifier",
  );
  check(
    typeof target.qualname === "string" &&
      target.qualname.length <= 256 &&
      target.qualname.split(".").length <= 4 &&
      target.qualname.split(".").every((part) => matches(part, SEGMENT)),
    "Invalid qualified name",
  );
  check(CALLS.includes(target.call), "Invalid target call shape");
  validateHooks(target.hooks);
  validateFields(target);

  const iterator = target.call.endsWith("_iter");
  if (Object.hasOwn(target, "stream")) {
    const stream = target.stream;
    record(stream, ["buffer", "max_items", "max_bytes"]);
    check(
      iterator &&
        typeof stream.buffer === "boolean" &&
        Number.isInteger(stream.max_items) &&
        stream.max_items >= 1 &&
        stream.max_items <= 65536 &&
        Number.isInteger(stream.max_bytes) &&
        stream.max_bytes >= 1 &&
        stream.max_bytes <= 16777216,
      "Invalid stream configuration",
    );
  }
  const after = target.hooks["tool.after"];
  const gate = target.hooks["security.violation"];
  if (
    iterator &&
    (after?.effects.includes("replace_result") ||
      after?.failure_actions.includes("withhold_result") ||
      (gate?.mode === "control" &&
        parseFieldPath(target.fields.output.from).root === "result"))
  )
    check(
      target.stream?.buffer === true,
      "The target requires a buffered stream",
    );
  if (Object.hasOwn(target, "code_sha256") || profile.origin === "custom")
    check(
      matches(target.code_sha256, /^[a-f0-9]{64}$/),
      "Invalid or missing code_sha256",
    );
}

function freeze(value) {
  if (value && typeof value === "object") {
    for (const child of Object.values(value)) freeze(child);
    Object.freeze(value);
  }
  return value;
}

export function validateProfile(input) {
  let profile;
  if (typeof input === "string" || input instanceof Uint8Array) {
    profile = parseProfileJson(input);
  } else {
    profile = clonePlainJson(input);
    check(profile !== UNCLONEABLE, "Profile must contain JSON data");
  }
  record(profile, ["version", "id", "origin", "runtime", "package", "targets"]);
  check(profile.version === VERSION, "Invalid instrumentation profile version");
  check(matches(profile.id, IDENTIFIER), "Invalid profile ID");
  check(
    ["custom", "builtin"].includes(profile.origin),
    "Invalid profile origin",
  );
  check(profile.runtime === "js", "The profile runtime must be js");
  record(profile.package, ["name", "versions"]);
  check(
    matches(profile.package.name, PACKAGE) &&
      profile.package.name.length <= 214,
    "Invalid npm package name",
  );
  comparators(profile.package.versions);
  check(
    Array.isArray(profile.targets) &&
      profile.targets.length >= 1 &&
      profile.targets.length <= 32,
    "Invalid target count",
  );
  const ids = new Set();
  const seams = new Set();
  for (const target of profile.targets) {
    validateTarget(target, profile);
    const seam = target.module + ":" + target.qualname;
    check(
      !ids.has(target.id) && !seams.has(seam),
      "Duplicate target ID or seam",
    );
    ids.add(target.id);
    seams.add(seam);
  }
  return freeze(profile);
}

export function intersectHooks(targets) {
  const hooks = {};
  for (const target of targets) {
    for (const [event, hook] of Object.entries(target.hooks)) {
      const previous = hooks[event];
      hooks[event] = {
        mode:
          hook.mode === "observe" ? "observe" : (previous?.mode ?? "control"),
        effects: hook.effects.filter(
          (value) => !previous || previous.effects.includes(value),
        ),
        failure_actions: hook.failure_actions.filter(
          (value) => !previous || previous.failure_actions.includes(value),
        ),
      };
    }
  }
  return freeze(hooks);
}

function child(value, key) {
  if (value == null || typeof value !== "object" || types.isProxy(value))
    return undefined;
  if (/^[0-9]/.test(key) && !Array.isArray(value)) return undefined;
  return Object.getOwnPropertyDescriptor(value, key)?.value;
}

export function fieldValue(binding, context) {
  if (!binding) return undefined;
  if (Object.hasOwn(binding, "const")) return binding.const;
  const { root, arg, parts } = parseFieldPath(binding.from);
  return parts.reduce(
    child,
    root === "arg" ? context.args[arg] : context[root],
  );
}

export function readField(
  binding,
  context,
  raw = fieldValue(binding, context),
) {
  if (raw == null || Object.hasOwn(binding, "const")) return raw;
  switch (binding.codec) {
    case "text":
      if (typeof raw === "string") return raw;
      if (["number", "boolean", "bigint"].includes(typeof raw))
        return String(raw);
      break;
    case "json": {
      const copy = cloneStrictPlainJson(raw);
      if (copy !== UNCLONEABLE) return copy;
      break;
    }
    case "json_string":
      if (typeof raw !== "string") break;
      try {
        return JSON.parse(raw);
      } catch (error) {
        throw new AwError("source_invalid_payload", { cause: error });
      }
  }
  throw new AwError("source_invalid_payload");
}

export function replaceField(binding, context, replacement, root) {
  const encoded =
    binding.codec === "json_string" ? JSON.stringify(replacement) : replacement;
  if (binding.codec === "text" && typeof encoded !== "string")
    throw new AwError(
      root === "arg"
        ? "source_input_replace_unsupported"
        : "source_result_replace_unsupported",
    );
  return writeField(binding, context, structuredClone(encoded), root);
}

export function writeField(binding, context, encoded, root) {
  const path = parseFieldPath(binding.from);
  const code =
    root === "arg"
      ? "source_input_replace_unsupported"
      : "source_result_replace_unsupported";
  if (path.root !== root) throw new AwError(code);
  function rebuild(value, depth) {
    if (depth === path.parts.length) return encoded;
    if (
      value == null ||
      typeof value !== "object" ||
      types.isProxy(value) ||
      (!Array.isArray(value) && !isPlainObject(value))
    )
      throw new AwError(code);
    const key = path.parts[depth];
    const descriptors = Object.getOwnPropertyDescriptors(value);
    const property = Object.hasOwn(descriptors, key)
      ? descriptors[key]
      : undefined;
    if (
      !property ||
      !Object.hasOwn(property, "value") ||
      (/^[0-9]/.test(key) && !Array.isArray(value))
    )
      throw new AwError(code);
    property.value = rebuild(property.value, depth + 1);
    const prototype = Object.getPrototypeOf(value);
    const copy = Array.isArray(value)
      ? Object.setPrototypeOf([], prototype)
      : Object.create(prototype);
    return Object.defineProperties(copy, descriptors);
  }
  if (root === "result") return rebuild(context.result, 0);
  const args = [...context.args];
  args[path.arg] = rebuild(args[path.arg], 0);
  return args;
}
