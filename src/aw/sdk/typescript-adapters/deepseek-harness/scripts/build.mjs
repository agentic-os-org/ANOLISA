import { copyFile, mkdir, readFile, rm, writeFile } from "node:fs/promises";
const root = new URL("../", import.meta.url);
const dist = new URL("dist/", root);
await rm(dist, { recursive: true, force: true });
await mkdir(new URL("native/shared/js-runtime/", dist), { recursive: true });
await mkdir(new URL("native/deepseek-harness/", dist), { recursive: true });
await copyFile(new URL("src/index.d.ts", root), new URL("index.d.ts", dist));
await copyFile(new URL("../../../../../LICENSE", root), new URL("LICENSE", dist));
const entry = (await readFile(new URL("src/index.mjs", root), "utf8"))
  .replace("../../../typescript/src/client.mjs", "@anolisa/aw-sdk")
  .replace("../../../typescript/src/configuration.mjs", "@anolisa/aw-sdk/configuration")
  .replaceAll("../../../../crates/aw-adapter/native/", "./native/");
await writeFile(new URL("index.mjs", dist), entry);
for (const file of ["deepseek-harness/hooks.mjs", "deepseek-harness/callback.mjs",
  "deepseek-harness/result-preload.mjs", "shared/js-runtime/module-patch.mjs"])
  await copyFile(new URL("../../../crates/aw-adapter/native/" + file, root), new URL("native/" + file, dist));
