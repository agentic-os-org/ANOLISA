import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

test('final preload closes early events, reaps cancellation and skips subsequent steps', { timeout: 15000 }, () => {
  const root = mkdtempSync(join(tmpdir(), 'aw-claude-final-'));
  try {
    mkdirSync(join(root, 'shared/js-runtime'), { recursive: true });
    for (const [source, target] of [['final-preload.mjs', 'preload.mjs'],
      ['final-gate.mjs', 'claude-final-gate.mjs'], ['../shared/js-runtime/fetch-gate.mjs', 'shared/js-runtime/fetch-gate.mjs']]) {
      copyFileSync(new URL(source, import.meta.url), join(root, target));
    }
    const executable = join(root, 'aw');
    writeFileSync(executable, `#!${process.execPath}
import { appendFileSync } from 'node:fs';
const chunks = []; for await (const part of process.stdin) chunks.push(part);
appendFileSync(process.env.CALLS, JSON.stringify({pid:process.pid,args:process.argv.slice(2),payload:JSON.parse(Buffer.concat(chunks))})+'\\n');
if(process.argv.includes('--close-event')) process.exit(process.env.MODE==='cleanup-error'?1:0);
if(process.env.MODE==='abort') await new Promise(resolve=>setTimeout(resolve,10000));
process.exit(process.env.MODE==='allow'?0:2);
`, { mode: 0o700 });
    writeFileSync(join(root, 'package.json'), '{"type":"module"}');
    writeFileSync(join(root, 'run.mjs'), `
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
const root=${JSON.stringify(root)};
const native={session_id:'session',tool_use_id:'call',tool_name:'Bash',tool_input:{command:'test'},cwd:'/work'};
writeFileSync(root+'/claude-final-records.json',JSON.stringify({call:native}));
const config={executable:root+'/aw',binding:'/fixture',origin:'https://model.test',ready_token:'fixture',
final_profile:{bun:'fixture',sha256:{'_bundled/claude':createHash('sha256').update(readFileSync('/proc/self/exe')).digest('hex')}},
hooks:[1,2].map(i=>({event:'security.violation',step:'step-'+i,on_error:'block',budget_ms:1000}))};
writeFileSync(root+'/config.json',JSON.stringify(config));
process.env.AW_CLAUDE_FINAL_CONFIG=root+'/config.json';process.env.BUN_OPTIONS='--preload='+root+'/preload.mjs';
globalThis.Bun={version:'fixture'};let sent;
globalThis.fetch=async(_url,init)=>{sent=JSON.parse(init.body);};
await import(root+'/preload.mjs');
const controller=new AbortController();
const body=JSON.stringify({metadata:{user_id:JSON.stringify({session_id:'session'})},messages:[
{role:'assistant',content:[{type:'tool_use',id:'call',name:'Bash',input:{command:'test'}}]},
{role:'user',content:[{type:'tool_result',tool_use_id:'call',content:'secret'}]}]});
const pending=fetch('https://model.test/v1/messages',{body,signal:controller.signal}).then(()=>null,error=>error);
if(process.env.MODE==='abort'){
for(let i=0;i<200&&!existsSync(process.env.CALLS);i++) await new Promise(resolve=>setTimeout(resolve,5));
assert(existsSync(process.env.CALLS));controller.abort();}
const error=await pending;
const calls=readFileSync(process.env.CALLS,'utf8').trim().split('\\n').map(JSON.parse);
assert.equal(calls.length,2);assert(calls.every(call=>!existsSync('/proc/'+call.pid)));
assert.deepEqual(calls[0].payload,calls[1].payload);
if(process.env.MODE==='allow'){assert.equal(sent.messages[1].content[0].content,'secret');assert(calls[1].args.includes('step-2'));}
else {assert(calls[1].args.includes('--close-event'));assert(!calls[1].args.includes('step-2'));
if(process.env.MODE==='abort'){assert.equal(error,controller.signal.reason);assert.equal(sent,undefined);}
else assert.equal(sent.messages[1].content[0].content,'AW withheld the final tool result');}
`);
    for (const mode of ['allow', 'deny', 'cleanup-error', 'abort']) {
      const result = spawnSync(process.execPath, [join(root, 'run.mjs')], {
        env: { ...process.env, MODE: mode, CALLS: join(root, `${mode}.jsonl`) },
        encoding: 'utf8', timeout: 3000,
      });
      assert.equal(result.status, 0, `${mode}: ${result.stderr}`);
      // The preload receipt is intentionally exclusive for each launch.
      rmSync(join(root, 'claude-final-ready.json'));
    }
  } finally { rmSync(root, { recursive: true, force: true }); }
});
