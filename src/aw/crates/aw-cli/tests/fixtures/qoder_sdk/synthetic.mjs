/** Exercise the installed bridge against explicit SDK protocol fixtures, without a model. */
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { runFixture } from "@qoder-ai/qoder-agent-sdk";
const { createQuery } = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const query = await createQuery({ prompt: "fixture", options: {
  settingSources: [], cwd: process.cwd(),
} });
const output = await runFixture();
await query.return();
writeFileSync("query-closed", "yes");
process.stdout.write(JSON.stringify(output));
