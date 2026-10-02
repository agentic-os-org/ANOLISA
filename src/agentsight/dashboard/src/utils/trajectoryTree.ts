// Subagent trajectory tree: flatten an ATIF document into a navigable node
// tree, lay it out for the horizontal graph view, and round-trip the selected
// node through the URL.
//
// All functions here are pure — no React, no fetching. The whole tree arrives
// in a single API response (the server injects each subagent's stored ATIF
// document, which itself carries its own embedded subagents), so selecting a
// node never needs a network round trip.

import type { AtifDocument, AtifStep, SubagentTrajectoryRef } from '../types';

/** One trajectory in the tree. `doc` is null for external (non-embedded) refs. */
export interface TrajNode {
  /** Trajectory-id chain from the root's children down to this node. Root is `[]`. */
  path: string[];
  /** Stable key for React / lookups: the encoded path. */
  key: string;
  label: string;
  /** Tooltip detail — the document's notes, or the external session id. */
  detail?: string;
  depth: number;
  doc: AtifDocument | null;
  stepCount: number;
  promptTokens: number;
  /** Set when the subagent is referenced by session id but not embedded. */
  externalSessionId?: string;
  children: TrajNode[];
}

const AGENT_NAME_PREFIX = 'agentsight-opt:';

function stepsOf(doc: AtifDocument): AtifStep[] {
  return Array.isArray(doc.steps) ? doc.steps : [];
}

function subagentsOf(doc: AtifDocument): AtifDocument[] {
  return Array.isArray(doc.subagent_trajectories) ? doc.subagent_trajectories : [];
}

function observationResultsOf(step: AtifStep) {
  return Array.isArray(step.observation?.results) ? step.observation.results : [];
}

function subagentRefsOf(result: { subagent_trajectory_ref?: SubagentTrajectoryRef[] }) {
  return Array.isArray(result.subagent_trajectory_ref) ? result.subagent_trajectory_ref : [];
}

function displayLabel(doc: AtifDocument): string {
  const name = doc.agent?.name ?? 'subagent';
  return name.startsWith(AGENT_NAME_PREFIX) ? name.slice(AGENT_NAME_PREFIX.length) : name;
}

/** Identity of an embedded subagent, preferring the id that refs point at. */
function subagentKey(doc: AtifDocument, index: number): string {
  return doc.trajectory_id ?? doc.session_id ?? `#${index}`;
}

function sumPromptTokens(doc: AtifDocument): number {
  const total = doc.final_metrics?.total_prompt_tokens;
  if (total != null) return total;
  return stepsOf(doc).reduce((acc, s) => acc + (s.metrics?.prompt_tokens ?? 0), 0);
}

/** Every subagent ref appearing in a document's steps, in step order. */
function collectRefs(doc: AtifDocument): SubagentTrajectoryRef[] {
  const refs: SubagentTrajectoryRef[] = [];
  for (const step of stepsOf(doc)) {
    for (const result of observationResultsOf(step)) {
      for (const ref of subagentRefsOf(result)) refs.push(ref);
    }
  }
  return refs;
}

function buildNode(doc: AtifDocument, path: string[], depth: number): TrajNode {
  const embedded = subagentsOf(doc);
  const children: TrajNode[] = embedded.map((sub, i) => {
    const key = subagentKey(sub, i);
    return buildNode(sub, [...path, key], depth + 1);
  });

  // Refs that name a session but have no embedded document become external
  // nodes: reachable, but explicitly marked as living outside this page.
  const embeddedIds = new Set<string>();
  embedded.forEach((sub, i) => {
    embeddedIds.add(subagentKey(sub, i));
    if (sub.trajectory_id) embeddedIds.add(sub.trajectory_id);
    if (sub.session_id) embeddedIds.add(sub.session_id);
  });
  const seenExternal = new Set<string>();
  for (const ref of collectRefs(doc)) {
    const refId = ref.trajectory_id ?? ref.session_id;
    if (!refId || embeddedIds.has(refId)) continue;
    if (!ref.session_id || seenExternal.has(ref.session_id)) continue;
    seenExternal.add(ref.session_id);
    children.push({
      path: [...path, ref.session_id],
      key: encodeNodePath([...path, ref.session_id]),
      label: shortLabel(ref.session_id),
      // Data only — the display layer localizes external-node tooltips
      // via `externalSessionId`.
      detail: ref.session_id,
      depth: depth + 1,
      doc: null,
      stepCount: 0,
      promptTokens: 0,
      externalSessionId: ref.session_id,
      children: [],
    });
  }

  return {
    path,
    key: encodeNodePath(path),
    label: displayLabel(doc),
    detail: doc.notes,
    depth,
    doc,
    stepCount: stepsOf(doc).length,
    promptTokens: sumPromptTokens(doc),
    children,
  };
}

function shortLabel(id: string): string {
  return id.length > 18 ? id.slice(0, 18) + '…' : id;
}

/** Build the tree rooted at `doc`. Returns null when there are no subagents. */
export function buildTrajectoryTree(doc: AtifDocument | null): TrajNode | null {
  if (!doc) return null;
  const root = buildNode(doc, [], 0);
  return root.children.length > 0 ? root : null;
}

// ─── URL round-trip ──────────────────────────────────────────────────────────
// Segments are percent-encoded individually so ids containing '/' survive.

export function encodeNodePath(path: string[]): string {
  return path.map(encodeURIComponent).join('/');
}

export function decodeNodePath(encoded: string | null): string[] {
  if (!encoded) return [];
  return encoded
    .split('/')
    .filter(Boolean)
    .map(seg => {
      try {
        return decodeURIComponent(seg);
      } catch {
        return seg;
      }
    });
}

/** Walk `path` from the root. Unresolvable paths fall back to the root node. */
export function findNodeByPath(root: TrajNode, path: string[]): TrajNode {
  let node = root;
  for (const segment of path) {
    const next = node.children.find(c => c.path[c.path.length - 1] === segment);
    if (!next) return node;
    node = next;
  }
  return node;
}

/**
 * Resolve the node a step's `subagent_trajectory_ref` points at, searching the
 * whole tree so a ref inside a nested trajectory still resolves.
 */
export function findNodeByRef(root: TrajNode, ref: SubagentTrajectoryRef): TrajNode | null {
  const wanted = [ref.trajectory_id, ref.session_id].filter(Boolean) as string[];
  if (wanted.length === 0) return null;

  const stack: TrajNode[] = [root];
  while (stack.length > 0) {
    const node = stack.pop()!;
    const last = node.path[node.path.length - 1];
    const ids = [last, node.doc?.trajectory_id, node.doc?.session_id, node.externalSessionId];
    if (wanted.some(w => ids.includes(w))) return node;
    stack.push(...node.children);
  }
  return null;
}

// ─── Layout ──────────────────────────────────────────────────────────────────
// Simplified Reingold–Tilford: depth picks the column, a post-order walk hands
// each leaf the next row, and every parent centers on its children.

export const NODE_W = 170;
export const NODE_H = 48;
const COL_GAP = 58;
const ROW_GAP = 12;
const PAD_X = 12;
const PAD_Y = 12;

export interface PositionedNode {
  node: TrajNode;
  x: number;
  y: number;
}

export interface LayoutEdge {
  fromKey: string;
  toKey: string;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

export interface GraphLayout {
  nodes: PositionedNode[];
  edges: LayoutEdge[];
  width: number;
  height: number;
}

export function layoutTree(root: TrajNode): GraphLayout {
  const nodes: PositionedNode[] = [];
  const centerByKey = new Map<string, number>();
  let nextRow = 0;

  const place = (node: TrajNode): number => {
    const x = PAD_X + node.depth * (NODE_W + COL_GAP);
    let centerY: number;
    if (node.children.length === 0) {
      centerY = PAD_Y + nextRow * (NODE_H + ROW_GAP) + NODE_H / 2;
      nextRow += 1;
    } else {
      const childCenters = node.children.map(place);
      centerY = (childCenters[0] + childCenters[childCenters.length - 1]) / 2;
    }
    nodes.push({ node, x, y: centerY - NODE_H / 2 });
    centerByKey.set(node.key, centerY);
    return centerY;
  };
  place(root);

  const edges: LayoutEdge[] = [];
  const walk = (node: TrajNode) => {
    const x1 = PAD_X + node.depth * (NODE_W + COL_GAP) + NODE_W;
    const y1 = centerByKey.get(node.key) ?? 0;
    for (const child of node.children) {
      edges.push({
        fromKey: node.key,
        toKey: child.key,
        x1,
        y1,
        x2: PAD_X + child.depth * (NODE_W + COL_GAP),
        y2: centerByKey.get(child.key) ?? 0,
      });
      walk(child);
    }
  };
  walk(root);

  const maxDepth = nodes.reduce((m, n) => Math.max(m, n.node.depth), 0);
  return {
    nodes,
    edges,
    width: PAD_X * 2 + (maxDepth + 1) * NODE_W + maxDepth * COL_GAP,
    height: PAD_Y * 2 + nextRow * (NODE_H + ROW_GAP) - (nextRow > 0 ? ROW_GAP : 0),
  };
}

/** Keys of the nodes on the root→selected path, for highlighting. */
export function pathKeys(path: string[]): Set<string> {
  const keys = new Set<string>([encodeNodePath([])]);
  for (let i = 1; i <= path.length; i++) keys.add(encodeNodePath(path.slice(0, i)));
  return keys;
}
