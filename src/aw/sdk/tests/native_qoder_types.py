"""Compile public core, source and adapter consumers against pinned Qoder SDK Query."""

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile


def pack(package: Path, out: Path) -> Path:
    out.mkdir(parents=True, exist_ok=True)
    packed = subprocess.run(
        ["npm", "pack", "--pack-destination", str(out)],
        cwd=package,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert packed.returncode == 0, packed.stderr
    (tarball,) = out.glob("*.tgz")
    return tarball


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--npm", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    sdk_root = Path(__file__).resolve().parents[1]
    args.output.mkdir(parents=True, exist_ok=True)
    sdk = args.npm / "node_modules/@qoder-ai/qoder-agent-sdk"
    compiler = args.npm / "node_modules/typescript"
    sdk_version = json.loads((sdk / "package.json").read_text())["version"]
    compiler_version = json.loads((compiler / "package.json").read_text())["version"]
    assert sdk_version == "1.0.56" and compiler_version == "5.9.3"
    with tempfile.TemporaryDirectory(prefix="aw-sdk-types-", dir="/tmp") as temporary:
        root = Path(temporary)
        core_tgz = pack(sdk_root / "typescript", root / "pack-core")
        qoder_tgz = pack(sdk_root / "typescript-adapters/qoder", root / "pack-qoder")
        pi_tgz = pack(sdk_root / "typescript-adapters/pi", root / "pack-pi")
        subprocess.run(
            [
                "npm",
                "install",
                "--prefix",
                str(root),
                "--offline",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                str(core_tgz),
                str(qoder_tgz),
                str(pi_tgz),
            ],
            check=True,
            capture_output=True,
            timeout=30,
        )
        consumer = root / "consumer.mts"
        consumer.write_text(
            "import { query } from " + json.dumps(str(sdk / "dist/index.js")) + ";\n"
            'import { Admission, Instance } from "@anolisa/aw-sdk";\n'
            'import { LanguageSource, SourceDenied, sourceCapabilities } from "@anolisa/aw-sdk/sources";\n'
            'import type { WrapToolOptions } from "@anolisa/aw-sdk/sources";\n'
            'import { runPiWorker } from "@anolisa/aw-sdk/pi";\n'
            'import { runPiWorker as directPiWorker } from "@anolisa/aw-sdk-pi";\n'
            'import { createQoderBridge, qoderOptions } from "@anolisa/aw-sdk/qoder";\n'
            'import { createQoderBridge as directQoderBridge, qoderOptions as directQoderOptions } from "@anolisa/aw-sdk-qoder";\n'
            "export async function consumer(instance: Instance) {\n"
            ' const bridge = await createQoderBridge({configFile:"/tmp/aw.yaml", query, availability:"native_fallback"});\n'
            ' const direct = await directQoderBridge({configFile:"/tmp/aw.yaml", query, availability:"native_fallback"});\n'
            ' const session = bridge.query({prompt:"text"});\n'
            " await session.initializationResult();\n"
            " await direct.close();\n"
            " const gate = new Admission(instance, {events:[\"tool.before\"], deferAcknowledgement:true});\n"
            " await gate.prepare();\n"
            " const task = await instance.task({deferAcknowledgement:true});\n"
            " await task.acknowledge();\n"
            ' const native = qoderOptions(task).bind(query({prompt:"text"}));\n'
            ' const directNative = directQoderOptions(task).bind(query({prompt:"text"}));\n'
            " await native.initializationResult();\n"
            " await directNative.initializationResult();\n"
            ' void runPiWorker({application:"/app/worker.mjs", configFile:"/tmp/aw.yaml", descriptorFile:"/tmp/d.json", automatic:true});\n'
            ' void directPiWorker({application:"/app/worker.mjs", configFile:"/tmp/aw.yaml", descriptorFile:"/tmp/d.json", automatic:false});\n'
            ' const source = LanguageSource.create({endpoint:"/tmp/aw.sock", target:"app", events:["input.submit","tool.before","tool.after"]});\n'
            " void sourceCapabilities();\n"
            " void SourceDenied;\n"
            ' const langSession = source.session({sessionId:"s1"});\n'
            ' const wrapped = langSession.wrapTool(async (x: { cmd: string }) => x.cmd, {name:"sh"});\n'
            ' void wrapped({cmd:"ok"});\n'
            ' class ToolInput { constructor(public command: string) {} }\n'
            ' const snapshotArg: WrapToolOptions<[ToolInput], string>["snapshotArg"] = (input) => new ToolInput(input.command);\n'
            ' function run(input: ToolInput) { return input.command; }\n'
            ' async function* runStream(input: ToolInput) { yield input.command; }\n'
            ' await langSession.wrapTool(run, {snapshotArg})(new ToolInput("go"));\n'
            ' for await (const chunk of langSession.wrapToolStream(runStream, {snapshotArg})(new ToolInput("go"))) chunk.toUpperCase();\n'
            ' langSession.instrument({run, runStream}, {toolMethods: {run: {snapshotArg}, runStream: {stream: true, snapshotArg}}}).restore();\n'
            ' // @ts-expect-error The callback must return a supported argument type.\n'
            ' langSession.wrapTool(run, {snapshotArg: (input) => input.command});\n'
            ' const agent = { async ask(prompt: string) { return prompt; }, async run(cmd: string) { return { ok: Boolean(cmd) }; } };\n'
            ' const inst = langSession.instrument(agent, { inputMethods: { ask: { extractText: (p: string) => p } }, toolMethods: { run: { name: "sh", extractInput: (c: string) => ({ cmd: c }), extractResult: (r: { ok: boolean }) => r.ok } } });\n'
            " inst.restore();\n"
            ' const streamTool = langSession.wrapToolStream(async function* (cmd: string) { yield cmd; }, { name: "st", extractInput: (c: string) => ({ cmd: c }), snapshotResult: (chunk: string) => chunk, extractResult: (items: string[]) => items.join(",") });\n'
            ' for await (const chunk of streamTool("go")) { void chunk; }\n'
            ' async function* chunks(text: string) { yield text; }\n'
            ' const factory = (text: string) => chunks(text);\n'
            ' const inputs = langSession.wrapInput(chunks)("go");\n'
            ' const tools = langSession.wrapTool(chunks)("go");\n'
            ' const factoryInput = langSession.wrapInput(factory)("go");\n'
            ' const factoryTool = langSession.wrapTool(factory)("go");\n'
            ' for (const result of [inputs, tools, factoryInput, factoryTool]) {\n'
            '   for await (const chunk of await result) chunk.toUpperCase();\n'
            ' }\n'
            ' // @ts-expect-error An input factory can take the Promise path.\n'
            ' for await (const chunk of factoryInput) void chunk;\n'
            ' // @ts-expect-error A tool factory can take the Promise path.\n'
            ' factoryTool.next();\n'
            ' // @ts-expect-error Native input generator wrappers have no Promise methods.\n'
            ' inputs.then();\n'
            ' // @ts-expect-error Native tool generator wrappers have no Promise methods.\n'
            ' tools.then();\n'
            ' for await (const chunk of langSession.wrapInputStream(factory)("go")) chunk.toUpperCase();\n'
            ' for await (const chunk of langSession.wrapTool(factory, {stream:true})("go")) chunk.toUpperCase();\n'
            ' function* syncStream() { yield { text: "ok" }; }\n'
            ' const syncWrapped = langSession.wrapTool(syncStream, { bufferStream: true, snapshotResult: (chunk) => ({ text: chunk.text }) });\n'
            ' for (const chunk of await syncWrapped()) chunk.text.toUpperCase();\n'
            ' langSession.instrument({ syncStream }, { toolMethods: { syncStream: { bufferStream: true, snapshotResult: (chunk) => ({ text: chunk.text }) } } }).restore();\n'
            ' const syncIter = (): Iterator<{ text: string }> => ({ next: () => ({ done: true, value: undefined }) });\n'
            ' void langSession.wrapTool(syncIter, { bufferStream: true, snapshotResult: (chunk) => ({ text: chunk.text }) });\n'
            ' const syncIterable = (): Iterable<{ text: string }> => ({ [Symbol.iterator]: () => syncIter() });\n'
            ' void langSession.wrapTool(syncIterable, { bufferStream: true, snapshotResult: (chunk) => ({ text: chunk.text }) });\n'
            ' const dualSync = (): Iterator<{ text: string }> & Iterable<number> => ({ next: () => ({ done: true, value: undefined }), [Symbol.iterator]: () => [1][Symbol.iterator]() });\n'
            ' void langSession.wrapTool(dualSync, { bufferStream: true, snapshotResult: (chunk) => ({ text: chunk.text }) });\n'
            ' langSession.instrument({ dualSync }, { toolMethods: { dualSync: { bufferStream: true, snapshotResult: (chunk) => ({ text: chunk.text }) } } }).restore();\n'
            ' // @ts-expect-error Dual Iterator & Iterable prefers direct next() over Symbol.iterator.\n'
            ' langSession.wrapTool(dualSync, { bufferStream: true, snapshotResult: (chunk) => { void chunk.toFixed(); return chunk; } });\n'
            ' // @ts-expect-error Instrument also prefers direct next() over Symbol.iterator.\n'
            ' langSession.instrument({ dualSync }, { toolMethods: { dualSync: { bufferStream: true, snapshotResult: (chunk) => { void chunk.toFixed(); return chunk; } } } }).restore();\n'
            ' function* typedGen(): Generator<{ text: string }, string, number> { yield { text: "ok" }; return "done"; }\n'
            ' void langSession.wrapTool(typedGen, { bufferStream: true, snapshotResult: (chunk) => ({ text: chunk.text }) });\n'
            ' void langSession.wrapTool(typedGen, { bufferStream: true, extractResult: (chunks) => chunks.map((c) => c.text), applyResult: (chunks, replaced) => [...chunks, { text: String(replaced) }] });\n'
            ' langSession.instrument({ typedGen }, { toolMethods: { typedGen: { bufferStream: true, extractResult: (chunks) => chunks.map((c) => c.text), applyResult: (chunks, replaced) => [...chunks, { text: String(replaced) }] } } }).restore();\n'
            ' const asyncSyncGen = async (): Promise<Generator<{ text: string }, string, number>> => typedGen();\n'
            ' void langSession.wrapTool(asyncSyncGen, { bufferStream: true, extractResult: (chunks) => chunks.map((c) => c.text), applyResult: (chunks, replaced) => [...chunks, { text: String(replaced) }] });\n'
            ' void langSession.wrapTool(syncIter, { bufferStream: true, extractResult: (chunks) => chunks.map((c) => c.text), applyResult: (chunks, replaced) => [...chunks, { text: String(replaced) }] });\n'
            ' void langSession.wrapTool(syncIterable, { bufferStream: true, extractResult: (chunks) => chunks.map((c) => c.text), applyResult: (chunks, replaced) => [...chunks, { text: String(replaced) }] });\n'
            ' void langSession.wrapTool(dualSync, { bufferStream: true, extractResult: (chunks) => chunks.map((c) => c.text), applyResult: (chunks, replaced) => [...chunks, { text: String(replaced) }] });\n'
            ' langSession.instrument({ syncIter, syncIterable, dualSync }, { toolMethods: { syncIter: { bufferStream: true, extractResult: (chunks) => chunks.map((c) => c.text), applyResult: (chunks, replaced) => [...chunks, { text: String(replaced) }] }, syncIterable: { bufferStream: true, extractResult: (chunks) => chunks.map((c) => c.text), applyResult: (chunks, replaced) => [...chunks, { text: String(replaced) }] }, dualSync: { bufferStream: true, extractResult: (chunks) => chunks.map((c) => c.text), applyResult: (chunks, replaced) => [...chunks, { text: String(replaced) }] } } }).restore();\n'
            ' // @ts-expect-error Buffered sync stream extractResult receives chunk array, not Generator.\n'
            ' langSession.wrapTool(typedGen, { bufferStream: true, extractResult: (chunks) => chunks.next(1) });\n'
            ' // @ts-expect-error Instrument buffered sync stream extractResult receives chunk array, not Generator.\n'
            ' langSession.instrument({ typedGen }, { toolMethods: { typedGen: { bufferStream: true, extractResult: (chunks) => chunks.next(1) } } }).restore();\n'
            ' // Non-buffered sync stream keeps the original Generator signature.\n'
            ' void langSession.wrapTool(typedGen, { extractResult: (gen) => gen.next(1) });\n'
            ' void langSession.wrapTool(typedGen, { bufferStream: false, extractResult: (gen) => gen.next(1) });\n'
            ' langSession.instrument({ typedGen }, { toolMethods: { typedGen: { bufferStream: false, extractResult: (gen) => gen.next(1) } } }).restore();\n'
            ' langSession.instrument({ typedGen }, { toolMethods: { typedGen: { extractResult: (gen) => gen.next(1) } } }).restore();\n'
            ' // @ts-expect-error Non-buffered instrument extractResult does not accept chunk array.\n'
            ' langSession.instrument({ typedGen }, { toolMethods: { typedGen: { extractResult: (chunks: { text: string }[]) => chunks.map((c) => c.text) } } }).restore();\n'
            ' // @ts-expect-error Non-buffered wrapTool extractResult does not accept chunk array methods.\n'
            ' langSession.wrapTool(typedGen, { extractResult: (chunks) => chunks.map((c: { text: string }) => c.text) });\n'
            ' langSession.instrument({ typedGen }, { toolMethods: { typedGen: { snapshotResult: (chunk) => ({ text: chunk.text }) } } }).restore();\n'
            ' // Dynamic boolean bufferStream works for both ordinary objects and sync generators.\n'
            ' const dynamicBuffer = Boolean(Math.random());\n'
            ' const plainObjTool = () => ({ text: "ok" });\n'
            ' void langSession.wrapTool(plainObjTool, { bufferStream: dynamicBuffer });\n'
            ' void langSession.wrapTool(plainObjTool, { bufferStream: dynamicBuffer, extractResult: (r) => r.text, applyResult: (r) => ({ text: r.text }) });\n'
            ' void langSession.wrapTool(typedGen, { bufferStream: dynamicBuffer });\n'
            ' void langSession.wrapTool(typedGen, { bufferStream: dynamicBuffer, extractResult: (r) => (Array.isArray(r) ? r.map((c) => c.text) : r.next(1)) });\n'
            ' langSession.instrument({ plainObjTool, typedGen }, { toolMethods: { plainObjTool: { bufferStream: dynamicBuffer, extractResult: (r) => r.text }, typedGen: { bufferStream: dynamicBuffer, extractResult: (r) => (Array.isArray(r) ? r.map((c) => c.text) : r.next(1)) } } }).restore();\n'
            ' const explicitToolMethods: import("@anolisa/aw-sdk/sources").ToolMethodMap<{ plainObjTool: typeof plainObjTool; typedGen: typeof typedGen }> = { plainObjTool: { bufferStream: dynamicBuffer }, typedGen: { bufferStream: dynamicBuffer } };\n'
            ' langSession.instrument({ plainObjTool, typedGen }, { toolMethods: explicitToolMethods }).restore();\n'
            ' // @ts-expect-error Sync stream snapshotResult receives a chunk, not the Generator.\n'
            ' langSession.wrapTool(syncStream, { bufferStream: true, snapshotResult: (chunk) => { void chunk.next; return chunk; } });\n'
            ' void langSession.wrapTool(() => "ok", { snapshotResult: (s) => s.trim() });\n'
            ' void langSession.wrapTool(() => ["a"], { snapshotResult: (arr) => arr.map((x) => x.toUpperCase()) });\n'
            ' void langSession.wrapTool(() => new Map<string, number>(), { snapshotResult: (m) => { void m.get("a"); return new Map(m.entries()); } });\n'
            ' void langSession.wrapTool(() => new Set<string>(), { snapshotResult: (s) => { void s.has("a"); return new Set(s); } });\n'
            ' void langSession.wrapTool(() => new Uint8Array(2), { snapshotResult: (b) => { void b.byteLength; return new Uint8Array(b); } });\n'
            ' const eagerTarget = { str: () => "ok", arr: () => ["a"], map: () => new Map<string, number>(), set: () => new Set<string>(), bytes: () => new Uint8Array(2) };\n'
            ' void langSession.wrapTool(eagerTarget.arr, { bufferStream: true, extractResult: (arr) => arr.map((x) => x.toUpperCase()), applyResult: (arr) => arr.slice() });\n'
            ' void langSession.wrapTool(eagerTarget.map, { bufferStream: true, extractResult: (m) => m.get("a"), applyResult: (m) => new Map(m.entries()) });\n'
            ' langSession.instrument(eagerTarget, { toolMethods: { str: { bufferStream: true, extractResult: (s) => s.trim(), applyResult: (s) => s.toUpperCase() }, arr: { bufferStream: true, extractResult: (a) => a.map((x) => x.toUpperCase()), applyResult: (a) => a.slice() }, map: { bufferStream: true, extractResult: (m) => m.get("a"), applyResult: (m) => new Map(m.entries()) }, set: { bufferStream: true, extractResult: (s) => s.has("a"), applyResult: (s) => new Set(s) }, bytes: { bufferStream: true, extractResult: (b) => b.byteLength, applyResult: (b) => new Uint8Array(b) } } }).restore();\n'
            " await langSession.close({timeoutMs: 5000});\n"
            " await source.close({closeInstance: true, timeoutMs: 5000});\n"
            " return session;\n}\n"
        )
        result = subprocess.run(
            [
                "node",
                str(compiler / "bin/tsc"),
                "--strict",
                "--noEmit",
                "--skipLibCheck",
                "--module",
                "nodenext",
                "--target",
                "es2022",
                "--lib",
                "es2022,dom",
                str(consumer),
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        (args.output / "tsc.log").write_text(result.stdout + result.stderr)
        shutil.copyfile(consumer, args.output / "consumer.mts")
        assert result.returncode == 0, result.stdout + result.stderr
    (args.output / "result.json").write_text(
        json.dumps(
            {
                "passed": True,
                "sdk": sdk_version,
                "typescript": compiler_version,
            }
        )
        + "\n"
    )


if __name__ == "__main__":
    main()
