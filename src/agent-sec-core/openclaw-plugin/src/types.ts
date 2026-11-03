// types.ts
import type { OpenClawPluginApi } from "openclaw/plugin-sdk/plugin-entry";

/**
 * 安全能力的元数据 + 注册契约。
 *
 * 每个能力自行拥有 api.on() 调用、错误处理和 hook 特定返回类型。
 * 框架只读取 id（配置解析）和 hooks（日志记录）。
 */
export type SecurityCapability = {
  /** 唯一标识 — 用于配置解析和日志 */
  id: string;
  /** 可读名称 */
  name: string;
  /** Supported hooks; runtime registration may select a version-specific subset. */
  hooks: string[];
  /** 注册函数 — 插件启动时调用一次（若该能力已启用） */
  register: (api: OpenClawPluginApi) => void;
};
