/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Test cases for orphaned tool calls cleanup
 */

export const createTestMessages = () => [
  // System message
  {
    role: 'system' as const,
    content: 'You are a helpful assistant.',
  },
  // User message
  {
    role: 'user' as const,
    content: 'Please use a tool to help me.',
  },
  // Assistant message with tool calls (some will be orphaned)
  {
    role: 'assistant' as const,
    content: 'I will help you with that.',
    tool_calls: [
      {
        id: 'call_1',
        type: 'function' as const,
        function: {
          name: 'search_web',
          arguments: '{"query": "test"}',
        },
      },
      {
        id: 'call_2',
        type: 'function' as const,
        function: {
          name: 'calculate',
          arguments: '{"expression": "2+2"}',
        },
      },
      {
        id: 'call_3', // This will be orphaned
        type: 'function' as const,
        function: {
          name: 'send_email',
          arguments: '{"to": "test@example.com"}',
        },
      },
    ],
  },
  // Tool response for call_1
  {
    role: 'tool' as const,
    tool_call_id: 'call_1',
    content: 'Search results: Found relevant information.',
  },
  // Tool response for call_2
  {
    role: 'tool' as const,
    tool_call_id: 'call_2',
    content: 'Calculation result: 4',
  },
  // Note: No tool response for call_3 (this creates the orphaned tool call issue)

  // User continues conversation
  {
    role: 'user' as const,
    content: 'Thank you, that was helpful.',
  },
];

export const expectedCleanedMessages = () => [
  // System message (unchanged)
  {
    role: 'system' as const,
    content: 'You are a helpful assistant.',
  },
  // User message (unchanged)
  {
    role: 'user' as const,
    content: 'Please use a tool to help me.',
  },
  // Assistant message with only valid tool calls
  {
    role: 'assistant' as const,
    content: 'I will help you with that.',
    tool_calls: [
      {
        id: 'call_1',
        type: 'function' as const,
        function: {
          name: 'search_web',
          arguments: '{"query": "test"}',
        },
      },
      {
        id: 'call_2',
        type: 'function' as const,
        function: {
          name: 'calculate',
          arguments: '{"expression": "2+2"}',
        },
      },
      // call_3 removed because it has no response
    ],
  },
  // Tool responses (unchanged because they have corresponding calls)
  {
    role: 'tool' as const,
    tool_call_id: 'call_1',
    content: 'Search results: Found relevant information.',
  },
  {
    role: 'tool' as const,
    tool_call_id: 'call_2',
    content: 'Calculation result: 4',
  },
  // User message (unchanged)
  {
    role: 'user' as const,
    content: 'Thank you, that was helpful.',
  },
];
