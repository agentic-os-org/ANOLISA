import type { Task } from "./index.js";
export function qoderOptions<T extends object>(
  task: Task,
  options?: T,
): {
  options: T;
  bind<Q extends { interrupt(): Promise<void> }>(query: Q): Q;
  readonly failure?: Error;
};
