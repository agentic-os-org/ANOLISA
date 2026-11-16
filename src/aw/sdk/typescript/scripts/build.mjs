/** Package native exports from their canonical Adapter sources. */
import { mkdir, copyFile, rm } from "node:fs/promises";
const root = new URL("../", import.meta.url);
const output = new URL("dist/", root);
await rm(output, { recursive: true, force: true });
await mkdir(new URL("native/qoder-sdk/", output), { recursive: true });
for (const file of [
  "client.mjs",
  "transport.mjs",
  "control.mjs",
  "index.d.ts",
  "qoder.d.ts",
])
  await copyFile(new URL(`src/${file}`, root), new URL(file, output));
for (const file of ["sdk.mjs", "callback.mjs", "skills.mjs"])
  await copyFile(
    new URL(`../../crates/aw-adapter/native/qoder-sdk/${file}`, root),
    new URL(`native/qoder-sdk/${file}`, output),
  );

await copyFile(
  new URL("../../../../LICENSE", root),
  new URL("LICENSE", output),
);
