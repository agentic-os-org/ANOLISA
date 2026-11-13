/** Check converted tool results before the embedded Agent calls its model. */
import { installVerifiedPatch, removeOwnPreload } from "../shared/js-runtime/module-patch.mjs";

const modelGate = Symbol.for("aw.openclaw.modelGate");
const patched = Symbol.for("aw.openclaw.finalPatched");
const payloadGate = Symbol.for("aw.openclaw.payloadGate");
const providerPatched = Symbol.for("aw.openclaw.providerPatched");
const modelAnchor = `const transformed = config.transformContext ? await config.transformContext(messages, projectionSignal) : messages;
\t\treturn config.convertToLlm(normalizeCoreContextMessages(transformed));`;
const providerAnchor = `let params = buildOpenAICompletionsParams(model, context, options);
\t\t\t\tconst nextParams = await options?.onPayload?.(params, model);
\t\t\t\tif (nextParams !== void 0) params = nextParams;
\t\t\t\tif (options?.openclawCodeModeToolSurface === true) {
\t\t\t\t\tconst visibleToolNames = resolveCodeModeResponsesVisibleToolNames(context);
\t\t\t\t\tenforceCodeModeResponsesToolSurface(params, visibleToolNames, void 0, codeModeToolSurfaceObserver.get(options));
\t\t\t\t\tassertCodeModeResponsesToolSurface(params, visibleToolNames);
\t\t\t\t}
\t\t\t\tif (getCompat(model).requiresNonEmptyUserOrAssistantMessage) assertOpenAICompletionsPayloadHasConversationTurn(params, model);`;

const profiles = [
  {
    resolver: "attempt.model-diagnostic-events-cm5plRPy.mjs",
    resolverDigest: "4bb03fdf79a2d16f094ce168577c79cd7dfe6483e89d6adf0e490db34917d7eb",
    resolveFunction: "resolveEmbeddedAgentStream",
    core: "agent-core-iKDcH8nh.mjs",
    coreDigest: "990da1e3b53940558a6996f8631a8774bb07639ec92d8a614bdf30aab7a505cf",
    transportDigest: "c536d07188bbe9479f31d1a4733bd90f1f9404317afd6e38725a79c9296e445f",
    transportAnchor: providerAnchor,
  },
  {
    resolver: "attempt.model-diagnostic-events-B1vBuHmP.mjs",
    resolverDigest: "4f2cb3e7bcb05424da43b2f8f073d7afb6a1248fe09bff6b7e7d226c3712bbd7",
    resolveFunction: "resolveEmbeddedAgentStream",
    core: "agent-core-DG5aPUnb.mjs",
    coreDigest: "96221c65ea8e15d6499ca9f817e5d7629151b42c6d9742000791f2f48f2cebcd",
    transportDigest: "038e2c72e79a3e1c9d06e9c5f3a61a13acc297e1026696da24fee634a3bc4abc",
    transportAnchor: providerAnchor,
  },
  {
    resolver: "session-boundary-prompt-cache-key-CDZeB8W0.mjs",
    resolverDigest: "08cdd0961479d778f0d7385f7080bed5f25623e1f1f47b0db4fc6dff61202a13",
    resolveFunction: "selectEmbeddedAgentStream",
    core: "agent-core-yAMLnNkR.mjs",
    coreDigest: "28bd7caff972c4fc5dbcfa14d88e45be4795ef79809342295056aa3ca54bf72a",
    transportDigest: "31ac730235a155a5a662096c3d467554e07b66a456b05b1971a081379ab4bc51",
    transportAnchor: providerAnchor.replace("const nextParams =",
      "const encodeBody = prepareModelRequestBody(options);\n\t\t\t\tconst nextParams =") + `
				const emitReasoning = shouldEmitOpenAICompletionsReasoning(model, options);
				firstEventAbort = createFirstStreamEventAbortController(options?.signal);
				const { data: responseStream, response } = await client.chat.completions.create(params, {
					...buildOpenAISdkRequestOptions(model, firstEventAbort.signal, { timeoutMs: options?.timeoutMs }),
					...await encodeBody(params)
				}).withResponse();`,
    encoded: true,
  },
];

globalThis[modelGate] = async () => {
  throw Error("AW OpenClaw model result gate is not ready");
};
globalThis[payloadGate] = async () => {
  throw Error("AW OpenClaw provider payload gate is not ready");
};
for (const { core, coreDigest, resolver, resolverDigest, resolveFunction } of profiles) {
  installVerifiedPatch({
    target: `/node_modules/openclaw/dist/${resolver}`,
    digest: resolverDigest,
    anchor: `function ${resolveFunction}(params) {`,
    replacement: `function ${resolveFunction}(params) {
      if (params.providerStreamFn) throw Error("AW final gate rejects a Provider stream override");
      const selected = awResolveVerifiedStream(params);
      if (selected.strategy !== "boundary-aware:openai-completions")
        throw Error("AW final gate requires the verified boundary-aware stream");
      return selected;
    }
    function awResolveVerifiedStream(params) {`,
    onPatched: () => { globalThis[Symbol.for("aw.openclaw.streamPatched")] = true; },
  });
  installVerifiedPatch({
    target: `/node_modules/openclaw/dist/${core}`,
    digest: coreDigest,
    anchor: modelAnchor,
    replacement: `return await globalThis[Symbol.for("aw.openclaw.modelGate")](messages, config, projectionSignal, normalizeCoreContextMessages);`,
    onPatched: () => { globalThis[patched] = true; },
  });
}
removeOwnPreload(import.meta.url);
installVerifiedPatch({
  target: "/node_modules/@openclaw/ai/dist/transports.mjs",
  variants: profiles.map(({ transportDigest, transportAnchor, encoded }) => ({
    digest: transportDigest,
    anchor: transportAnchor,
    replacement: transportAnchor
      .replace("const nextParams =", "const awBeforePayload = structuredClone(params);\n\t\t\t\tconst nextParams =")
      .replace("if (getCompat(model)", `params = await globalThis[Symbol.for(\"aw.openclaw.payloadGate\")](params, options?.sessionId, awBeforePayload, context.messages, model.api${encoded ? ", encodeBody" : ", undefined"}, options?.signal);\n\t\t\t\tif (getCompat(model)`)
      .replace("...await encodeBody(params)", 'body: JSON.stringify(params), headers: { "content-type": "application/json" }'),
  })),
  onPatched: () => { globalThis[providerPatched] = true; },
});
