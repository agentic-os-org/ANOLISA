/** Drive one policy-free tool turn through the reloadable SDK queue fixture. */
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
const { createQuery, startDaemon } = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const daemon = await startDaemon({});
const query = await createQuery({ prompt: "metadata", options: {
  daemon, settingSources: [], cwd: process.cwd(),
} });
let result;
for await (const message of query) {
  if (message.type === "result") { result = message; break; }
}
await query.close();
await daemon.stop();
writeFileSync("query-closed", "yes");
process.stdout.write(JSON.stringify(result));
