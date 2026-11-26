/** Native acceptance gateway: exercise the application seam before upstream dispatch. */
import http from "node:http";
import { once } from "node:events";
import { forwardCodexResponses, gatewayArgs, reviewedHookTrust } from "../sdk/typescript/src/codex-gateway.mjs";

const [upstream, home, hashes] = process.argv.slice(2);
const gateway = http.createServer(async (request, response) => {
  if (request.url !== "/v1/responses" || request.headers.authorization !== "Bearer fixture-key") {
    response.writeHead(403).end();
    return;
  }
  try {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const payload = JSON.parse(Buffer.concat(chunks));
    await forwardCodexResponses(payload, text => text.replaceAll("sample-credential", "[credential]"),
      async checked => {
        const reply = await fetch(upstream, { method: "POST", body: JSON.stringify(checked) });
        response.writeHead(reply.status, { "Content-Type": "text/event-stream" });
        response.end(Buffer.from(await reply.arrayBuffer()));
      });
  } catch {
    response.writeHead(422).end();
  }
});
gateway.listen(0, "127.0.0.1");
await once(gateway, "listening");
const options = { provider: "gateway", name: "Local Gateway", envKey: "LOCAL_CODEX_GATEWAY_KEY" };
console.log(JSON.stringify({ argv: gatewayArgs(gateway.address().port, options), trust: reviewedHookTrust(home, JSON.parse(hashes)) }));
