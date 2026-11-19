/** Package the framework-free core SDK and shared Scope runtime helpers. */
import { mkdir, copyFile, rm, readFile, writeFile } from "node:fs/promises";
const root = new URL("../", import.meta.url);
const output = new URL("dist/", root);
await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });

for (const file of [
  "client.mjs",
  "admission.mjs",
  "configuration.mjs",
  "deployment.mjs",
  "transport.mjs",
  "control.mjs",
  "source-helpers.mjs",
  "sources.mjs",
  "index.d.ts",
  "sources.d.ts",
  "configuration.d.ts",
  "task-scope.d.ts",
  "qoder.d.ts",
  "pi.d.ts",
])
  await copyFile(new URL(`src/${file}`, root), new URL(file, output));

await copyFile(
  new URL("../../../../LICENSE", root),
  new URL("LICENSE", output),
);

// Packaged compatibility shims delegate only to installed optional adapter packages.
for (const file of ["qoder.mjs", "pi.mjs"]) {
  const source = await readFile(new URL(`src/${file}`, root), "utf8");
  const packaged = source.replace(
    /\/\* DEV_FALLBACK_START \*\/[\s\S]*?\/\* DEV_FALLBACK_END \*\//g,
    "",
  );
  await writeFile(new URL(file, output), packaged);
}

// TaskScope keeps only the shared Scope helper; framework bridges live in optional packages.
await writeFile(
  new URL("task-scope.mjs", output),
  (await readFile(new URL("src/task-scope.mjs", root), "utf8")).replaceAll(
    "../../../crates/aw-adapter/native/",
    "./native/",
  ),
);

await mkdir(new URL("native/shared/", output), { recursive: true });
for (const file of ["scope.mjs", "transport.mjs"])
  await copyFile(
    new URL(`../../crates/aw-adapter/native/shared/${file}`, root),
    new URL(`native/shared/${file}`, output),
  );
