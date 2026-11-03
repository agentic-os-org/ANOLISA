/** Local catalog resolution follows the pinned Worker's name and source rules. */
import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { resolveSkill, bindSkillQuery, noteSkillRead } from "../../../../aw-adapter/native/qoder-sdk/skills.mjs";

async function fixture(run) {
  const root = await mkdtemp(join(tmpdir(), "aw-skill-"));
  const cwd = join(root, "project");
  await mkdir(cwd);
  const user = join(root, "config");
  const payload = () => ({ hook_event_name: "PreToolUse", tool_name: "Skill",
    cwd, transcript_path: join(user, "projects", "project", "session.jsonl"), tool_input: { skill: "example" } });
  const query = { async request(input) { return input.type === "get_settings" ?
    { effective: {}, sources: {} } : { skills: [{ name: "example" }] }; } };
  const skill = async (path, body = "instructions") => {
    await mkdir(path, { recursive: true });
    await writeFile(join(path, "SKILL.md"), `---\nname: display-only\ndescription: fixture\n---\n${body}\n`);
  };
  try { await run({root, cwd, user, payload, query, skill}); }
  finally { await rm(root, { recursive: true, force: true }); }
}

test("Qoder refuses same-name assets across enabled sources", async () => fixture(async f => {
  await f.skill(join(f.cwd, ".qoder/skills/example"), "project");
  const expected = join(f.user, "skills/example");
  await f.skill(expected, "user");
  const payload = f.payload();
  assert.equal(await resolveSkill(payload, {}, f.query), undefined);
  assert.deepEqual(payload.aw_skill, { error_code: "skill_resolution_error" });
  const verify = await resolveSkill(payload, { settingSources: ["user"] }, f.query);
  assert.deepEqual(payload.aw_skill, { name: "example", directory: expected, source: "user" });
  await verify();
}));

test("native-unloadable user assets cannot authorize a project fallback", async () => fixture(async f => {
  await f.skill(join(f.cwd, ".qoder/skills/example"), "unchecked project instructions");
  const user = join(f.user, "skills/example");
  await f.skill(user);
  for (const bytes of ["---\n\n---\n", "---\n# empty metadata\n---\n", "---\nnull\n---\n", "---\nscalar\n---\n", "", " \n"]) {
    await writeFile(join(user, "SKILL.md"), bytes);
    const payload = f.payload();
    assert.equal(await resolveSkill(payload, {}, f.query), undefined);
    assert.deepEqual(payload.aw_skill, { error_code: "skill_resolution_error" });
  }
}));

test("Qoder resolves nested leaf names and case-insensitive calls", async () => fixture(async f => {
  const expected = join(f.cwd, ".qoder/skills/nested/Example");
  await f.skill(expected);
  const payload = f.payload();
  await resolveSkill(payload, {}, f.query);
  assert.equal(payload.aw_skill.directory, expected);
}));

test("Read history fences retained dynamic Skills across refresh and verification", async () => fixture(async f => {
  await f.skill(join(f.user, "skills/example"));
  const initial = f.payload();
  const verify = await resolveSkill(initial, {}, f.query);
  noteSkillRead({ hook_event_name: "PreToolUse", tool_name: "Read" }, f.query);
  await assert.rejects(verify());
  // A deleted file or an empty reload listing cannot prove that the native
  // dynamic object has left the cache. Only a new Query has a fresh history.
  for (let i = 0; i < 2; i++) {
    await f.query.request({ type: "reload_skills" });
    const payload = f.payload();
    await resolveSkill(payload, {}, f.query);
    assert.deepEqual(payload.aw_skill, { error_code: "skill_resolution_error" });
  }
  const userOnly = f.payload();
  await (await resolveSkill(userOnly, { settingSources: ["user"] }, f.query))();
  const fresh = { request: f.query.request };
  await (await resolveSkill(f.payload(), {}, fresh))();
}));

test("local Workflow metadata cannot borrow a resource Skill signature", async () => {
  for (const source of ["user", "project"]) {
    for (const entry of ["unrelated.js", "bundle/workflow.js", "bundle/index.js"]) {
      await fixture(async f => {
        const directory = join(f.user, "skills/example");
        await f.skill(directory, "signed user instructions");
        const base = source === "user" ? f.user : join(f.cwd, ".qoder");
        const path = join(base, "workflows", entry);
        await mkdir(join(path, ".."), { recursive: true });
        for (const [name, bytes] of [["Example", "signed user instructions"], ["example", "---\n\n---\n"]]) {
          await writeFile(join(directory, "SKILL.md"), bytes);
          await writeFile(path, `export const meta = { name: "${name}", description: "UNCHECKED_WORKFLOW_INSTRUCTIONS" }; return {};`);
          const payload = f.payload();
          assert.equal(await resolveSkill(payload, {}, f.query), undefined);
          assert.deepEqual(payload.aw_skill, { error_code: "skill_resolution_error" });
        }
      });
    }
  }
});

test("Workflow fencing follows enabled sources and native entrypoint discovery", async () => fixture(async f => {
  const directory = join(f.user, "skills/example");
  await f.skill(directory);
  const root = join(f.cwd, ".qoder/workflows");
  await mkdir(join(root, "bundle/deeper"), { recursive: true });
  for (const file of ["README.md", ".hidden.js", "bundle/other.js", "bundle/deeper/index.js"]) {
    await writeFile(join(root, file), "ignored by native Workflow discovery");
  }
  await (await resolveSkill(f.payload(), {}, f.query))();
  await writeFile(join(root, "arbitrary.js"), "export const meta = {name: 'Example', description: 'alias'}; return {};");
  const payload = f.payload();
  await (await resolveSkill(payload, { settingSources: ["user"] }, f.query))();
  assert.equal(payload.aw_skill.directory, directory);
}));

test("new Workflow entrypoints during refresh or Provider checking block loading", async () => fixture(async f => {
  await f.skill(join(f.user, "skills/example"));
  const path = join(f.cwd, ".qoder/workflows/alias.js");
  await mkdir(join(path, ".."), { recursive: true });
  const verify = await resolveSkill(f.payload(), {}, f.query);
  await writeFile(path, "export const meta = {name: 'Example', description: 'late workflow'}; return {};");
  await assert.rejects(verify());
  await rm(path);
  const query = { async request(input) {
    if (input.type === "get_settings") return { effective: {} };
    await writeFile(path, "export const meta = {name: 'Example', description: 'refresh workflow'}; return {};");
    return { skills: [] };
  } };
  const payload = f.payload();
  await resolveSkill(payload, {}, query);
  assert.deepEqual(payload.aw_skill, { error_code: "skill_resolution_error" });
}));

test("Qoder refuses duplicate leaf names within a source", async () => fixture(async f => {
  await f.skill(join(f.cwd, ".qoder/skills/example"));
  await f.skill(join(f.cwd, ".qoder/skills/nested/example"));
  const payload = f.payload();
  await resolveSkill(payload, {}, f.query);
  assert.equal(payload.aw_skill.error_code, "skill_resolution_error");
}));

test("Qoder rejects case collisions across all enabled catalogs", async () => {
  for (const [userName, projectName, projectCatalog] of [
    ["example", "Example", ".qoder"], ["Example", "example", ".qoder"],
    ["example", "Example", ".agents"],
  ]) await fixture(async f => {
    await f.skill(join(f.user, "skills", userName), "signed user instructions");
    await f.skill(join(f.cwd, projectCatalog, "skills", projectName), "unchecked project instructions");
    const query = { async request(input) { return input.type === "get_settings" ?
      { effective: {}, sources: { workspace: { skills: { loadFromAgentsDirectory: true } } } } :
      { skills: [{ name: projectName }, { name: userName }] }; } };
    const payload = f.payload();
    assert.equal(await resolveSkill(payload, {}, query), undefined);
    assert.deepEqual(payload.aw_skill, { error_code: "skill_resolution_error" });
    const filtered = f.payload();
    const projectQuery = { async request(input) { return input.type === "get_settings" ?
      await query.request(input) : { skills: [{ name: projectName }] }; } };
    const verify = await resolveSkill(filtered, { settingSources: ["project"] }, projectQuery);
    assert.equal(filtered.aw_skill.directory, join(f.cwd, projectCatalog, "skills", projectName));
    await verify();
  });
});

test("Qoder restricts Skill catalogs to the Query settingSources", async () => fixture(async f => {
  const project = join(f.cwd, ".qoder/skills/example");
  const user = join(f.user, "skills/example");
  await f.skill(project, "unchecked project instructions");
  await f.skill(user, "signed user instructions");
  for (const [sources, directory] of [
    [["project"], project], [["project", "local"], project], [["user"], user],
    [["user", "project"], undefined], [undefined, undefined], [[], undefined], [["local"], undefined],
  ]) {
    const payload = f.payload();
    const verify = await resolveSkill(payload, { settingSources: sources }, f.query);
    assert.equal(payload.aw_skill.directory, directory);
    if (directory) await verify();
    else assert.deepEqual(payload.aw_skill, { error_code: "skill_resolution_error" });
  }
  for (const flag of ["setting-sources", "settingSources", "add-dir", "addDir", "enable-builtin-skills", "enableBuiltinSkills"]) {
    const payload = f.payload();
    await resolveSkill(payload, { extraArgs: { [flag]: "project" } }, f.query);
    assert.deepEqual(payload.aw_skill, { error_code: "skill_resolution_error" });
  }
}));

test("Qoder missing and invalid selections remain resolution errors", async () => fixture(async f => {
  for (const name of ["example", "../example", "plugin:example", null, ""]) {
    const payload = f.payload();
    payload.tool_input.skill = name;
    await resolveSkill(payload, {}, f.query);
    assert.equal(payload.aw_skill.error_code, "skill_resolution_error");
  }
  const unrelated = { tool_name: "Bash" };
  await resolveSkill(unrelated, {}, f.query);
  assert.equal(unrelated.aw_skill, undefined);
}));

test("Qoder rejects source edits during reload and after backend checking", async () => fixture(async f => {
  const directory = join(f.cwd, ".qoder/skills/example");
  await f.skill(directory);
  const payload = f.payload();
  const verify = await resolveSkill(payload, {}, f.query);
  await writeFile(join(directory, "SKILL.md"), "changed");
  await assert.rejects(verify());
  const query = { async request(input) {
    if (input.type === "get_settings") return { effective: {} };
    await writeFile(join(directory, "SKILL.md"), "edited while reloading");
    return { skills: [{ name: "example" }] };
  } };
  const edited = f.payload();
  await resolveSkill(edited, {}, query);
  assert.equal(edited.aw_skill.error_code, "skill_resolution_error");
}));

test("Qoder requires cache reload success and reports control faults", async () => fixture(async f => {
  await f.skill(join(f.cwd, ".qoder/skills/example"));
  for (const query of [undefined, { request: async () => { throw Error("timeout"); } },
    { request: async () => ({ effective: {} }) },
    { request: async () => ({ effective: {}, skills: null }) }]) {
    const payload = f.payload();
    await resolveSkill(payload, {}, query);
    assert.equal(payload.aw_skill.error_code, "skill_resolution_error");
  }
}));

test("model-only Skills can load without a displayable entry", async () => fixture(async f => {
  const directory = join(f.cwd, ".qoder/skills/example");
  await f.skill(directory);
  await writeFile(join(directory, "SKILL.md"),
    "---\nname: example\ndescription: Model-only fixture\nuser-invocable: false\n---\nModel instructions.\n");
  for (const skills of [[], [{ name: "unrelated-displayable-skill" }]]) {
    const query = { async request(input) { return input.type === "get_settings" ?
      { effective: {}, sources: {} } : { skills }; } };
    const payload = f.payload();
    const verify = await resolveSkill(payload, {}, query);
    assert.deepEqual(payload.aw_skill, { name: "example", directory, source: "project" });
    await verify();
  }
}));

test("Qoder only enables agents catalogs from explicit native source settings", async () => fixture(async f => {
  const expected = join(f.cwd, ".agents/skills/example");
  await f.skill(expected);
  const missing = f.payload();
  await resolveSkill(missing, {}, f.query);
  assert.equal(missing.aw_skill.error_code, "skill_resolution_error");
  const query = { async request(input) { return input.type === "get_settings" ?
    {effective: {}, sources: {flag: {skills: {loadFromAgentsDirectory: true}}}} :
    {skills: [{name: "example"}]}; } };
  const payload = f.payload();
  await resolveSkill(payload, {}, query);
  assert.equal(payload.aw_skill.directory, expected);
}));

test("Qoder reads workspace enablement in native settings priority order", async () => fixture(async f => {
  const expected = join(f.cwd, ".agents/skills/example");
  await f.skill(expected);
  for (const [values, enabled] of [
    [{ workspace: true }, true], [{ workspace: true, user: false }, true],
    [{ workspace: false, user: true }, false], [{ local: false, workspace: true }, false],
    [{ flag: false, local: true, workspace: true }, false],
    [{ flag: true, local: false, workspace: false }, true],
  ]) {
    const sources = Object.fromEntries(Object.entries(values).map(([source, value]) =>
      [source, { skills: { loadFromAgentsDirectory: value } }]));
    const query = { async request(input) { return input.type === "get_settings" ?
      { effective: { skills: { loadFromAgentsDirectory: true } }, sources } :
      { skills: [{ name: "example" }] }; } };
    const payload = f.payload();
    const verify = await resolveSkill(payload, {}, query);
    if (enabled) {
      assert.equal(payload.aw_skill.directory, expected);
      await verify();
    } else assert.deepEqual(payload.aw_skill, { error_code: "skill_resolution_error" });
  }
}));

test("runtime directory grants retain native results and fence Skill checking", async () => fixture(async f => {
  await f.skill(join(f.cwd, ".qoder/skills/example"));
  const granted = { added: ["/native/directory"] };
  f.query.addDirectories = async () => granted;
  bindSkillQuery(f.query, true);
  const payload = f.payload();
  const verify = await resolveSkill(payload, {}, f.query);
  assert.equal(await f.query.addDirectories(["/native/directory"]), granted);
  await assert.rejects(verify());
  const later = f.payload();
  await resolveSkill(later, {}, f.query);
  assert.equal(later.aw_skill.error_code, "skill_resolution_error");
  const unselected = { addDirectories: () => granted };
  const original = unselected.addDirectories;
  bindSkillQuery(unselected, false);
  assert.equal(unselected.addDirectories, original);
}));
