/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render } from 'ink-testing-library';
import {
  CustomAgentKeySharePrompt,
  type AgentChoice,
} from './CustomAgentKeySharePrompt.js';
import { RadioButtonSelect } from './shared/RadioButtonSelect.js';

// Mock useKeypress so the component does not try to attach to a real stdin.
vi.mock('../hooks/useKeypress.js', () => ({
  useKeypress: vi.fn(),
}));

// Mock RadioButtonSelect so we can assert the exact item list passed in,
// without depending on BaseSelectionList / selection hooks.
vi.mock('./shared/RadioButtonSelect.js', () => ({
  RadioButtonSelect: vi.fn(() => null),
}));

const MockedRadioButtonSelect = vi.mocked(
  RadioButtonSelect,
) as unknown as ReturnType<typeof vi.fn>;

function getRenderedChoices(): AgentChoice[] {
  const props = MockedRadioButtonSelect.mock.calls.at(-1)?.[0] as
    | { items: Array<{ value: AgentChoice }> }
    | undefined;
  if (!props) {
    throw new Error('RadioButtonSelect was not rendered');
  }
  return props.items.map((item) => item.value);
}

describe('CustomAgentKeySharePrompt', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('renders all three choices when nothing is excluded', () => {
    render(<CustomAgentKeySharePrompt onSelect={vi.fn()} onCancel={vi.fn()} />);
    expect(getRenderedChoices()).toEqual(['openclaw', 'qwencode', 'none']);
  });

  it('hides qwencode when it is in excludedChoices', () => {
    // Scenario from issue #386: ~/.qwen does not exist, so Qwen Code must
    // not appear in the Agent Key Sharing list even though the flow itself
    // is shown (because ~/.openclaw exists).
    render(
      <CustomAgentKeySharePrompt
        onSelect={vi.fn()}
        onCancel={vi.fn()}
        excludedChoices={['qwencode']}
      />,
    );
    expect(getRenderedChoices()).toEqual(['openclaw', 'none']);
  });

  it('hides openclaw when it is in excludedChoices', () => {
    render(
      <CustomAgentKeySharePrompt
        onSelect={vi.fn()}
        onCancel={vi.fn()}
        excludedChoices={['openclaw']}
      />,
    );
    expect(getRenderedChoices()).toEqual(['qwencode', 'none']);
  });

  it('hides all agent options but keeps the manual-config entry', () => {
    render(
      <CustomAgentKeySharePrompt
        onSelect={vi.fn()}
        onCancel={vi.fn()}
        excludedChoices={['openclaw', 'qwencode']}
      />,
    );
    expect(getRenderedChoices()).toEqual(['none']);
  });
});
