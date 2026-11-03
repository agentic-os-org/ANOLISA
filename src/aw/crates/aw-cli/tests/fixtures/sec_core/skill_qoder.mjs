/** Drive actual Skill calls through an unmodified SDK/Worker and the AW factories. */
import { readFile, writeFile, unlink } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { join } from "node:path";
const bridge = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const root = process.cwd();
const current = JSON.parse(await readFile(join(root, "case.json"), "utf8"));
const daemon = await bridge.startDaemon({ auth: { type: "accessToken",
  accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" } } });
try {
  const dynamic = current.skill === "dynamic-read";
  let query;
  query = await bridge.createQuery({
    prompt: `${dynamic ? `First use Read on ${join(root, "nested/readme.txt")}. Then ` : ""}Invoke the Skill named "${current.skill}" exactly once. Stop after its result. Never retry. If denied, report DENIED.`,
    options: { daemon, cwd: root, tools: dynamic ? ["Read", "Skill"] : ["Skill"], model: "auto", maxTurns: dynamic ? 3 : 2,
      ...(dynamic ? { hooks: { PostToolUse: [{ matcher: "Read", hooks: [async () => {
        const before = await query.reloadSkills();
        await unlink(join(root, "nested/.qoder/skills/dynamic-read/SKILL.md"));
        const after = await query.reloadSkills();
        await writeFile(join(root, "skill-catalog.json"), JSON.stringify([before, after]));
        return {};
      }] }] } } : {}),
      permissionMode: "bypassPermissions", settingSources: current.skill === "project-only" ?
        ["project"] : ["user", "project", "local"] },
  });
  const messages = [];
  try {
    if (current.skill === "model-only" || current.skill.startsWith("workflow-"))
      await writeFile(join(root, "skill-catalog.json"), JSON.stringify(await query.reloadSkills()));
    for await (const message of query) messages.push(message);
  }
  finally { await query.close(); }
  await writeFile(join(root, "messages.json"), JSON.stringify(messages));
} finally { await daemon.stop(); }
