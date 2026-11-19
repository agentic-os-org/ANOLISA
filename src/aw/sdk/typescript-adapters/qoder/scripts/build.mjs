/** Package the optional Qoder SDK adapter from canonical Adapter sources. */
import { mkdir, copyFile, rm, readFile, writeFile } from "node:fs/promises";
const root = new URL("../", import.meta.url);
const output = new URL("dist/", root);
await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });

await copyFile(new URL("src/index.d.ts", root), new URL("index.d.ts", output));
await copyFile(
  new URL("../../../../../LICENSE", root),
  new URL("LICENSE", output),
);

const source = (await readFile(new URL("src/index.mjs", root), "utf8"))
  .replaceAll(
    "../../../typescript/src/configuration.mjs",
    "@anolisa/aw-sdk/configuration",
  )
  .replaceAll("../../../typescript/src/client.mjs", "@anolisa/aw-sdk")
  .replaceAll("../../../typescript/src/admission.mjs", "@anolisa/aw-sdk")
  .replaceAll("../../../../crates/aw-adapter/native/", "./native/");
await writeFile(new URL("index.mjs", output), source);

await mkdir(new URL("native/qoder-sdk/", output), { recursive: true });
for (const file of ["sdk.mjs", "callback.mjs", "skills.mjs"])
  await copyFile(
    new URL(`../../../crates/aw-adapter/native/qoder-sdk/${file}`, root),
    new URL(`native/qoder-sdk/${file}`, output),
  );
