export type SessionGridColumnId =
  | 'name'
  | 'started'
  | 'duration'
  | 'repository'
  | 'agent'
  | 'parent'
  | 'model'
  | 'turns'
  | 'tools'
  | 'input'
  | 'cached'
  | 'output'
  | 'total'
  | 'cost';

export interface SessionGridColumn {
  id: SessionGridColumnId;
  label: string;
  width: string;
  sortable: boolean;
  align?: 'right';
}

export const SESSION_GRID_COLUMNS: readonly SessionGridColumn[] = [
  { id: 'name', label: 'Name', width: 'minmax(12rem, 2.4fr)', sortable: true },
  { id: 'started', label: 'Started', width: '9.5rem', sortable: true },
  { id: 'duration', label: 'Duration', width: '6.5rem', sortable: true, align: 'right' },
  { id: 'repository', label: 'Repository', width: 'minmax(8rem, 1fr)', sortable: true },
  { id: 'agent', label: 'Agent', width: 'minmax(7rem, 1fr)', sortable: true },
  { id: 'parent', label: 'Parent', width: 'minmax(8rem, 1.2fr)', sortable: true },
  { id: 'model', label: 'Model', width: 'minmax(8rem, 1.1fr)', sortable: true },
  { id: 'turns', label: 'Turns', width: '5rem', sortable: true, align: 'right' },
  { id: 'tools', label: 'Tools', width: '5rem', sortable: true, align: 'right' },
  { id: 'input', label: 'Input', width: '6.5rem', sortable: true, align: 'right' },
  { id: 'cached', label: 'Cached', width: '6.5rem', sortable: true, align: 'right' },
  { id: 'output', label: 'Output', width: '6.5rem', sortable: true, align: 'right' },
  { id: 'total', label: 'Total tok', width: '7rem', sortable: true, align: 'right' },
  { id: 'cost', label: 'Cost', width: '6.5rem', sortable: true, align: 'right' },
];

const STORAGE_KEY = 'sessionGridPreferences.v1';
const ALL_COLUMN_IDS = SESSION_GRID_COLUMNS.map((column) => column.id);
/// Columns shown before the user picks. The per-run detail columns
/// (duration/agent/parent/turns/tools) are available in the picker but off by
/// default: fourteen columns is a worse first view than nine, and stored
/// preferences from before they existed stay valid either way.
export const DEFAULT_COLUMN_IDS: readonly SessionGridColumnId[] = [
  'name',
  'started',
  'repository',
  'model',
  'input',
  'cached',
  'output',
  'total',
  'cost',
];

interface StoredPreferences {
  columns?: unknown;
  groupByRepository?: unknown;
  colorByModelProvider?: unknown;
  flattenSubagents?: unknown;
}

function validColumns(value: unknown): SessionGridColumnId[] | null {
  if (!Array.isArray(value)) return null;
  const known = new Set<SessionGridColumnId>(ALL_COLUMN_IDS);
  const unique = value.filter((id): id is SessionGridColumnId => typeof id === 'string' && known.has(id as SessionGridColumnId));
  if (unique.length === 0 || new Set(unique).size !== unique.length || !unique.includes('name')) return null;
  return unique;
}

function loadPreferences(): {
  columns: SessionGridColumnId[];
  groupByRepository: boolean;
  colorByModelProvider: boolean;
  flattenSubagents: boolean;
} {
  try {
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '{}') as StoredPreferences;
    return {
      columns: validColumns(parsed.columns) ?? [...DEFAULT_COLUMN_IDS],
      groupByRepository: parsed.groupByRepository === true,
      colorByModelProvider: parsed.colorByModelProvider === true,
      flattenSubagents: parsed.flattenSubagents === true,
    };
  } catch {
    return {
      columns: [...DEFAULT_COLUMN_IDS],
      groupByRepository: false,
      colorByModelProvider: false,
      flattenSubagents: false,
    };
  }
}

function createSessionGridStore() {
  const saved = loadPreferences();
  let columnIds = $state<SessionGridColumnId[]>(saved.columns);
  let groupByRepository = $state(saved.groupByRepository);
  let colorByModelProvider = $state(saved.colorByModelProvider);
  let flattenSubagents = $state(saved.flattenSubagents);

  function persist() {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({ columns: columnIds, groupByRepository, colorByModelProvider, flattenSubagents }),
    );
  }

  function setVisible(id: SessionGridColumnId, visible: boolean) {
    if (id === 'name') return;
    columnIds = visible
      ? [...columnIds, id]
      : columnIds.filter((columnId) => columnId !== id);
    persist();
  }

  function move(id: SessionGridColumnId, direction: -1 | 1) {
    const index = columnIds.indexOf(id);
    const next = index + direction;
    if (index < 0 || next < 0 || next >= columnIds.length) return;
    const reordered = [...columnIds];
    [reordered[index], reordered[next]] = [reordered[next], reordered[index]];
    columnIds = reordered;
    persist();
  }

  function setGroupByRepository(next: boolean) {
    groupByRepository = next;
    persist();
  }

  function setColorByModelProvider(next: boolean) {
    colorByModelProvider = next;
    persist();
  }

  function setFlattenSubagents(next: boolean) {
    flattenSubagents = next;
    persist();
  }

  function reset() {
    columnIds = [...DEFAULT_COLUMN_IDS];
    groupByRepository = false;
    colorByModelProvider = false;
    flattenSubagents = false;
    persist();
  }

  return {
    get columns() {
      return columnIds.map((id) => SESSION_GRID_COLUMNS.find((column) => column.id === id)!);
    },
    get columnIds() {
      return columnIds;
    },
    get groupByRepository() {
      return groupByRepository;
    },
    get colorByModelProvider() {
      return colorByModelProvider;
    },
    get flattenSubagents() {
      return flattenSubagents;
    },
    setVisible,
    move,
    setGroupByRepository,
    setColorByModelProvider,
    setFlattenSubagents,
    reset,
  };
}

export const sessionGridStore = createSessionGridStore();
