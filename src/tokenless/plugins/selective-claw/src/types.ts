export type SelectiveClawConfig = {
  freshTailTurns: number;
  dbPath: string;
  enabled: boolean;
};

export const DEFAULT_CONFIG: SelectiveClawConfig = {
  freshTailTurns: 3,
  dbPath: "",
  enabled: true,
};
