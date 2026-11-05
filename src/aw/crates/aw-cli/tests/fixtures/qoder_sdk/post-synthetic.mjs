/** The service fixture exposes strict Post errors through the public Query iterator. */
import { pathToFileURL } from "node:url";
import { runFixture } from "@qoder-ai/qoder-agent-sdk";
const { createQuery } = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const query = await createQuery({ prompt: "fixture", options: { settingSources: [], cwd: process.cwd() } });
let withheld = false;
try {
  try { await runFixture(); await query.next(); }
  catch (error) {
    if (!/callback_exit_1/.test(error.message)) throw error;
    withheld = true;
  }
} finally { await query.close(); }
process.stdout.write(JSON.stringify({ withheld }));
