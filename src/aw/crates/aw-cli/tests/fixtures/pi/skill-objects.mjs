/** Actual Coding Agent loader selection and native expansion equivalence. */
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, symlinkSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { prepareSkill } from '../../../../aw-adapter/native/pi/skills.mjs';

const sdk = await import(pathToFileURL(join(resolve(process.argv[2]),
  'node_modules/@earendil-works/pi-coding-agent/dist/index.js')));
const root = mkdtempSync(join(tmpdir(), 'aw-pi-skill-object-'));
const cases = [];
const skill = (name, body, directory = name) => {
  const base = join(root, directory);
  mkdirSync(base, { recursive: true });
  writeFileSync(join(base, 'SKILL.md'), `---\nname: ${name}\ndescription: fixture\n---\n${body}\n`);
  return base;
};
try {
  const first = skill('native', 'FIRST', 'first');
  const duplicate = skill('native', 'DUPLICATE', 'second');
  const loader = new sdk.DefaultResourceLoader({ cwd: root, agentDir: join(root, 'agent'),
    settingsManager: sdk.SettingsManager.inMemory({}), noExtensions: true,
    noPromptTemplates: true, noThemes: true, noContextFiles: true,
    additionalSkillPaths: [first, duplicate] });
  await loader.reload();
  const session = { resourceLoader: loader, extensionRunner: { getCommand: () => undefined } };
  const selected = loader.getSkills().skills.find(value => value.name === 'native');
  assert(selected, 'actual loader did not select Skill');
  for (const text of ['/skill:native', '/skill:native  argument\n中文 ', '/skill:native\targ']) {
    const fixed = await prepareSkill(session, sdk, text);
    if (text.includes('\t')) {
      await assert.rejects(fixed.verify, /could not be validated/);
    } else {
      assert.equal(fixed.text, sdk.AgentSession.prototype._expandSkillCommand.call(session, text));
      assert.equal(fixed.metadata.aw_skill.directory, selected.baseDir);
      assert.equal(fixed.options.expandPromptTemplates, false);
      await fixed.verify();
    }
  }
  cases.push('native-selection-and-expansion');
  for (const content of ['---\nname: native\n---\n body \n', '\ufeff---\r\nname: native\r\n---\r\n中文\r\n']) {
    writeFileSync(selected.filePath, content);
    const fixed = await prepareSkill(session, sdk, '/skill:native args');
    assert.equal(fixed.text, sdk.AgentSession.prototype._expandSkillCommand.call(session, '/skill:native args'));
    await fixed.verify();
  }
  cases.push('native-parser-and-encoding');
  const snapshot = await prepareSkill(session, sdk, '/skill:native');
  writeFileSync(selected.filePath, 'CHANGED');
  await assert.rejects(snapshot.verify, /could not be validated/);
  assert(!snapshot.text.includes('CHANGED'));
  cases.push('changed-source-rejected');
  for (const text of ['ordinary', '/skill:native']) {
    const options = text.startsWith('/') ? { expandPromptTemplates: false } : undefined;
    const fixed = await prepareSkill({}, {}, text, options);
    assert.equal(fixed.text, text);
    assert.deepEqual(fixed.metadata, { aw_skill: null, aw_skill_request: null });
    await fixed.verify();
  }
  cases.push('explicit-no-skill');
  const conflict = await prepareSkill({ ...session, extensionRunner: { getCommand: () => ({}) } }, sdk, '/skill:native');
  await assert.rejects(conflict.verify, /could not be validated/);
  cases.push('extension-command-conflict');
  for (const text of ['/skill:missing', '/skill:']) {
    const fixed = await prepareSkill(session, sdk, text);
    await assert.rejects(fixed.verify, /could not be validated/);
  }
  cases.push('unknown-skill-rejected');
  const flat = join(root, 'flat.md');
  writeFileSync(flat, 'FLAT');
  const flatSession = { ...session, resourceLoader: { getSkills: () => ({ skills: [{ name: 'flat', baseDir: root, filePath: flat }] }) } };
  await assert.rejects((await prepareSkill(flatSession, sdk, '/skill:flat')).verify, /could not be validated/);
  cases.push('flat-file-rejected');
  writeFileSync(selected.filePath, 'x'.repeat(1024 * 1024 + 1));
  await assert.rejects((await prepareSkill(session, sdk, '/skill:native')).verify, /could not be validated/);
  cases.push('bounded-file');
  writeFileSync(selected.filePath, 'PINNED');
  const link = join(root, 'alias');
  symlinkSync(dirname(selected.filePath), link);
  const linked = { ...selected, baseDir: link, filePath: join(link, 'SKILL.md') };
  const linkSession = { ...session, resourceLoader: { getSkills: () => ({ skills: [linked] }) } };
  const fixed = await prepareSkill(linkSession, sdk, '/skill:native');
  assert.equal(fixed.metadata.aw_skill.directory, selected.baseDir);
  rmSync(link); symlinkSync(duplicate, link);
  await assert.rejects(fixed.verify, /could not be validated/);
  cases.push('symlink-target-change');
  const pinned = await prepareSkill(session, sdk, '/skill:native');
  session.resourceLoader = { getSkills: () => ({ skills: [] }) };
  await pinned.verify();
  assert(pinned.text.includes('PINNED'));
  assert.equal(pinned.options.expandPromptTemplates, false);
  cases.push('fixed-adoption-without-second-lookup');
  const report = JSON.stringify({ passed: true, case_count: cases.length, cases });
  if (process.argv[3]) {
    mkdirSync(dirname(process.argv[3]), { recursive: true });
    writeFileSync(process.argv[3], report);
  }
  console.log(report);
} finally { rmSync(root, { recursive: true, force: true }); }
