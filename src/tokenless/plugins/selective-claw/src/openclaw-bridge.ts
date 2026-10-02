export type AgentMessage = {
  role: string;
  content?: any;
  timestamp?: number;
  toolCallId?: string;
  toolUseId?: string;
  toolName?: string;
  isError?: boolean;
};

export type ContextEngineInfo = {
  id: string;
  name: string;
  version: string;
  ownsCompaction?: boolean;
};

export type AssembleResult = {
  messages: AgentMessage[];
  estimatedTokens: number;
  systemPromptAddition?: string;
};

export type BootstrapResult = {
  bootstrapped: boolean;
  importedMessages: number;
  reason?: string;
};

export type CompactResult = {
  ok: boolean;
  compacted: boolean;
  reason?: string;
};

export type IngestResult = {
  ingested: boolean;
};

export type ContextEngine = {
  info: ContextEngineInfo;
  bootstrap(params: {
    sessionId: string;
    sessionKey?: string;
    messages?: AgentMessage[];
  }): Promise<BootstrapResult>;
  ingest(params: {
    sessionId: string;
    sessionKey?: string;
    message: AgentMessage;
  }): Promise<IngestResult>;
  assemble(params: {
    sessionId: string;
    sessionKey?: string;
    messages: AgentMessage[];
    tokenBudget?: number;
    prompt?: string;
  }): Promise<AssembleResult>;
  compact(params: {
    sessionId: string;
    sessionKey?: string;
    tokenBudget?: number;
    force?: boolean;
  }): Promise<CompactResult>;
  afterTurn?(params: {
    sessionId: string;
    sessionKey?: string;
    messages?: AgentMessage[];
  }): Promise<void>;
};

export type OpenClawPluginApi = {
  config?: any;
  runtime?: any;
  logger?: any;
  registerContextEngine: (id: string, factory: () => ContextEngine | Promise<ContextEngine>) => void;
  [key: string]: any;
};
