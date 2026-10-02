export type SecurityTab = 'overview' | 'events' | 'timeline';

export const EVENT_PAGE_SIZE = 25;
export const OVERVIEW_EVENT_SAMPLE_LIMIT = 500;

export const EMPTY_EVENT_FILTERS = {
  category: '',
  result: '',
  verdict: '',
  session_id: '',
  run_id: '',
};

export type SecurityEventFilters = typeof EMPTY_EVENT_FILTERS;
