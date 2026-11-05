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
  const version = value => typeof value === "string" && /^[0-9][0-9A-Za-z.+_!-]{0,127}$/.test(value);
  if (metadata.name !== name || !version(metadata.version) ||
      !version(metadata.qoderCliVersion) || metadata.qoderSdkBrand !== site)
    throw new Error("AW requires Qoder SDK and bundled Worker version metadata at the declared site");
  resolved[site] = { entry: realpathSync(entry), version: metadata.version,
    worker_version: metadata.qoderCliVersion };
}
if (Number(process.versions.node.split(".")[0]) < 22) throw new Error("AW Qoder SDK requires Node 22+");
process.stdout.write(JSON.stringify(resolved) + "\n");
