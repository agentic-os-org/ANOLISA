/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

/* eslint-disable @typescript-eslint/no-explicit-any */
import type { Mocked } from 'vitest';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import type { ConfigParameters } from '../config/config.js';
import { Config, ApprovalMode } from '../config/config.js';
import { ToolRegistry, DiscoveredTool } from './tool-registry.js';
import { DiscoveredMCPTool } from './mcp-tool.js';
import type { FunctionDeclaration, CallableTool } from '@google/genai';
import { mcpToTool } from '@google/genai';
import { spawn } from 'node:child_process';
import { PassThrough, EventEmitter } from 'node:stream';
import fs from 'node:fs';
import { MockTool } from '../test-utils/mock-tool.js';

import { McpClientManager } from './mcp-client-manager.js';
import { ToolErrorType } from './tool-error.js';

vi.mock('node:fs');

// Mock ./mcp-client.js to control its behavior within tool-registry tests
vi.mock('./mcp-client.js', async () => {
  const originalModule = await vi.importActual('./mcp-client.js');
  return {
    ...originalModule,
  };
});

// Mock node:child_process
vi.mock('node:child_process', async () => {
  const actual = await vi.importActual('node:child_process');
  return {
    ...actual,
    execSync: vi.fn(),
    spawn: vi.fn(),
  };
});

// Mock MCP SDK Client and Transports
const mockMcpClientConnect = vi.fn();
const mockMcpClientOnError = vi.fn();
const mockStdioTransportClose = vi.fn();
const mockSseTransportClose = vi.fn();

vi.mock('@modelcontextprotocol/sdk/client/index.js', () => {
  const MockClient = vi.fn().mockImplementation(() => ({
    connect: mockMcpClientConnect,
    set onerror(handler: any) {
      mockMcpClientOnError(handler);
    },
  }));
  return { Client: MockClient };
});

vi.mock('@modelcontextprotocol/sdk/client/stdio.js', () => {
  const MockStdioClientTransport = vi.fn().mockImplementation(() => ({
    stderr: {
      on: vi.fn(),
    },
    close: mockStdioTransportClose,
  }));
  return { StdioClientTransport: MockStdioClientTransport };
});

vi.mock('@modelcontextprotocol/sdk/client/sse.js', () => {
  const MockSSEClientTransport = vi.fn().mockImplementation(() => ({
    close: mockSseTransportClose,
  }));
  return { SSEClientTransport: MockSSEClientTransport };
});

// Mock @google/genai mcpToTool
vi.mock('@google/genai', async () => {
  const actualGenai =
    await vi.importActual<typeof import('@google/genai')>('@google/genai');
  return {
    ...actualGenai,
    mcpToTool: vi.fn().mockImplementation(() => ({
      tool: vi.fn().mockResolvedValue({ functionDeclarations: [] }),
      callTool: vi.fn(),
    })),
  };
});

// Helper to create a mock CallableTool for specific test needs
const createMockCallableTool = (
  toolDeclarations: FunctionDeclaration[],
): Mocked<CallableTool> => ({
  tool: vi.fn().mockResolvedValue({ functionDeclarations: toolDeclarations }),
  callTool: vi.fn(),
});

const baseConfigParams: ConfigParameters = {
  cwd: '/tmp',
  model: 'test-model',
  embeddingModel: 'test-embedding-model',

  targetDir: '/test/dir',
  debugMode: false,
  userMemory: '',
  geminiMdFileCount: 0,
  approvalMode: ApprovalMode.DEFAULT,
};

describe('ToolRegistry', () => {
  let config: Config;
  let toolRegistry: ToolRegistry;
  let mockConfigGetToolDiscoveryCommand: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    vi.mocked(fs.existsSync).mockReturnValue(true);
    vi.mocked(fs.statSync).mockReturnValue({
      isDirectory: () => true,
    } as fs.Stats);
    config = new Config(baseConfigParams);
    toolRegistry = new ToolRegistry(config);
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.spyOn(console, 'debug').mockImplementation(() => {});
    vi.spyOn(console, 'log').mockImplementation(() => {});

    mockMcpClientConnect.mockReset().mockResolvedValue(undefined);
    mockStdioTransportClose.mockReset();
    mockSseTransportClose.mockReset();
    vi.mocked(mcpToTool).mockClear();
    vi.mocked(mcpToTool).mockReturnValue(createMockCallableTool([]));

    mockConfigGetToolDiscoveryCommand = vi.spyOn(
      config,
      'getToolDiscoveryCommand',
    );
    vi.spyOn(config, 'getMcpServers');
    vi.spyOn(config, 'getMcpServerCommand');
    vi.spyOn(config, 'getPromptRegistry').mockReturnValue({
      clear: vi.fn(),
      removePromptsByServer: vi.fn(),
    } as any);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe('registerTool', () => {
    it('should register a new tool', () => {
      const tool = new MockTool({ name: 'mock-tool' });
      toolRegistry.registerTool(tool);
      expect(toolRegistry.getTool('mock-tool')).toBe(tool);
    });
  });

  describe('getAllTools', () => {
    it('should return all registered tools sorted alphabetically by displayName', () => {
      // Register tools with displayNames in non-alphabetical order
      const toolC = new MockTool({ name: 'c-tool', displayName: 'Tool C' });
      const toolA = new MockTool({ name: 'a-tool', displayName: 'Tool A' });
      const toolB = new MockTool({ name: 'b-tool', displayName: 'Tool B' });

      toolRegistry.registerTool(toolC);
      toolRegistry.registerTool(toolA);
      toolRegistry.registerTool(toolB);

      const allTools = toolRegistry.getAllTools();
      const displayNames = allTools.map((t) => t.displayName);

      // Assert that the returned array is sorted by displayName
      expect(displayNames).toEqual(['Tool A', 'Tool B', 'Tool C']);
    });
  });

  describe('getAllToolNames', () => {
    it('should return all registered tool names', () => {
      // Register tools with displayNames in non-alphabetical order
      const toolC = new MockTool({ name: 'c-tool', displayName: 'Tool C' });
      const toolA = new MockTool({ name: 'a-tool', displayName: 'Tool A' });
      const toolB = new MockTool({ name: 'b-tool', displayName: 'Tool B' });

      toolRegistry.registerTool(toolC);
      toolRegistry.registerTool(toolA);
      toolRegistry.registerTool(toolB);

      const toolNames = toolRegistry.getAllToolNames();

      // Assert that the returned array contains all tool names
      expect(toolNames).toEqual(['c-tool', 'a-tool', 'b-tool']);
    });
  });

  describe('getToolsByServer', () => {
    it('should return an empty array if no tools match the server name', () => {
      toolRegistry.registerTool(new MockTool({ name: 'mock-tool' }));
      expect(toolRegistry.getToolsByServer('any-mcp-server')).toEqual([]);
    });

    it('should return only tools matching the server name, sorted by name', async () => {
      const server1Name = 'mcp-server-uno';
      const server2Name = 'mcp-server-dos';
      const mockCallable = {} as CallableTool;
      const mcpTool1_c = new DiscoveredMCPTool(
        mockCallable,
        server1Name,
        'zebra-tool',
        'd1',
        {},
      );
      const mcpTool1_a = new DiscoveredMCPTool(
        mockCallable,
        server1Name,
        'apple-tool',
        'd2',
        {},
      );
      const mcpTool1_b = new DiscoveredMCPTool(
        mockCallable,
        server1Name,
        'banana-tool',
        'd3',
        {},
      );

      const mcpTool2 = new DiscoveredMCPTool(
        mockCallable,
        server2Name,
        'tool-on-server2',
        'd4',
        {},
      );
      const nonMcpTool = new MockTool({ name: 'regular-tool' });

      toolRegistry.registerTool(mcpTool1_c);
      toolRegistry.registerTool(mcpTool1_a);
      toolRegistry.registerTool(mcpTool1_b);
      toolRegistry.registerTool(mcpTool2);
      toolRegistry.registerTool(nonMcpTool);

      const toolsFromServer1 = toolRegistry.getToolsByServer(server1Name);
      const toolNames = toolsFromServer1.map((t) => t.name);

      // Assert that the array has the correct tools and is sorted by name
      expect(toolsFromServer1).toHaveLength(3);
      expect(toolNames).toEqual(['apple-tool', 'banana-tool', 'zebra-tool']);

      // Assert that all returned tools are indeed from the correct server
      for (const tool of toolsFromServer1) {
        expect((tool as DiscoveredMCPTool).serverName).toBe(server1Name);
      }

      // Assert that the other server's tools are returned correctly
      const toolsFromServer2 = toolRegistry.getToolsByServer(server2Name);
      expect(toolsFromServer2).toHaveLength(1);
      expect(toolsFromServer2[0].name).toBe(mcpTool2.name);
    });
  });

  describe('discoverTools', () => {
    it('should will preserve tool parametersJsonSchema during discovery from command', async () => {
      const discoveryCommand = 'my-discovery-command';
      mockConfigGetToolDiscoveryCommand.mockReturnValue(discoveryCommand);

      const unsanitizedToolDeclaration: FunctionDeclaration = {
        name: 'tool-with-bad-format',
        description: 'A tool with an invalid format property',
        parametersJsonSchema: {
          type: 'object',
          properties: {
            some_string: {
              type: 'string',
              format: 'uuid', // This is an unsupported format
            },
          },
        },
      };

      const mockSpawn = vi.mocked(spawn);
      const mockChildProcess = {
        stdout: { on: vi.fn() },
        stderr: { on: vi.fn() },
        on: vi.fn(),
      };
      mockSpawn.mockReturnValue(mockChildProcess as any);

      // Simulate stdout data
      mockChildProcess.stdout.on.mockImplementation((event, callback) => {
        if (event === 'data') {
          callback(
            Buffer.from(
              JSON.stringify([
                { function_declarations: [unsanitizedToolDeclaration] },
              ]),
            ),
          );
        }
        return mockChildProcess as any;
      });

      // Simulate process close
      mockChildProcess.on.mockImplementation((event, callback) => {
        if (event === 'close') {
          callback(0);
        }
        return mockChildProcess as any;
      });

      await toolRegistry.discoverAllTools();

      const discoveredTool = toolRegistry.getTool('tool-with-bad-format');
      expect(discoveredTool).toBeDefined();

      const registeredParams = (discoveredTool as DiscoveredTool).schema
        .parametersJsonSchema;
      expect(registeredParams).toStrictEqual({
        type: 'object',
        properties: {
          some_string: {
            type: 'string',
            format: 'uuid',
          },
        },
      });
    });

    it('should return a DISCOVERED_TOOL_EXECUTION_ERROR on tool failure', async () => {
      const discoveryCommand = 'my-discovery-command';
      mockConfigGetToolDiscoveryCommand.mockReturnValue(discoveryCommand);
      vi.spyOn(config, 'getToolCallCommand').mockReturnValue('my-call-command');

      const toolDeclaration: FunctionDeclaration = {
        name: 'failing-tool',
        description: 'A tool that fails',
        parametersJsonSchema: {
          type: 'object',
          properties: {},
        },
      };

      const mockSpawn = vi.mocked(spawn);
      // --- Discovery Mock ---
      const discoveryProcess = {
        stdout: { on: vi.fn(), removeListener: vi.fn() },
        stderr: { on: vi.fn(), removeListener: vi.fn() },
        on: vi.fn(),
      };
      mockSpawn.mockReturnValueOnce(discoveryProcess as any);

      discoveryProcess.stdout.on.mockImplementation((event, callback) => {
        if (event === 'data') {
          callback(
            Buffer.from(
              JSON.stringify([{ functionDeclarations: [toolDeclaration] }]),
            ),
          );
        }
      });
      discoveryProcess.on.mockImplementation((event, callback) => {
        if (event === 'close') {
          callback(0);
        }
      });

      await toolRegistry.discoverAllTools();
      const discoveredTool = toolRegistry.getTool('failing-tool');
      expect(discoveredTool).toBeDefined();

      // --- Execution Mock ---
      const executionProcess = {
        stdout: { on: vi.fn(), removeListener: vi.fn() },
        stderr: { on: vi.fn(), removeListener: vi.fn() },
        stdin: { write: vi.fn(), end: vi.fn(), on: vi.fn() },
        on: vi.fn(),
        connected: true,
        disconnect: vi.fn(),
        removeListener: vi.fn(),
      };
      mockSpawn.mockReturnValueOnce(executionProcess as any);

      executionProcess.stderr.on.mockImplementation((event, callback) => {
        if (event === 'data') {
          callback(Buffer.from('Something went wrong'));
        }
      });
      executionProcess.on.mockImplementation((event, callback) => {
        if (event === 'close') {
          callback(1); // Non-zero exit code
        }
      });

      const invocation = (discoveredTool as DiscoveredTool).build({});
      const result = await invocation.execute(new AbortController().signal);

      expect(result.error?.type).toBe(
        ToolErrorType.DISCOVERED_TOOL_EXECUTION_ERROR,
      );
      expect(result.llmContent).toContain('Stderr: Something went wrong');
      expect(result.llmContent).toContain('Exit Code: 1');
    });

    it('should discover tools using MCP servers defined in getMcpServers', async () => {
      const discoverSpy = vi.spyOn(
        McpClientManager.prototype,
        'discoverAllMcpTools',
      );
      mockConfigGetToolDiscoveryCommand.mockReturnValue(undefined);
      vi.spyOn(config, 'getMcpServerCommand').mockReturnValue(undefined);
      const mcpServerConfigVal = {
        'my-mcp-server': {
          command: 'mcp-server-cmd',
          args: ['--port', '1234'],
          trust: true,
        },
      };
      vi.spyOn(config, 'getMcpServers').mockReturnValue(mcpServerConfigVal);

      await toolRegistry.discoverAllTools();

      expect(discoverSpy).toHaveBeenCalled();
    });
  });

  describe('DiscoveredToolInvocation', () => {
    it('should return the stringified params from getDescription', () => {
      const tool = new DiscoveredTool(config, 'test-tool', 'A test tool', {});
      const params = { param: 'testValue' };
      const invocation = tool.build(params);
      const description = invocation.getDescription();
      expect(description).toBe(JSON.stringify(params));
    });

    function createFakeChildProcess() {
      const child = new EventEmitter() as any;
      child.stdin = new PassThrough();
      child.stdout = new PassThrough();
      child.stderr = new PassThrough();
      child.kill = vi.fn();
      child.disconnect = vi.fn();
      child.connected = true;
      return child;
    }

    it('should treat EPIPE on stdin as a tool failure with partial output instead of crashing', async () => {
      vi.spyOn(config, 'getToolDiscoveryCommand').mockReturnValue(
        'my-discovery-command',
      );
      vi.spyOn(config, 'getToolCallCommand').mockReturnValue('my-call-command');

      const fakeChild = createFakeChildProcess();
      vi.mocked(spawn).mockReturnValue(fakeChild);

      const tool = new DiscoveredTool(config, 'epipe-tool', 'A test tool', {});
      const invocation = tool.build({ param: 'value' });
      const execution = invocation.execute(new AbortController().signal);

      // The child exits before consuming stdin, so the write fails with EPIPE.
      const epipeError = Object.assign(new Error('write EPIPE'), {
        code: 'EPIPE',
      });
      fakeChild.stdin.emit('error', epipeError);
      fakeChild.stdout.write(Buffer.from('partial stdout before exit'));
      fakeChild.stderr.write(Buffer.from('bye'));
      fakeChild.emit('close', 1, null);

      const result = await execution;

      expect(result.error?.type).toBe(
        ToolErrorType.DISCOVERED_TOOL_EXECUTION_ERROR,
      );
      expect(String(result.llmContent)).toContain('partial stdout before exit');
      expect(String(result.llmContent)).toContain('EPIPE');
    });

    it('should cap accumulated output instead of buffering unbounded child output', async () => {
      vi.spyOn(config, 'getToolDiscoveryCommand').mockReturnValue(
        'my-discovery-command',
      );
      vi.spyOn(config, 'getToolCallCommand').mockReturnValue('my-call-command');

      const fakeChild = createFakeChildProcess();
      vi.mocked(spawn).mockReturnValue(fakeChild);

      const tool = new DiscoveredTool(config, 'chatty-tool', 'A test tool', {});
      const invocation = tool.build({});
      const execution = invocation.execute(new AbortController().signal);

      const MAX_OUTPUT_SIZE = 10 * 1024 * 1024;
      const chunk = Buffer.alloc(6 * 1024 * 1024, 0x61); // 6MB of 'a'
      fakeChild.stdout.write(chunk); // 6MB, still under the cap
      fakeChild.stdout.write(chunk); // 12MB total, exceeds the cap
      fakeChild.emit('close', null, 'SIGKILL');

      const result = await execution;

      // The runaway child must be killed and the tool must fail.
      expect(fakeChild.kill).toHaveBeenCalled();
      expect(result.error?.type).toBe(
        ToolErrorType.DISCOVERED_TOOL_EXECUTION_ERROR,
      );
      // Whatever is reported back must not exceed the cap (plus a small
      // envelope for the surrounding error details).
      expect(String(result.llmContent).length).toBeLessThan(
        MAX_OUTPUT_SIZE + 1024,
      );
      expect(String(result.llmContent)).not.toContain('b'.repeat(1024));
    });

    it('should settle with the execution error when stdin fails synchronously', async () => {
      vi.spyOn(config, 'getToolDiscoveryCommand').mockReturnValue(
        'my-discovery-command',
      );
      vi.spyOn(config, 'getToolCallCommand').mockReturnValue('my-call-command');

      const fakeChild = createFakeChildProcess();
      vi.mocked(spawn).mockReturnValue(fakeChild);

      const tool = new DiscoveredTool(config, 'bigint-tool', 'A test tool', {});
      // A permissive schema can admit a bigint, which JSON.stringify
      // rejects synchronously before anything reaches stdin.
      const invocation = tool.build({ value: 1n });
      const execution = invocation.execute(new AbortController().signal);

      let timeout: ReturnType<typeof setTimeout> | undefined;
      const timeoutPromise = new Promise<never>((_, reject) => {
        timeout = setTimeout(
          () =>
            reject(
              new Error(
                'execution did not settle after a synchronous stdin failure',
              ),
            ),
          5_000,
        );
      });

      let result;
      try {
        result = await Promise.race([execution, timeoutPromise]);
      } finally {
        clearTimeout(timeout);
      }

      // The invocation must not hang waiting for a child that never got a
      // stdin payload; it settles through the same failure completion the
      // async error paths use, and never spawns the call command at all.
      expect(spawn).not.toHaveBeenCalled();
      expect(result.error?.type).toBe(
        ToolErrorType.DISCOVERED_TOOL_EXECUTION_ERROR,
      );
      expect(String(result.llmContent)).toContain('BigInt');
    });
  });
});
