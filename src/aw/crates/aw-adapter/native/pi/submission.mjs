/** Text-only factory submissions retain native text and reject alternate ingress. */
import { isDeepStrictEqual } from "node:util";

export function inputBoundary(session) {
  let dispatch = null;
  const reject = () => { throw new Error("AW Pi input.submit requires session.prompt text input"); };
  const own = (object, key, value) => Object.defineProperty(object, key,
    { value, configurable: false, writable: false });
  const violation = (kind) => {
    const error = new Error(`AW Pi input.submit forbids changing ${kind} after scanning`);
    if (dispatch) dispatch.error ??= error;
    throw error;
  };
  const userContent = messages => {
    if (!Array.isArray(messages)) violation("user content");
    return messages.filter(message => message?.role === "user").map(message =>
      typeof message.content === "string" ? [{ type: "text", text: message.content }] : message.content);
  };
  for (const key of ["transformContext", "convertToLlm", "onPayload"]) {
    const callback = session.agent[key];
    own(session.agent, key, typeof callback !== "function" ? callback : async function (input, ...args) {
      if (!dispatch?.used) reject();
      if (dispatch.error) throw dispatch.error;
      // Isolate in-place edits and retained callback references from native state/transport.
      const before = structuredClone(input);
      const candidate = structuredClone(input);
      const result = await callback.call(this, candidate, ...args);
      const output = structuredClone(key === "onPayload" && result === undefined ? candidate : result);
      const payload = key === "onPayload";
      if (!isDeepStrictEqual(payload ? before : userContent(before), payload ? output : userContent(output)))
        violation(payload ? "provider payload" : "user content");
      return output;
    });
  }
  for (const key of ["steer", "followUp", "sendUserMessage", "sendCustomMessage"])
    own(session, key, reject);
  for (const key of ["steer", "followUp"])
    own(session.agent, key, reject);
  const nativePrompt = session.agent.prompt.bind(session.agent);
  own(session.agent, "prompt", async (messages, ...args) => {
    // Pi 1.3 prepends its system message; the submitted user text must still be exact.
    const userIndex = messages?.[0]?.role === "system" ? 1 : 0;
    const content = messages?.[userIndex]?.content;
    if (!dispatch || dispatch.used || !Array.isArray(messages) ||
        messages[userIndex]?.role !== "user" || content?.length !== 1 ||
        content[0]?.type !== "text" || content[0].text !== dispatch.text ||
        messages.slice(userIndex + 1).some(message =>
          message.role === "user" || message.role === "system")) reject();
    dispatch.used = true;
    dispatch.users = structuredClone(userContent([...(session.agent.state?.messages ?? []), ...messages]));
    return await nativePrompt(messages, ...args);
  });
  const nativeContinue = session.agent.continue.bind(session.agent);
  own(session.agent, "continue", (...args) => {
    if (!dispatch?.used) reject();
    return nativeContinue(...args);
  });
  return {
    model(context, options) {
      if (!dispatch?.used) reject();
      if (dispatch.error) throw dispatch.error;
      if (!isDeepStrictEqual(userContent(context.messages), dispatch.users)) violation("user content");
      if (options?.onPayload !== session.agent.onPayload) violation("provider payload callback");
      return { ...context, messages: structuredClone(context.messages) };
    },
    validate(text, options, skill = false) {
      if (typeof text !== "string" || (options !== undefined &&
          (!options || typeof options !== "object" || Array.isArray(options)))) reject();
      if (options?.images !== undefined &&
          (!Array.isArray(options.images) || options.images.length)) reject();
      if (options?.source !== undefined && options.source !== "interactive") reject();
      if ((options?.streamingBehavior !== undefined &&
          !["steer", "followUp"].includes(options.streamingBehavior)) || session.isStreaming ||
          session.agent.hasQueuedMessages()) reject();
      // Context and serialized-payload hooks can rewrite text after Agent.prompt.
      for (const event of ["input", "context", "before_provider_request"]) {
        if (session.hasExtensionHandlers(event))
          throw new Error(`AW Pi input.submit does not support ${event} extension handlers`);
      }
      if (!skill && (options?.expandPromptTemplates ?? true) && text.startsWith("/"))
        throw new Error("AW Pi input.submit requires literal text; disable prompt expansion for slash text");
    },
    async run(text, body) {
      dispatch = { text, used: false };
      try {
        const value = await body();
        // Pi may expose callback failures as assistant errors instead of rejecting prompt.
        if (dispatch.error) throw dispatch.error;
        return value;
      }
      finally { dispatch = null; }
    },
  };
}
