/** Package the optional Pi SDK adapter from canonical Adapter sources. */
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
  .replaceAll("../../../typescript/src/client.mjs", "@anolisa/aw-sdk")
  .replaceAll(
    "../../../typescript/src/configuration.mjs",
    "@anolisa/aw-sdk/configuration",
  )
  .replaceAll(
    "../../../typescript/src/task-scope.mjs",
    "@anolisa/aw-sdk/task-scope",
  )
  .replaceAll("../../../../crates/aw-adapter/native/", "./native/");
await writeFile(new URL("index.mjs", output), source);

for (const [directory, files] of [
  [
    "pi",
    [
      "entrypoint.mjs",
      "resident.mjs",
      "bridge.mjs",
      "producer.mjs",
      "scope.mjs",
      "skills.mjs",
      "submission.mjs",
      "transport.mjs",
      "sdk.mjs",
      "capabilities.json",
    ],
  ],
  ["shared", ["scope.mjs", "transport.mjs"]],
]) {
  await mkdir(new URL(`native/${directory}/`, output), { recursive: true });
  for (const file of files)
    await copyFile(
      new URL(`../../../crates/aw-adapter/native/${directory}/${file}`, root),
      new URL(`native/${directory}/${file}`, output),
    );
}
