import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile, readFile, chmod, rm } from "node:fs/promises";
import { loadConfiguration } from "../src/configuration.mjs";

const profile = { adapter: "fixture", version: "v1", entrypoint: "worker" };
const projection = {
  target: "app",
  daemon: { endpoint: "auto", state_dir: "/service" },
  input_enabled: false,
  target_requires_service: false,
};
async function parser(t) {
  const root = await mkdtemp("/tmp/aw-config-projection-");
  t.after(() => rm(root, { recursive: true, force: true }));
  const executable = root + "/parser";
  const argv = root + "/argv";
  await writeFile(executable, `#!/bin/sh
printf '%s\\0' "$@" > "$AW_CONFIG_ARGV_FILE"
if [ "$AW_CONFIG_REJECT" = 1 ]; then
  printf '%s' 'unsupported argument --sdk-target' >&2
  exit 2
fi
printf '%s' "$AW_CONFIG_RESPONSE"
`);
  await chmod(executable, 0o700);
  return {
    argv,
    async load(document, options = {}) {
      return loadConfiguration({
        configFile: "/config", profile, executable, ...options,
        environment: {
          ...process.env,
          AW_CONFIG_ARGV_FILE: argv,
          AW_CONFIG_RESPONSE: JSON.stringify(document),
          AW_CONFIG_REJECT: "0",
          ...options.environment,
        },
      });
    },
  };
}

test("parser receives adapter/target and preserves explicit service constraints", async (t) => {
  const f = await parser(t);
  for (const target of [undefined, "app", "-app"]) {
    for (const required of [false, true]) {
      for (const requiresService of [false, true]) {
        const wiring = await f.load(
          { ...projection, target: target ?? "app", target_requires_service: required },
          { target, requiresService },
        );
        const expected = [
          "config", "show", "--config", "/config", "--sdk-target",
          target === undefined ? "fixture" : `fixture/${target}`,
        ];
        assert.deepEqual((await readFile(f.argv, "utf8")).split("\0").slice(0, -1), expected);
        assert.equal(wiring.target, target ?? "app");
        assert.equal(wiring.endpoint, "/service/aw.sock");
        assert.equal(wiring.inputEnabled, false);
        assert.equal(
          wiring.availability,
          required || requiresService ? "required" : "native_fallback",
        );
      }
    }
  }
});

test("a parser rejecting projection arguments cannot fall back to native", async (t) => {
  const f = await parser(t);
  await assert.rejects(
    () => f.load(projection, { environment: { AW_CONFIG_REJECT: "1" } }),
    { code: 2 },
  );
});

test("projection booleans are strict even when service is explicitly required", async (t) => {
  const f = await parser(t);
  for (const key of ["input_enabled", "target_requires_service"]) {
    for (const invalid of [undefined, null, 0, 1, "false", [], {}]) {
      await assert.rejects(
        () => f.load({ ...projection, [key]: invalid }, { requiresService: true }),
        { code: "sdk_configuration_invalid" },
      );
    }
  }
});

test("missing or mismatched targets retain their error before projection checks", async (t) => {
  const f = await parser(t);
  for (const [selected, target] of [
    [null, undefined], [null, "missing"], ["other", "app"], ["", undefined],
  ]) {
    await assert.rejects(
      () => f.load({ target: selected }, { target }),
      { code: "sdk_target_required" },
    );
  }
});

test("explicit targets are preserved without splitting or fallback", async (t) => {
  const f = await parser(t);
  for (const target of ["", "app/other", "/app", "app/"]) {
    await assert.rejects(
      () => f.load({ target: null }, { target }),
      { code: "sdk_target_required" },
    );
    assert.deepEqual(
      (await readFile(f.argv, "utf8")).split("\0").slice(-3, -1),
      ["--sdk-target", `fixture/${target}`],
    );
  }
});
