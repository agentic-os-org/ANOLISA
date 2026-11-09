/** Explicit preload for a prepared native foreground Gateway. */
import { spawnSync } from 'node:child_process';
import { accessSync, closeSync, constants, fstatSync, openSync, readFileSync, realpathSync } from 'node:fs';
import { basename, isAbsolute, join } from 'node:path';

function nodeInterpreter(shebang, environment) {
  if (shebang !== '#!/usr/bin/env node') {
    const interpreter = shebang.slice(2);
    if (!isAbsolute(interpreter) || /\s/.test(interpreter))
      throw new Error('Native Gateway requires a supported Node entrypoint');
    return realpathSync(interpreter);
  }
  for (const directory of (environment.PATH ?? '/usr/bin:/bin').split(':')) {
    const path = join(directory || '.', 'node');
    try {
      accessSync(path, constants.X_OK);
      return realpathSync(path);
    } catch { /* Continue the admitted command's PATH lookup. */ }
  }
  throw new Error('Native Gateway Node interpreter is unavailable');
}

function configuredArguments(command, environment) {
  if (typeof command?.program !== 'string' || !isAbsolute(command.program))
    throw new Error('AW native preparation returned an invalid Gateway program');
  const argv = command.args;
  if (!Array.isArray(argv) || argv.some(value => typeof value !== 'string'))
    throw new Error('AW native preparation returned invalid Gateway arguments');
  const program = realpathSync(command.program);
  const entry = realpathSync(process.argv[1]);
  if (basename(argv[0] ?? '') === 'openclaw.mjs') {
    if (program !== realpathSync(process.execPath) || realpathSync(argv[0]) !== entry)
      throw new Error('Native Gateway interpreter or module differs from the configured target');
    return argv.slice(1);
  }
  // npm's executable is a symlink to the Node entry. Shell wrappers cannot
  // establish which module was admitted and require an explicit Node command.
  if (program !== entry) throw new Error('Native Gateway program differs from the configured target');
  const shebang = readFileSync(program, 'utf8').split('\n', 1)[0].trim();
  if (nodeInterpreter(shebang, environment) !== realpathSync(process.execPath))
    throw new Error('Native Gateway interpreter differs from the configured target');
  return argv;
}

function privateJson(path, limit = 65536) {
  if (typeof path !== 'string' || !isAbsolute(path)) throw new Error('AW bootstrap requires an absolute private descriptor');
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.uid !== process.getuid() || (stat.mode & 0o7777) !== 0o600 || stat.size > limit)
      throw new Error('AW bootstrap descriptor must be a private bounded regular file');
    const bytes = readFileSync(fd);
    if (bytes.length > limit) throw new Error('AW bootstrap descriptor exceeds its limit');
    return JSON.parse(bytes);
  } finally { closeSync(fd); }
}

export function prepareGateway(descriptor) {
  const options = privateJson(descriptor);
  if (!isAbsolute(options.binary ?? '') || !isAbsolute(options.config ?? '') ||
      typeof options.agent !== 'string' || !options.agent || options.agent.length > 512)
    throw new Error('AW bootstrap requires binary, config and target');
  if (!isAbsolute(options.settings ?? '') || !isAbsolute(options.state_dir ?? ''))
    throw new Error('AW native Gateway requires settings and state directory');
  const result = spawnSync(options.binary, [
    'instance', 'prepare', '--config', options.config, '--agent', options.agent,
    '--native-settings', options.settings, '--native-state-dir', options.state_dir,
  ], { encoding: 'utf8', timeout: 90000, maxBuffer: 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] });
  if (result.error || result.status !== 0) throw new Error('AW native Gateway preparation failed before startup');
  const reply = JSON.parse(result.stdout);
  if (typeof reply.binding !== 'string' || !isAbsolute(reply.binding))
    throw new Error('AW native preparation returned an invalid binding');
  const release = () => {
    const stopped = spawnSync(options.binary, ['instance', 'release', '--binding', reply.binding],
      { encoding: 'utf8', timeout: 15000, maxBuffer: 65536, stdio: ['ignore', 'pipe', 'pipe'] });
    if (stopped.error || stopped.status !== 0) {
      process.stderr.write('AW native Gateway preparation cleanup failed\n');
      return false;
    }
    return true;
  };
  try {
    if (typeof reply.instance_id !== 'string' || !reply.instance_id || !reply.environment)
      throw new Error('AW native preparation returned an invalid instance');
    const environment = { ...process.env, ...reply.environment };
    const configured = configuredArguments(reply.command, environment);
    if (JSON.stringify(configured) !== JSON.stringify(process.argv.slice(2)))
      throw new Error('Native Gateway arguments differ from the configured target');
    for (const key of ['OPENCLAW_CONFIG_PATH', 'OPENCLAW_STATE_DIR']) {
      if (typeof environment[key] !== 'string' || !isAbsolute(environment[key]))
        throw new Error('AW native preparation returned an invalid Gateway environment');
    }
    if (environment.OPENCLAW_CONFIG_READONLY !== '1') throw new Error('AW native Gateway requires a read-only overlay');
    const config = privateJson(environment.OPENCLAW_CONFIG_PATH, 4 * 1024 * 1024);
    const plugin = config.plugins?.entries?.['aw-native-hooks'];
    if (plugin?.enabled !== true || !plugin.config?.binding || !plugin.config?.ready?.token)
      throw new Error('AW native Gateway overlay has no prepared hook registration');
    // Keep unrelated native environment and model credentials in their original owner.
    for (const key of ['OPENCLAW_CONFIG_PATH', 'OPENCLAW_STATE_DIR', 'OPENCLAW_CONFIG_READONLY'])
      process.env[key] = environment[key];
    process.env.NODE_DISABLE_COMPILE_CACHE = '1';
    delete process.env.AW_INSTANCE_BOOTSTRAP;
    // Exit listeners run while the native owner can still authenticate release.
    process.once('exit', code => {
      if (!release() && code === 0) process.exitCode = 1;
    });
    return reply.instance_id;
  } catch (error) {
    release();
    throw error;
  }
}

// Version and management probes must not recursively prepare another instance.
if (process.argv[1] && basename(realpathSync(process.argv[1])) === 'openclaw.mjs' &&
    process.argv[2] === 'gateway' && process.argv[3] === 'run') {
  prepareGateway(process.env.AW_INSTANCE_BOOTSTRAP);
}
