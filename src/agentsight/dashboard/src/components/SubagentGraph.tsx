// Horizontal subagent topology graph. Replaces the old card grid + breadcrumb:
// the highlighted root→selected path *is* the breadcrumb, and selecting a node
// re-renders the trajectory below without leaving the page.

import React from 'react';
import type { TrajNode, PositionedNode } from '../utils/trajectoryTree';
import { layoutTree, pathKeys, encodeNodePath, NODE_W, NODE_H } from '../utils/trajectoryTree';
import { useI18n } from '../i18n';

interface SubagentGraphProps {
  root: TrajNode;
  selectedPath: string[];
  onSelect: (node: TrajNode) => void;
}

function fmtTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

/** Horizontal cubic bezier between two node edges. */
function edgePath(x1: number, y1: number, x2: number, y2: number): string {
  const mid = x1 + (x2 - x1) / 2;
  return `M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`;
}

const NodeBox: React.FC<{
  positioned: PositionedNode;
  isSelected: boolean;
  isOnPath: boolean;
  onSelect: () => void;
}> = ({ positioned, isSelected, isOnPath, onSelect }) => {
  const { t } = useI18n();
  const { node, x, y } = positioned;
  const isExternal = !!node.externalSessionId;

  const fill = isSelected ? '#eef2ff' : isOnPath ? '#f8fafc' : '#ffffff';
  const stroke = isSelected ? '#6366f1' : isOnPath ? '#94a3b8' : '#e2e8f0';

  return (
    <g
      onClick={onSelect}
      className="cursor-pointer"
      role="button"
      tabIndex={0}
      onKeyDown={e => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onSelect();
        }
      }}
    >
      <title>
        {isExternal
          ? t('comp.externalTrajectoryDetail', { id: node.externalSessionId ?? '' })
          : node.detail ?? node.label}
      </title>
      <rect
        x={x}
        y={y}
        width={NODE_W}
        height={NODE_H}
        rx={8}
        fill={fill}
        stroke={stroke}
        strokeWidth={isSelected ? 2 : 1}
        strokeDasharray={isExternal ? '4 3' : undefined}
      />
      <text
        x={x + 10}
        y={y + 19}
        className="text-xs"
        fill={isSelected ? '#4338ca' : '#1f2937'}
        style={{ fontSize: 12, fontWeight: 500 }}
      >
        {node.label.length > 20 ? node.label.slice(0, 20) + '…' : node.label}
      </text>
      <text x={x + 10} y={y + 36} fill="#94a3b8" style={{ fontSize: 11 }}>
        {isExternal
          ? t('comp.externalTrajectory')
          : `${t('comp.subagentSteps', { n: node.stepCount })}${node.promptTokens > 0 ? ` · ${fmtTokens(node.promptTokens)} in` : ''}`}
      </text>
    </g>
  );
};

export const SubagentGraph: React.FC<SubagentGraphProps> = ({ root, selectedPath, onSelect }) => {
  const { t } = useI18n();
  const layout = React.useMemo(() => layoutTree(root), [root]);
  const onPath = React.useMemo(() => pathKeys(selectedPath), [selectedPath]);
  const selectedKey = encodeNodePath(selectedPath);
  const subagentCount = layout.nodes.length - 1;

  return (
    <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
      <h3 className="text-sm font-semibold text-gray-900 mb-3">
        {t('comp.subagentTopology')}
        <span className="ml-2 text-xs font-normal text-gray-400">
          {t('comp.subagentCount', { n: subagentCount })}
        </span>
      </h3>
      <div className="overflow-x-auto flex">
        <svg width={layout.width} height={layout.height} className="block m-auto">
          {layout.edges.map(e => {
            const highlighted = onPath.has(e.fromKey) && onPath.has(e.toKey);
            return (
              <path
                key={`${e.fromKey}->${e.toKey}`}
                d={edgePath(e.x1, e.y1, e.x2, e.y2)}
                fill="none"
                stroke={highlighted ? '#6366f1' : '#cbd5e1'}
                strokeWidth={highlighted ? 2 : 1.5}
              />
            );
          })}
          {layout.nodes.map(p => (
            <NodeBox
              key={p.node.key}
              positioned={p}
              isSelected={p.node.key === selectedKey}
              isOnPath={onPath.has(p.node.key)}
              onSelect={() => onSelect(p.node)}
            />
          ))}
        </svg>
      </div>
    </div>
  );
};
