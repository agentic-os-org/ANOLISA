import type { SecurityContainmentAction } from './apiClient';
import type { MessageKey } from '../i18n';

export interface ContainmentLifecyclePresentation {
  labelKey: MessageKey;
  detailKey: MessageKey;
  style: string;
}

export function containmentLifecyclePresentation(
  action: SecurityContainmentAction,
): ContainmentLifecyclePresentation | null {
  switch (action.lifecycle_state) {
    case 'pending':
      return {
        labelKey: 'cont.lifecycle.pending.label',
        detailKey: 'cont.lifecycle.pending.detail',
        style: 'bg-amber-100 text-amber-700',
      };
    case 'active':
      return action.blocked_at_ns !== null
        ? {
            labelKey: 'cont.lifecycle.activeBlocked.label',
            detailKey: 'cont.lifecycle.activeBlocked.detail',
            style: 'bg-red-100 text-red-700',
          }
        : {
            labelKey: 'cont.lifecycle.activePending.label',
            detailKey: 'cont.lifecycle.activePending.detail',
            style: 'bg-blue-100 text-blue-700',
          };
    case 'expiring':
      return {
        labelKey: 'cont.lifecycle.expiring.label',
        detailKey: 'cont.lifecycle.expiring.detail',
        style: 'bg-amber-100 text-amber-700',
      };
    case 'expired':
      return {
        labelKey: 'cont.lifecycle.expired.label',
        detailKey: 'cont.lifecycle.expired.detail',
        style: 'bg-gray-100 text-gray-700',
      };
    case 'failed':
      return {
        labelKey: 'cont.lifecycle.failed.label',
        detailKey: 'cont.lifecycle.failed.detail',
        style: 'bg-red-100 text-red-700',
      };
    default:
      return null;
  }
}
