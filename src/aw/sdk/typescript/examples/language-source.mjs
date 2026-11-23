/** Instrument an arbitrary Node/JS agent with zero dedicated Rust Adapter. */
import { LanguageSource, SourceDenied } from "@anolisa/aw-sdk/sources";

class CustomAgent {
  async ask(prompt) {
    return this.echo({ text: prompt });
  }
  async echo({ text }) {
    return `echo:${text}`;
  }
}

const agent = new CustomAgent();
const source = LanguageSource.create({
  endpoint: process.env.AW_SOCKET,
  target: "app",
  availability: "required",
});

try {
  await source.connect();
  await source.withSession({ sessionId: "example-js-agent" }, async (session) => {
    session.instrument(agent, {
      inputMethods: ["ask"],
      toolMethods: { echo: "echo" },
    });
    try {
      console.log(await agent.ask("hello"));
    } catch (error) {
      if (error instanceof SourceDenied) console.log(`AW denied ${error.event}`);
      else throw error;
    }
  });
} finally {
  await source.close();
}
