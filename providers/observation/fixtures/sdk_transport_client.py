"""Real Python SDK calls to the synthetic local model, without AW or native hooks."""

import asyncio
import json
import sys


def main() -> None:
    kind, endpoint, certificate = sys.argv[1:]
    if kind == "langchain":
        import httpx
        from langchain_openai import ChatOpenAI

        client = ChatOpenAI(
            model="fixture",
            api_key="synthetic-fixture-key",
            base_url=endpoint + "/v1",
            http_client=httpx.Client(verify=certificate, trust_env=False),
            max_retries=0,
        )
        print(json.dumps({"ready": True}), flush=True)
        input()
        response = client.invoke("SYNTHETIC_TRANSPORT_PROBE")
        print(json.dumps({"completed": response.content == "FIXTURE_OK"}), flush=True)
        input()
    else:
        from claude_agent_sdk import ClaudeAgentOptions, ClaudeSDKClient, ResultMessage

        async def run() -> None:
            options = ClaudeAgentOptions(
                model="claude-sonnet-4-6",
                max_turns=1,
                setting_sources=[],
                tools=[],
                env={
                    "ANTHROPIC_BASE_URL": endpoint,
                    "ANTHROPIC_API_KEY": "synthetic-fixture-key",
                    "NODE_EXTRA_CA_CERTS": certificate,
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                },
            )
            async with ClaudeSDKClient(options=options) as client:
                print(json.dumps({"ready": True}), flush=True)
                await asyncio.to_thread(input)
                await client.query("SYNTHETIC_TRANSPORT_PROBE")
                results = [
                    item
                    async for item in client.receive_response()
                    if isinstance(item, ResultMessage)
                ]
                print(
                    json.dumps({"completed": bool(results) and not results[-1].is_error}),
                    flush=True,
                )
                await asyncio.to_thread(input)

        asyncio.run(run())


if __name__ == "__main__":
    main()
