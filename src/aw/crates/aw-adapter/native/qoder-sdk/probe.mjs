/** Resolve explicitly declared SDK sites without importing application code. */
import { createRequire } from "node:module";
import { readFileSync, realpathSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
const require = createRequire(pathToFileURL(process.argv.at(-2)));
const sites = JSON.parse(process.argv.at(-1));
const resolved = {};
for (const site of sites) {
  if (!["cn", "global"].includes(site)) throw new Error("Unsupported AW Qoder SDK site");
  const name = site === "cn" ? "@qodercn-ai/qodercn-agent-sdk" : "@qoder-ai/qoder-agent-sdk";
  const entry = require.resolve(name);
  const metadata = JSON.parse(readFileSync(join(dirname(entry), "../package.json"), "utf8"));
  if (metadata.name !== name || metadata.version !== "1.0.56" ||
      metadata.qoderCliVersion !== "1.1.78" || metadata.qoderSdkBrand !== site)
    throw new Error("AW requires Qoder SDK 1.0.56 with bundled Worker 1.1.78 at the declared site");
  resolved[site] = realpathSync(entry);
}
if (Number(process.versions.node.split(".")[0]) < 22) throw new Error("AW Qoder SDK requires Node 22+");
process.stdout.write(JSON.stringify(resolved) + "\n");
