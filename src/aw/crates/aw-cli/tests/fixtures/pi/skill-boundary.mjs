/** Source pinning and explicit no-Skill metadata without external SDK packages. */
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rename, writeFile, rm, symlink, unlink } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { prepareSkill } from '../../../../aw-adapter/native/pi/skills.mjs';

test('selected source remains fixed and source changes prevent adoption', async () => {
  const root = await mkdtemp(join(tmpdir(), 'aw-pi-skill-'));
  try {
    const filePath = join(root, 'SKILL.md');
    await writeFile(filePath, 'original');
    const selected = { name: 'fixture', filePath, baseDir: root };
    const session = { resourceLoader: { getSkills: () => ({ skills: [selected] }) },
      extensionRunner: { getCommand: () => undefined } };
    const sdk = { stripFrontmatter: text => text };
    const fixed = await prepareSkill(session, sdk, '/skill:fixture argument');
    assert.equal(fixed.metadata.aw_skill.directory, root);
    assert.equal(fixed.metadata.aw_skill_request, 'fixture');
    assert.equal(fixed.options.expandPromptTemplates, false);
    assert(fixed.text.includes('original') && fixed.text.endsWith('argument'));
    await fixed.verify();
    await writeFile(filePath, 'modified');
    await assert.rejects(fixed.verify);
    const link = join(root, 'link');
    await symlink(root, link);
    selected.baseDir = link; selected.filePath = join(link, 'SKILL.md');
    const linked = await prepareSkill(session, sdk, '/skill:fixture');
    await linked.verify();
    await unlink(link);
    await assert.rejects(linked.verify);
    const missing = await prepareSkill(session, sdk, '/skill:unknown');
    assert.equal(missing.metadata.aw_skill.error_code, 'skill_resolution_error');
    await assert.rejects(missing.verify);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('only explicit ordinary or literal input skips source resolution', async () => {
  for (const [text, options] of [['hello'], ['/skill:fixture', { expandPromptTemplates: false }]]) {
    const fixed = await prepareSkill({}, {}, text, options);
    assert.deepEqual(fixed.metadata, { aw_skill: null, aw_skill_request: null });
    await fixed.verify();
  }
  const invalid = await prepareSkill({}, {}, '/skill:fixture');
  assert.equal(invalid.metadata.aw_skill_request, 'fixture');
  await assert.rejects(invalid.verify);
});

test('directory swaps retain a digest of the exact submitted bytes', async () => {
  const root = await mkdtemp(join(tmpdir(), 'aw-pi-skill-swap-'));
  try {
    const directory = join(root, 'selected'), signed = join(root, 'signed'), saved = join(root, 'saved');
    await mkdir(directory); await mkdir(signed);
    const untrusted = Buffer.from('\ufeff---\r\nname: fixture\r\n---\r\nUNTRUSTED-SNAPSHOT 中文\r\n');
    await writeFile(join(directory, 'SKILL.md'), untrusted);
    await writeFile(join(signed, 'SKILL.md'), 'SIGNED_LEDGER_CONTENT');
    const session = { resourceLoader: { getSkills: () => ({ skills: [
      { name: 'fixture', baseDir: directory, filePath: join(directory, 'SKILL.md') },
    ] }) }, extensionRunner: { getCommand: () => undefined } };
    const fixed = await prepareSkill(session, { stripFrontmatter: text => text }, '/skill:fixture');
    await rename(directory, saved); await rename(signed, directory);
    const ledgerHash = 'sha256:' + createHash('sha256').update(await readFile(join(directory, 'SKILL.md'))).digest('hex');
    await rename(directory, signed); await rename(saved, directory);
    await fixed.verify();
    assert(fixed.text.includes('UNTRUSTED-SNAPSHOT'));
    assert.equal(fixed.metadata.aw_skill.digest, 'sha256:' + createHash('sha256').update(untrusted).digest('hex'));
    assert.notEqual(fixed.metadata.aw_skill.digest, ledgerHash);
  } finally { await rm(root, { recursive: true, force: true }); }
});
