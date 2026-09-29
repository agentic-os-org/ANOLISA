/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { BaseDeclarativeTool, BaseToolInvocation, Kind } from './tools.js';
import { ToolNames, ToolDisplayNames } from './tool-names.js';
import type { ToolResult, ToolResultDisplay } from './tools.js';
import type { Config } from '../config/config.js';
import type { SkillManager } from '../skills/skill-manager.js';
import type { SkillConfig } from '../skills/types.js';
import { logSkillLaunch, SkillLaunchEvent } from '../telemetry/index.js';
import path from 'path';

export interface SkillParams {
  skill: string;
}

/**
 * Skill tool that enables the model to access skill definitions.
 * The tool dynamically loads available skills and includes them in its description
 * for the model to choose from.
 */
export class SkillTool extends BaseDeclarativeTool<SkillParams, ToolResult> {
  static readonly Name: string = ToolNames.SKILL;

  private skillManager: SkillManager;
  private availableSkills: SkillConfig[] = [];

  /**
   * Monotonic counter used to discard stale `refreshSkills` results when a
   * newer refresh starts before an older one resolves (e.g. a watcher
   * debounce burst on slow disks).
   */
  private refreshSeq = 0;

  /**
   * Tracks whether the initial discovery has produced a result at least
   * once. Once `true`, transient failures will keep the last-known-good
   * `availableSkills` instead of clobbering it with an empty list.
   */
  private hasLoadedOnce = false;

  /**
   * Resolves once the *local* skill discovery has completed. Remote skills
   * are loaded in the background AFTER this resolves so the CLI cold start
   * is never blocked by the remote registry's network round-trip.
   *
   * Callers that need an accurate tool description for the very first LLM
   * request (e.g. `Config.createToolRegistry`, `GeminiClient.startChat`)
   * MUST await this promise before reading `tool.schema`.
   */
  readonly initPromise: Promise<void>;

  constructor(private readonly config: Config) {
    // Initialize with a basic schema first
    const initialSchema = {
      type: 'object',
      properties: {
        skill: {
          type: 'string',
          description: 'The skill name (no arguments). E.g., "pdf" or "xlsx"',
        },
      },
      required: ['skill'],
      additionalProperties: false,
      $schema: 'http://json-schema.org/draft-07/schema#',
    };

    super(
      SkillTool.Name,
      ToolDisplayNames.SKILL,
      'Execute a skill within the main conversation. Loading available skills...', // Initial description
      Kind.Read,
      initialSchema,
      false, // isOutputMarkdown
      false, // canUpdateOutput
    );

    this.skillManager = config.getSkillManager()!;
    this.skillManager.addChangeListener(() => {
      // Watcher / state-toggle triggered: refresh both local + remote.
      void this.refreshSkills({ includeRemote: true });
    });

    // Cold start path:
    //   1. First load LOCAL skills only (fast, never blocks on network).
    //   2. After initPromise resolves, asynchronously enrich with REMOTE
    //      skills so the LLM can see them on subsequent requests.
    this.initPromise = this.refreshSkills({ includeRemote: false }).then(() => {
      // Fire-and-forget remote enrichment; failures are swallowed inside.
      void this.refreshSkills({ includeRemote: true });
    });
  }

  /**
   * Asynchronously initializes the tool by loading available skills
   * and updating the description and schema.
   *
   * Race-safe: if invoked multiple times concurrently (e.g. by the change
   * listener during a watcher burst), only the LATEST invocation is allowed
   * to overwrite `availableSkills` / description.
   */
  async refreshSkills(
    options: { includeRemote?: boolean } = { includeRemote: true },
  ): Promise<void> {
    const seq = ++this.refreshSeq;
    try {
      // Exclude disabled skills so they are hidden from the LLM.
      const skills = await this.skillManager.listSkills({
        includeRemote: options.includeRemote ?? true,
        excludeDisabled: true,
      });

      // Stale guard: a newer refresh has started; drop our result.
      if (seq !== this.refreshSeq) {
        return;
      }

      // Defensive: a misbehaving SkillManager (or a test mock that has
      // been cleared by `vi.clearAllMocks`) may return undefined; never let
      // that propagate into `availableSkills`, which every consumer expects
      // to be an array.
      this.availableSkills = skills ?? [];
      this.hasLoadedOnce = true;
      this.updateDescriptionAndSchema();
    } catch (error) {
      console.warn('Failed to load skills for Skills tool:', error);
      if (seq !== this.refreshSeq) {
        return;
      }
      // IMPORTANT: do NOT clobber a previously-good list with an empty one.
      // Only on the very first load do we initialize to [] so the description
      // gets a proper "No skills are currently configured" message.
      if (!this.hasLoadedOnce) {
        this.availableSkills = [];
        this.hasLoadedOnce = true;
        this.updateDescriptionAndSchema();
      }
    } finally {
      // Update the client with the new tools. setTools() is cheap and safe
      // to call as soon as a chat exists; if the chat isn't ready yet,
      // startChat() will pick up the latest description by reading
      // tool.schema directly.
      const geminiClient = this.config.getGeminiClient();
      if (geminiClient && geminiClient.isInitialized()) {
        try {
          await geminiClient.setTools();
        } catch (error) {
          console.warn('Failed to push refreshed tools to chat:', error);
        }
      }
    }
  }

  /**
   * Updates the tool's description and schema based on available skills.
   */
  private updateDescriptionAndSchema(): void {
    let skillDescriptions = '';
    if (this.availableSkills.length === 0) {
      skillDescriptions =
        'No skills are currently configured. Skills can be created by adding directories with SKILL.md files to .copilot-shell/skills/ or ~/.copilot-shell/skills/.';
    } else {
      skillDescriptions = this.availableSkills
        .map(
          (skill) => `<skill>
<name>
${skill.name}
</name>
<description>
${skill.description} (${skill.isRemote ? 'remote' : skill.level})
</description>
<location>
${skill.isRemote ? 'remote' : skill.level}
</location>
</skill>`,
        )
        .join('\n');
    }

    const baseDescription = `Execute a skill within the main conversation

<skills_instructions>
When users ask you to perform tasks, check if any of the available skills below can help complete the task more effectively. Skills provide specialized capabilities and domain knowledge.

How to invoke:
- Use this tool with the skill name only (no arguments)
- Examples:
  - \`{"skill": "pdf"}\` - invoke the pdf skill
  - \`{"skill": "xlsx"}\` - invoke the xlsx skill
  - \`{"skill": "ms-office-suite:pdf"}\` - invoke using fully qualified name

When asked to install:
- Skills can be installed by adding directories with SKILL.md files to ~/.copilot-shell/skills/
- When users clarify to install in the project, make sure the directory with SKILL.md is added to .copilot-shell/skills/
- If the given URL of a skill looks like a GitHub repo, clone the whole repository and copy the specific skill directory
- Any temporary files should be located in /tmp, and make sure they are removed after use

Important:
- When a skill is relevant, you must invoke this tool IMMEDIATELY as your first action
- NEVER just announce or mention a skill in your text response without actually calling this tool
- This is a BLOCKING REQUIREMENT: invoke the relevant Skill tool BEFORE generating any other response about the task
- Only use skills listed in <available_skills> below
- Do not invoke a skill that is already running
- Do not use this tool for built-in CLI commands (like /help, /clear, etc.)
- When executing scripts or loading referenced files, ALWAYS resolve absolute paths from skill's base directory. Examples:
  - \`bash scripts/init.sh\` -> \`bash /path/to/skill/scripts/init.sh\`
  - \`python scripts/helper.py\` -> \`python /path/to/skill/scripts/helper.py\`
  - \`reference.md\` -> \`/path/to/skill/reference.md\`
</skills_instructions>

<available_skills>
${skillDescriptions}
</available_skills>
`;
    // Update description using object property assignment
    (this as { description: string }).description = baseDescription;
  }

  override validateToolParams(params: SkillParams): string | null {
    // Validate required fields
    if (
      !params.skill ||
      typeof params.skill !== 'string' ||
      params.skill.trim() === ''
    ) {
      return 'Parameter "skill" must be a non-empty string.';
    }

    // Validate that the skill exists (local or remote)
    const skillExists = this.availableSkills.some(
      (skill) => skill.name === params.skill,
    );

    if (!skillExists) {
      const availableNames = this.availableSkills.map((s) => s.name);
      if (availableNames.length === 0) {
        return `Skill "${params.skill}" not found. No skills are currently available.`;
      }
      // Show first 10 skills to avoid too long message
      const displayNames = availableNames.slice(0, 10);
      const suffix =
        availableNames.length > 10
          ? ` (and ${availableNames.length - 10} more)`
          : '';
      return `Skill "${params.skill}" not found. Available skills: ${displayNames.join(', ')}${suffix}`;
    }

    return null;
  }

  protected createInvocation(params: SkillParams) {
    return new SkillToolInvocation(this.config, this.skillManager, params);
  }

  getAvailableSkillNames(): string[] {
    return this.availableSkills.map((skill) => skill.name);
  }

  /**
   * Resolve a skill name to its SkillConfig for PreToolUse hook enrichment.
   * Returns undefined if the skill cannot be found.
   */
  async resolveSkillConfig(
    skillName: string,
  ): Promise<SkillConfig | undefined> {
    try {
      return (
        (await this.skillManager.loadSkillForRuntime(skillName)) ?? undefined
      );
    } catch {
      return undefined;
    }
  }
}

class SkillToolInvocation extends BaseToolInvocation<SkillParams, ToolResult> {
  constructor(
    private readonly config: Config,
    private readonly skillManager: SkillManager,
    params: SkillParams,
  ) {
    super(params);
  }

  getDescription(): string {
    return `Use skill: "${this.params.skill}"`;
  }

  override async shouldConfirmExecute(): Promise<false> {
    // Skill loading is a read-only operation, no confirmation needed
    return false;
  }

  async execute(
    _signal?: AbortSignal,
    _updateOutput?: (output: ToolResultDisplay) => void,
  ): Promise<ToolResult> {
    try {
      // Load the skill with runtime config (includes additional files)
      const skill = await this.skillManager.loadSkillForRuntime(
        this.params.skill,
      );

      if (!skill) {
        // Log failed skill launch
        logSkillLaunch(
          this.config,
          new SkillLaunchEvent(this.params.skill, false),
        );

        // Get parse errors if any
        const parseErrors = this.skillManager.getParseErrors();
        const errorMessages: string[] = [];

        for (const [filePath, error] of parseErrors) {
          if (filePath.includes(this.params.skill)) {
            errorMessages.push(`Parse error at ${filePath}: ${error.message}`);
          }
        }

        const errorDetail =
          errorMessages.length > 0
            ? `\nErrors:\n${errorMessages.join('\n')}`
            : '';

        return {
          llmContent: `Skill "${this.params.skill}" not found.${errorDetail}`,
          returnDisplay: `Skill "${this.params.skill}" not found.${errorDetail}`,
        };
      }

      // Log successful skill launch
      logSkillLaunch(
        this.config,
        new SkillLaunchEvent(this.params.skill, true),
      );

      const baseDir = path.dirname(skill.filePath);

      // Build markdown content for LLM (show base dir, then body)
      const llmContent = `Base directory for this skill: ${baseDir}\nImportant: ALWAYS resolve absolute paths from this base directory when working with skills.\n\n${skill.body}\n`;

      return {
        llmContent: [{ text: llmContent }],
        returnDisplay: skill.description,
      };
    } catch (error) {
      const errorMessage =
        error instanceof Error ? error.message : String(error);
      console.error(`[SkillsTool] Error using skill: ${errorMessage}`);

      // Log failed skill launch
      logSkillLaunch(
        this.config,
        new SkillLaunchEvent(this.params.skill, false),
      );

      return {
        llmContent: `Failed to load skill "${this.params.skill}": ${errorMessage}`,
        returnDisplay: `Failed to load skill "${this.params.skill}": ${errorMessage}`,
      };
    }
  }
}
