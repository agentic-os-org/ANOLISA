/** Project model-produced identities without retaining completed native call history. */
import { randomUUID } from "node:crypto";

export function modelProducer(scope) {
  const pending = new Map();
  return {
    claim(context) {
      const source = pending.get(context.toolCall.id);
      if (!source || source.object !== context.toolCall ||
          source.name !== context.toolCall.name ||
          source.arguments !== JSON.stringify(context.toolCall.arguments))
        throw new Error("AW rejected an unknown, duplicate or changed Pi model invocation");
      pending.delete(context.toolCall.id);
    },
    retire(callId) { pending.delete(callId); },
    clear() { pending.clear(); },
    wrap(stream) {
      return async (...args) => {
        const execution = scope.execution;
        scope.checkExecution(execution);
        const response = await stream(...args);
        const identities = new Map();
        let completion;
        const project = (message) => ({ ...message, content: message.content.map((block, index) => {
          if (block.type !== "toolCall") return block;
          if (!identities.has(index)) identities.set(index, randomUUID());
          return { ...block, id: identities.get(index) };
        }) });
        const result = () => {
          // Stream and result consumers must share the same produced object.
          if (!completion) completion = (async () => {
            try {
              const final = project(await response.result());
              // A late response cannot populate identities for a later prompt.
              scope.checkExecution(execution);
              if (final.stopReason === "toolUse") {
                const calls = final.content.filter(block => block.type === "toolCall");
                if (pending.size + scope.invocations.size + calls.length > scope.invocationLimit)
                  throw new Error("AW model invocation capacity exceeded");
                for (const call of calls)
                  pending.set(call.id, { object: call, name: call.name, arguments: JSON.stringify(call.arguments) });
              }
              return final;
            } catch (error) { scope.failed = true; throw error; }
          })();
          return completion;
        };
        return {
          result,
          async *[Symbol.asyncIterator]() {
            for await (const event of response) {
              if (event.type === "done" || event.type === "error") {
                const message = await result();
                yield { ...event, [event.type === "done" ? "message" : "error"]: message };
              } else {
                yield event.partial ? { ...event, partial: project(event.partial) } : event;
              }
            }
          },
        };
      };
    },
  };
}
