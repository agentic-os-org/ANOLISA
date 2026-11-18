/** Package native exports from their canonical Adapter sources. */
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
  "index.d.ts",
  "qoder.d.ts",
  "pi.d.ts",
])
  await copyFile(new URL(`src/${file}`, root), new URL(file, output));

await copyFile(
  new URL("../../../../LICENSE", root),
  new URL("LICENSE", output),
);

// Framework entries import canonical Adapter files; the package keeps them in native/.
for (const file of ["qoder.mjs", "pi.mjs", "task-scope.mjs"])
  await writeFile(
    new URL(file, output),
    (await readFile(new URL(`src/${file}`, root), "utf8")).replaceAll(
      "../../../crates/aw-adapter/native/",
      "./native/",
    ),
  );

for (const [directory, files] of [
  ["qoder-sdk", ["sdk.mjs", "callback.mjs", "skills.mjs"]],
  ["pi", ["entrypoint.mjs", "resident.mjs", "bridge.mjs", "producer.mjs",
    "scope.mjs", "skills.mjs", "submission.mjs", "transport.mjs", "sdk.mjs",
    "capabilities.json"]],
  ["shared", ["scope.mjs", "transport.mjs"]],
]) {
  await mkdir(new URL(`native/${directory}/`, output), { recursive: true });
  for (const file of files)
    await copyFile(
      new URL(`../../crates/aw-adapter/native/${directory}/${file}`, root),
      new URL(`native/${directory}/${file}`, output),
    );
}
