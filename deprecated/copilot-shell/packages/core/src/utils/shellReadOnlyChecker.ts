/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { parse } from 'shell-quote';
import {
  detectCommandSubstitution,
  splitCommands,
  stripShellWrapper,
} from './shell-utils.js';

const READ_ONLY_ROOT_COMMANDS = new Set([
  'awk',
  'basename',
  'cat',
  'cd',
  'column',
  'cut',
  'df',
  'dirname',
  'du',
  'echo',
  'env',
  'find',
  'git',
  'grep',
  'head',
  'less',
  'ls',
  'more',
  'printenv',
  'printf',
  'ps',
  'pwd',
  'rg',
  'ripgrep',
  'sed',
  'sort',
  'stat',
  'tail',
  'tree',
  'uniq',
  'wc',
  'which',
  'where',
  'whoami',
]);

const BLOCKED_FIND_FLAGS = new Set([
  '-delete',
  '-exec',
  '-execdir',
  '-ok',
  '-okdir',
]);

const BLOCKED_FIND_PREFIXES = ['-fprint', '-fprintf'];

const READ_ONLY_GIT_SUBCOMMANDS = new Set([
  'blame',
  'branch',
  'cat-file',
  'diff',
  'grep',
  'log',
  'ls-files',
  'remote',
  'rev-parse',
  'show',
  'status',
  'describe',
]);

const BLOCKED_GIT_REMOTE_ACTIONS = new Set([
  'add',
  'remove',
  'rename',
  'set-url',
  'prune',
  'update',
]);

const BLOCKED_GIT_BRANCH_FLAGS = new Set([
  '-d',
  '-D',
  '--delete',
  '--move',
  '-m',
  '-M',
  '-c',
  '-C',
  '-u',
  '--set-upstream-to',
  '--edit-description',
]);

// `git log`/`diff`/`show` accept `--output=<file>`, which writes to disk.
const GIT_OUTPUT_FLAGS = ['--output'];

const BLOCKED_SED_PREFIXES = ['-i'];

// GNU sed also accepts the long form, with or without a backup suffix.
const BLOCKED_SED_LONG_IN_PLACE_PREFIX = '--in-place';

// `env` executes its operand once one is supplied; only pure environment
// printing/adjustment stays read-only.
const ENV_VALUE_OPTIONS = new Set([
  '-u',
  '--unset',
  '-C',
  '--chdir',
  '-S',
  '--split-string',
]);

const ENV_VALUE_OPTION_PREFIXES = ['--unset=', '--chdir=', '--split-string='];

// `sort -o <file>` / `sort --output=<file>` writes to disk.
const SORT_OUTPUT_PREFIXES = ['--output'];

// `uniq` treats a second file operand as the output file.
const UNIQ_VALUE_OPTIONS = new Set([
  '-f',
  '--skip-fields',
  '-s',
  '--skip-chars',
  '-w',
  '--check-chars',
]);

// ripgrep runs an external program when these options are present.
const RIPGREP_EXEC_PREFIXES = ['--pre=', '--hostname-bin='];
const RIPGREP_EXEC_OPTIONS = new Set(['--pre', '--hostname-bin']);

// AWK side-effect patterns that can execute commands or write files
const AWK_SIDE_EFFECT_PATTERNS = [
  /system\s*\(/, // system() function calls
  /print\s+[^>|]*>\s*"[^"]*"/, // print > "file"
  /printf\s+[^>|]*>\s*"[^"]*"/, // printf > "file"
  /print\s+[^>|]*>>\s*"[^"]*"/, // print >> "file"
  /printf\s+[^>|]*>>\s*"[^"]*"/, // printf >> "file"
  /print\s+[^|]*\|\s*"[^"]*"/, // print | "command"
  /printf\s+[^|]*\|\s*"[^"]*"/, // printf | "command"
  /getline\s*<\s*"[^"]*"/, // getline < "command"
  /"[^"]*"\s*\|\s*getline/, // "command" | getline
  /close\s*\(/, // close() can trigger command execution
];

// SED side-effect patterns
const SED_SIDE_EFFECT_PATTERNS = [
  /[^\\]e\s/, // e command (execute)
  /^e\s/, // e command at start
  /[^\\]w\s/, // w command (write)
  /^w\s/, // w command at start
  /[^\\]r\s/, // r command (read file)
  /^r\s/, // r command at start
];

const ENV_ASSIGNMENT_REGEX = /^[A-Za-z_][A-Za-z0-9_]*=/;

function containsWriteRedirection(command: string): boolean {
  let inSingleQuotes = false;
  let inDoubleQuotes = false;
  let escapeNext = false;

  for (const char of command) {
    if (escapeNext) {
      escapeNext = false;
      continue;
    }

    if (char === '\\' && !inSingleQuotes) {
      escapeNext = true;
      continue;
    }

    if (char === "'" && !inDoubleQuotes) {
      inSingleQuotes = !inSingleQuotes;
      continue;
    }

    if (char === '"' && !inSingleQuotes) {
      inDoubleQuotes = !inDoubleQuotes;
      continue;
    }

    if (!inSingleQuotes && !inDoubleQuotes && char === '>') {
      return true;
    }
  }

  return false;
}

function normalizeTokens(segment: string): string[] {
  const parsed = parse(segment);
  const tokens: string[] = [];
  for (const token of parsed) {
    if (typeof token === 'string') {
      tokens.push(token);
    }
  }
  return tokens;
}

function skipEnvironmentAssignments(tokens: string[]): {
  root?: string;
  args: string[];
} {
  let index = 0;
  while (index < tokens.length && ENV_ASSIGNMENT_REGEX.test(tokens[index]!)) {
    index++;
  }

  if (index >= tokens.length) {
    return { args: [] };
  }

  return {
    root: tokens[index],
    args: tokens.slice(index + 1),
  };
}

function evaluateFindCommand(tokens: string[]): boolean {
  const [, ...rest] = tokens;
  for (const token of rest) {
    const lower = token.toLowerCase();
    if (BLOCKED_FIND_FLAGS.has(lower)) {
      return false;
    }
    if (BLOCKED_FIND_PREFIXES.some((prefix) => lower.startsWith(prefix))) {
      return false;
    }
  }
  return true;
}

function evaluateSedCommand(tokens: string[]): boolean {
  const [, ...rest] = tokens;
  for (const token of rest) {
    if (
      BLOCKED_SED_PREFIXES.some((prefix) => token.startsWith(prefix)) ||
      token === BLOCKED_SED_LONG_IN_PLACE_PREFIX ||
      token.startsWith(`${BLOCKED_SED_LONG_IN_PLACE_PREFIX}=`)
    ) {
      return false;
    }
  }

  // Check for side-effect patterns in sed script
  const scriptContent = rest.join(' ');
  for (const pattern of SED_SIDE_EFFECT_PATTERNS) {
    if (pattern.test(scriptContent)) {
      return false;
    }
  }

  return true;
}

function evaluateAwkCommand(tokens: string[]): boolean {
  const [, ...rest] = tokens;

  // Join all arguments to check for awk script content
  const scriptContent = rest.join(' ');

  // Check for dangerous side-effect patterns
  for (const pattern of AWK_SIDE_EFFECT_PATTERNS) {
    if (pattern.test(scriptContent)) {
      return false;
    }
  }

  return true;
}

function evaluateGitRemoteArgs(args: string[]): boolean {
  for (const arg of args) {
    if (BLOCKED_GIT_REMOTE_ACTIONS.has(arg.toLowerCase())) {
      return false;
    }
  }
  return true;
}

function evaluateGitBranchArgs(args: string[]): boolean {
  for (const arg of args) {
    if (
      BLOCKED_GIT_BRANCH_FLAGS.has(arg) ||
      arg.startsWith('--set-upstream-to=')
    ) {
      return false;
    }
  }
  return true;
}

function evaluateGitCommand(tokens: string[]): boolean {
  let index = 1;
  while (index < tokens.length && tokens[index]!.startsWith('-')) {
    const flag = tokens[index]!.toLowerCase();
    if (flag === '--version' || flag === '--help') {
      return true;
    }
    index++;
  }

  if (index >= tokens.length) {
    return true;
  }

  const subcommand = tokens[index]!.toLowerCase();
  if (!READ_ONLY_GIT_SUBCOMMANDS.has(subcommand)) {
    return false;
  }

  const args = tokens.slice(index + 1);

  if (
    args.some(
      (arg) =>
        GIT_OUTPUT_FLAGS.includes(arg) ||
        GIT_OUTPUT_FLAGS.some((flag) => arg.startsWith(`${flag}=`)),
    )
  ) {
    // Read-only subcommands can still be asked to write their output to a file.
    return false;
  }

  if (subcommand === 'remote') {
    return evaluateGitRemoteArgs(args);
  }

  if (subcommand === 'branch') {
    return evaluateGitBranchArgs(args);
  }

  return true;
}

/**
 * `env` prints or adjusts the environment only when it has no command operand.
 * Once a non-option operand (the command to run) is present, the invocation can
 * execute arbitrary programs and must not be auto-approved as read-only.
 */
function evaluateEnvCommand(args: string[]): boolean {
  let index = 0;
  while (index < args.length) {
    const arg = args[index]!;
    if (ENV_ASSIGNMENT_REGEX.test(arg)) {
      index++;
      continue;
    }
    if (arg === '--') {
      // Everything after `--` is the command to execute.
      index++;
      break;
    }
    if (
      arg === '-i' ||
      arg === '--ignore-environment' ||
      arg === '-0' ||
      arg === '--null'
    ) {
      index++;
      continue;
    }
    if (ENV_VALUE_OPTIONS.has(arg)) {
      index += 2;
      continue;
    }
    if (ENV_VALUE_OPTION_PREFIXES.some((prefix) => arg.startsWith(prefix))) {
      index++;
      continue;
    }
    break;
  }
  return index >= args.length;
}

function evaluateSortCommand(args: string[]): boolean {
  for (const arg of args) {
    if (
      arg === '-o' ||
      (arg.startsWith('-o') && arg.length > 2) ||
      SORT_OUTPUT_PREFIXES.includes(arg) ||
      SORT_OUTPUT_PREFIXES.some((flag) => arg.startsWith(`${flag}=`))
    ) {
      // `sort -o <file>` writes to the given file.
      return false;
    }
  }
  return true;
}

function evaluateUniqCommand(args: string[]): boolean {
  let operands = 0;
  for (let index = 0; index < args.length; index++) {
    const arg = args[index]!;
    if (arg === '--') {
      operands += args.length - index - 1;
      break;
    }
    if (arg.startsWith('-')) {
      if (UNIQ_VALUE_OPTIONS.has(arg)) {
        index++; // Skip the option's value.
      }
      continue;
    }
    operands++;
  }
  // `uniq [INPUT [OUTPUT]]` writes to OUTPUT when two operands are supplied.
  return operands < 2;
}

function evaluateRipgrepCommand(args: string[]): boolean {
  for (const arg of args) {
    if (
      RIPGREP_EXEC_OPTIONS.has(arg) ||
      RIPGREP_EXEC_PREFIXES.some((prefix) => arg.startsWith(prefix))
    ) {
      // `--pre`/`--hostname-bin` execute an external program.
      return false;
    }
  }
  return true;
}

function evaluateShellSegment(segment: string): boolean {
  if (!segment.trim()) {
    return true;
  }

  const stripped = stripShellWrapper(segment);
  if (!stripped) {
    return true;
  }

  if (detectCommandSubstitution(stripped)) {
    return false;
  }

  if (containsWriteRedirection(stripped)) {
    return false;
  }

  const tokens = normalizeTokens(stripped);
  if (tokens.length === 0) {
    return true;
  }

  const { root, args } = skipEnvironmentAssignments(tokens);
  if (!root) {
    return true;
  }

  const normalizedRoot = root.toLowerCase();
  if (!READ_ONLY_ROOT_COMMANDS.has(normalizedRoot)) {
    return false;
  }

  if (normalizedRoot === 'find') {
    return evaluateFindCommand([normalizedRoot, ...args]);
  }

  if (normalizedRoot === 'sed') {
    return evaluateSedCommand([normalizedRoot, ...args]);
  }

  if (normalizedRoot === 'awk') {
    return evaluateAwkCommand([normalizedRoot, ...args]);
  }

  if (normalizedRoot === 'git') {
    return evaluateGitCommand([normalizedRoot, ...args]);
  }

  if (normalizedRoot === 'env') {
    return evaluateEnvCommand(args);
  }

  if (normalizedRoot === 'sort') {
    return evaluateSortCommand(args);
  }

  if (normalizedRoot === 'uniq') {
    return evaluateUniqCommand(args);
  }

  if (normalizedRoot === 'rg' || normalizedRoot === 'ripgrep') {
    return evaluateRipgrepCommand(args);
  }

  return true;
}

export function isShellCommandReadOnly(command: string): boolean {
  if (typeof command !== 'string' || !command.trim()) {
    return false;
  }

  const segments = splitCommands(command);

  for (const segment of segments) {
    if (!evaluateShellSegment(segment)) {
      return false;
    }
  }

  return segments.length > 0;
}
