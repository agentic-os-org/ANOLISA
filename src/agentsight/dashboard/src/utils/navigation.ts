/**
 * Dashboard navigation model.
 *
 * Single source of truth for the routes the NavBar renders, the capability
 * each one requires, and where a fresh visit lands. `App.tsx` derives both its
 * capability guard and its landing redirect from this table and `NavBar.tsx`
 * renders it, so the navigation order, the default page and the route guard
 * cannot drift apart.
 */
import type { MessageKey } from '../i18n';
import type { AppCapability } from './apiClient';

export type NavRoute = {
  /** Hash-router path. Never `/`: the bare root only redirects. */
  path: string;
  labelKey: MessageKey;
  icon: string;
  capability: AppCapability;
};

/** Agent dashboard — where a fresh visit lands (#2723). */
export const HEALTH_PATH = '/health';

/**
 * Agent observability.
 *
 * Moved off `/` so the bare root can redirect to the landing page instead of
 * rendering a page of its own; the conversation list keeps its own address.
 */
export const OBSERVABILITY_PATH = '/observability';

/** Landing path of last resort, for a capability set that advertises no route. */
export const LOCAL_DEFAULT_PATH = '/sessions';

/** Navigation order. Doubles as the landing-page preference order. */
export const NAV_ROUTES: readonly NavRoute[] = [
  { path: HEALTH_PATH, labelKey: 'nav.agentHealth', icon: '🩺', capability: 'agent_health' },
  { path: OBSERVABILITY_PATH, labelKey: 'nav.agentObservability', icon: '📊', capability: 'agent_observability' },
  { path: '/sessions', labelKey: 'nav.sessions', icon: '🗂️', capability: 'sessions' },
  { path: '/savings', labelKey: 'nav.tokenSavings', icon: '⚡', capability: 'token_savings' },
  { path: '/optimization', labelKey: 'nav.optimization', icon: '🔬', capability: 'optimization' },
  { path: '/skills', labelKey: 'nav.skillMetrics', icon: '🧩', capability: 'skills' },
  { path: '/security', labelKey: 'nav.securityObservability', icon: '🛡️', capability: 'security' },
  { path: '/audit', labelKey: 'nav.systemAudit', icon: '📋', capability: 'system_audit' },
  { path: '/enforcement', labelKey: 'nav.riskEnforcement', icon: '⛔', capability: 'enforcement' },
  { path: '/atif', labelKey: 'nav.trajectoryViewer', icon: '🔍', capability: 'atif' },
  { path: '/settings', labelKey: 'nav.settings', icon: '⚙️', capability: 'settings' },
];

/** Every capability the dashboard has a page for, in navigation order. */
export const ALL_CAPABILITIES: readonly AppCapability[] = NAV_ROUTES.map((route) => route.capability);

/** The nav entry serving `pathname`, or undefined when none owns it. */
function routeFor(pathname: string): NavRoute | undefined {
  return NAV_ROUTES.find(
    (route) => pathname === route.path || pathname.startsWith(`${route.path}/`),
  );
}

/**
 * Whether `pathname` may render under `capabilities`.
 *
 * The bare root renders no page of its own — it redirects to `defaultPath()`,
 * which is capability-checked in its own right — so it is always allowed. A
 * path no nav entry owns is allowed through to the router's catch-all, which
 * redirects too. Matching on a `/` boundary keeps a route from claiming a
 * sibling path that merely shares its prefix (`/audit` vs `/auditorium`).
 */
export function pathAllowed(pathname: string, capabilities: readonly AppCapability[]): boolean {
  if (pathname === '/') return true;
  const route = routeFor(pathname);
  return route === undefined ? true : capabilities.includes(route.capability);
}

/**
 * Where a fresh visit (`#/`) lands: the Agent dashboard, then Agent
 * observability, then the first other advertised page (#2723).
 *
 * Scanning `NAV_ROUTES` instead of a fixed prefix chain keeps the answer
 * inside the advertised capability set, so the redirect target is always a
 * path `pathAllowed` accepts and the guard cannot bounce back and forth.
 */
export function defaultPath(capabilities: readonly AppCapability[]): string {
  const route = NAV_ROUTES.find((item) => capabilities.includes(item.capability));
  return route ? route.path : LOCAL_DEFAULT_PATH;
}
