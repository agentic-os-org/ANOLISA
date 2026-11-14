import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { test } from "node:test";
import assert from "node:assert/strict";

test("a verified patch reaches its exact module and rejects changed or ambiguous builds", () => {
  const root = mkdtempSync(join(tmpdir(), "aw-js-patch-"));
  try {
    const target = join(root, "node_modules/fixture-agent/lib/index.mjs");
    mkdirSync(join(root, "node_modules/fixture-agent/lib"), { recursive: true });
    const source = 'export const result = "raw";\n';
    const digest = createHash("sha256").update(source).digest("hex");
    const helper = pathToFileURL(resolve(import.meta.dirname, "module-patch.mjs")).href;
    const preload = join(root, "preload.mjs");
    const writePreload = expected => writeFileSync(preload, `import { installVerifiedPatch, removeOwnPreload } from ${JSON.stringify(helper)};
      installVerifiedPatch({target:'/node_modules/fixture-agent/lib/index.mjs',
        digest:${JSON.stringify(expected)},anchor:${JSON.stringify(source.trim())},
        replacement:'export const result = "checked";'});
      removeOwnPreload(import.meta.url);`);
    writePreload(digest);
    const url = pathToFileURL(target).href;
    const run = (options = "--no-warnings") => spawnSync(process.execPath, ["--input-type=module", "--eval",
      `const module = await import(${JSON.stringify(url)});
       console.log(JSON.stringify({result:module.result,options:process.env.NODE_OPTIONS}));`], {
      encoding: "utf8", timeout: 5000,
      env: { ...process.env, NODE_OPTIONS: `${options} --import=${pathToFileURL(preload).href}` },
    });
    writeFileSync(target, source);
    const accepted = run();
    assert.equal(accepted.status, 0, accepted.stderr);
    assert.deepEqual(JSON.parse(accepted.stdout), { result: "checked", options: "--no-warnings" });
    const duplicate = run(`--import=${pathToFileURL(preload).href}`);
    assert.notEqual(duplicate.status, 0);
    assert.match(duplicate.stderr, /preload is duplicated/);
    writeFileSync(target, 'export const result = "changed";\n');
    const changed = run();
    assert.notEqual(changed.status, 0);
    assert.match(changed.stderr, /does not support .* build/);
    writeFileSync(target, source + source);
    writePreload(createHash("sha256").update(source + source).digest("hex"));
    const ambiguous = run();
    assert.notEqual(ambiguous.status, 0);
    assert.match(ambiguous.stderr, /does not support .* build/);
    writeFileSync(target, source);
    writeFileSync(preload, `import { installVerifiedPatch, removeOwnPreload } from ${JSON.stringify(helper)};
      installVerifiedPatch({target:'/node_modules/fixture-agent/lib/index.mjs', variants:[
        {digest:'${"0".repeat(64)}',anchor:'unused',replacement:'other'},
        {digest:'${digest}',anchor:${JSON.stringify(source.trim())},
          replacement:'export const result = "checked";'}]});
      removeOwnPreload(import.meta.url);`);
    const selected = run();
    assert.equal(selected.status, 0, selected.stderr);
    assert.equal(JSON.parse(selected.stdout).result, "checked");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
