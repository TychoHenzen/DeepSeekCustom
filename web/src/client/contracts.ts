export const workspaces = [
  'chat',
  'autopilot',
  'cascade',
  'evolve',
  'procedure',
  'sessions',
  'tests',
  'settings',
] as const;
export type Workspace = (typeof workspaces)[number];

export type AppErrorCode =
  | 'invalid_command'
  | 'invalid_input'
  | 'conflict'
  | 'operation_active'
  | 'not_found'
  | 'unavailable'
  | 'persistence_failed'
  | 'service_failed';

export interface AppError {
  code: AppErrorCode;
  message: string;
  recoverable: boolean;
  field: string | null;
}

export interface SessionSummary {
  id: string;
  title: string;
  backend: string;
  model: string;
}

export type PendingSessionSwitch =
  | { type: 'new' }
  | { type: 'load'; session_id: string };

export type TranscriptSpan =
  | { type: 'text'; text: string }
  | { type: 'reasoning'; text: string };

export type TranscriptBlock =
  | { id: number; type: 'user'; text: string; has_image: boolean }
  | { id: number; type: 'assistant'; spans: TranscriptSpan[] }
  | {
      id: number;
      type: 'tool_call';
      tool: string;
      args: string;
      output: string | null;
      is_error: boolean;
    }
  | { id: number; type: 'notice'; message: string; level: 'info' | 'warning' | 'error' }
  | { id: number; type: 'error'; message: string; recoverable: boolean }
  | { id: number; type: 'image'; media_type: string; data: string }
  | { id: number; type: 'terminal'; outcome: OperationPhase; message: string }
  | { id: number; type: 'subagent'; name: string; state: string; blocks: TranscriptBlock[] };

export interface VisibleSettings {
  selected_backend: string | null;
  selected_model: string | null;
  effort: string;
  context_budget: number;
  show_raw_output: boolean;
  working_dir: string | null;
  style: {
    plain_language: boolean;
    target_grade: number;
  };
  voice: {
    enabled: boolean;
    stt_enabled: boolean;
    tts_enabled: boolean;
    trigger_mode: string;
    wake_phrase: string;
    tts_voice: string;
    tts_speed: number;
  };
}

export type OperationKind =
  | 'chat'
  | 'autopilot'
  | 'cascade'
  | 'evolve'
  | 'procedure'
  | 'voice'
  | 'tests'
  | 'folder_picker';
export type OperationPhase =
  | 'idle'
  | 'running'
  | 'awaiting_review'
  | 'completed'
  | 'failed'
  | 'interrupted'
  | 'cancelled';

export interface OperationState {
  kind: OperationKind;
  operation_id: string | null;
  phase: OperationPhase;
  progress: { completed: number; total: number | null } | null;
  message: string | null;
  error: AppError | null;
}

export interface AppSnapshot {
  revision: number;
  workspace: Workspace;
  transcript: TranscriptBlock[];
  session: SessionSummary;
  pending_session_switch: PendingSessionSwitch | null;
  settings: VisibleSettings;
  operations: OperationState[];
}

export type AppChange =
  | { revision: number; type: 'reset'; value: AppSnapshot }
  | { revision: number; type: 'workspace_selected'; value: Workspace }
  | { revision: number; type: 'transcript_appended'; value: TranscriptBlock }
  | { revision: number; type: 'session_changed'; value: SessionSummary }
  | { revision: number; type: 'pending_session_switch_changed'; value: PendingSessionSwitch | null }
  | { revision: number; type: 'settings_changed'; value: VisibleSettings }
  | { revision: number; type: 'operation_changed'; value: OperationState }
  | { revision: number; type: 'error'; value: AppError };

export type AppCommand =
  | { command: 'select_workspace'; payload: { workspace: Workspace } }
  | { command: 'send_message'; payload: { text: string; attachment_id: string | null } }
  | { command: 'stop_operation'; payload: { kind: OperationKind } }
  | { command: 'new_session' }
  | { command: 'load_session'; payload: { session_id: string } }
  | { command: 'delete_session'; payload: { session_id: string } }
  | { command: 'select_backend'; payload: { backend: string; model: string } }
  | { command: 'update_settings'; payload: { settings: VisibleSettings } }
  | { command: 'start_autopilot'; payload: { task: string; iterations: number } }
  | { command: 'start_cascade'; payload: { prompt: string } }
  | { command: 'start_evolve'; payload: { prompt: string } }
  | { command: 'run_procedure'; payload: { change_id: string; task_id: string } }
  | { command: 'review_procedure'; payload: { run_id: string; decision: 'approve' | 'reject' } }
  | { command: 'start_voice_capture' }
  | { command: 'stop_voice_capture' }
  | { command: 'pick_working_directory' };

export type AppCommandResult =
  | { status: 'applied'; revision: number }
  | { status: 'conflict'; current_revision: number }
  | { status: 'rejected'; error: AppError };

const errorCodes: readonly AppErrorCode[] = [
  'invalid_command',
  'invalid_input',
  'conflict',
  'operation_active',
  'not_found',
  'unavailable',
  'persistence_failed',
  'service_failed',
];
const operationKinds: readonly OperationKind[] = [
  'chat',
  'autopilot',
  'cascade',
  'evolve',
  'procedure',
  'voice',
  'tests',
  'folder_picker',
];
const operationPhases: readonly OperationPhase[] = [
  'idle',
  'running',
  'awaiting_review',
  'completed',
  'failed',
  'interrupted',
  'cancelled',
];

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new Error('expected an object');
  }
  return value as Record<string, unknown>;
}

function string(value: unknown, name: string): string {
  if (typeof value !== 'string') throw new Error(`${name} must be a string`);
  return value;
}

function boolean(value: unknown, name: string): boolean {
  if (typeof value !== 'boolean') throw new Error(`${name} must be a boolean`);
  return value;
}

function number(value: unknown, name: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) throw new Error(`${name} must be a number`);
  return value;
}

function integer(value: unknown, name: string): number {
  const parsed = number(value, name);
  if (!Number.isSafeInteger(parsed) || parsed < 0) throw new Error(`${name} must be a safe unsigned integer`);
  return parsed;
}

function nullable<T>(value: unknown, parse: (entry: unknown) => T): T | null {
  return value === null ? null : parse(value);
}

function enumValue<T extends string>(value: unknown, values: readonly T[], name: string): T {
  if (typeof value !== 'string' || !values.includes(value as T)) throw new Error(`${name} is not supported`);
  return value as T;
}

function parseError(value: unknown): AppError {
  const item = record(value);
  return {
    code: enumValue(item.code, errorCodes, 'error code'),
    message: string(item.message, 'error message'),
    recoverable: boolean(item.recoverable, 'error recoverable'),
    field: nullable(item.field, (field) => string(field, 'error field')),
  };
}

function parseSession(value: unknown): SessionSummary {
  const item = record(value);
  return {
    id: string(item.id, 'session id'),
    title: string(item.title, 'session title'),
    backend: string(item.backend, 'session backend'),
    model: string(item.model, 'session model'),
  };
}

function parsePending(value: unknown): PendingSessionSwitch {
  const item = record(value);
  const type = string(item.type, 'pending session type');
  if (type === 'new') return { type };
  if (type === 'load') return { type, session_id: string(item.session_id, 'pending session id') };
  throw new Error('pending session type is not supported');
}

function parseSpan(value: unknown): TranscriptSpan {
  const item = record(value);
  const type = string(item.type, 'transcript span type');
  if (type !== 'text' && type !== 'reasoning') throw new Error('transcript span type is not supported');
  return { type, text: string(item.text, 'transcript span text') };
}

function parseBlock(value: unknown): TranscriptBlock {
  const item = record(value);
  const id = integer(item.id, 'transcript block id');
  const type = string(item.type, 'transcript block type');
  switch (type) {
    case 'user': return { id, type, text: string(item.text, 'user text'), has_image: boolean(item.has_image, 'user has_image') };
    case 'assistant': {
      if (!Array.isArray(item.spans)) throw new Error('assistant spans must be an array');
      return { id, type, spans: item.spans.map(parseSpan) };
    }
    case 'tool_call': return { id, type, tool: string(item.tool, 'tool name'), args: string(item.args, 'tool args'), output: nullable(item.output, (output) => string(output, 'tool output')), is_error: boolean(item.is_error, 'tool is_error') };
    case 'notice': return { id, type, message: string(item.message, 'notice message'), level: enumValue(item.level, ['info', 'warning', 'error'] as const, 'notice level') };
    case 'error': return { id, type, message: string(item.message, 'error message'), recoverable: boolean(item.recoverable, 'error recoverable') };
    case 'image': return { id, type, media_type: string(item.media_type, 'image media type'), data: string(item.data, 'image data') };
    case 'terminal': return { id, type, outcome: enumValue(item.outcome, operationPhases, 'terminal outcome'), message: string(item.message, 'terminal message') };
    case 'subagent': {
      if (!Array.isArray(item.blocks)) throw new Error('subagent blocks must be an array');
      return { id, type, name: string(item.name, 'subagent name'), state: string(item.state, 'subagent state'), blocks: item.blocks.map(parseBlock) };
    }
    default: throw new Error('transcript block type is not supported');
  }
}

function parseSettings(value: unknown): VisibleSettings {
  const item = record(value);
  const style = record(item.style);
  const voice = record(item.voice);
  return {
    selected_backend: nullable(item.selected_backend, (entry) => string(entry, 'selected backend')),
    selected_model: nullable(item.selected_model, (entry) => string(entry, 'selected model')),
    effort: string(item.effort, 'effort'),
    context_budget: integer(item.context_budget, 'context budget'),
    show_raw_output: boolean(item.show_raw_output, 'show raw output'),
    working_dir: nullable(item.working_dir, (entry) => string(entry, 'working directory')),
    style: {
      plain_language: boolean(style.plain_language, 'plain language'),
      target_grade: number(style.target_grade, 'target grade'),
    },
    voice: {
      enabled: boolean(voice.enabled, 'voice enabled'),
      stt_enabled: boolean(voice.stt_enabled, 'stt enabled'),
      tts_enabled: boolean(voice.tts_enabled, 'tts enabled'),
      trigger_mode: string(voice.trigger_mode, 'voice trigger mode'),
      wake_phrase: string(voice.wake_phrase, 'wake phrase'),
      tts_voice: string(voice.tts_voice, 'tts voice'),
      tts_speed: number(voice.tts_speed, 'tts speed'),
    },
  };
}

function parseOperation(value: unknown): OperationState {
  const item = record(value);
  return {
    kind: enumValue(item.kind, operationKinds, 'operation kind'),
    operation_id: nullable(item.operation_id, (entry) => string(entry, 'operation id')),
    phase: enumValue(item.phase, operationPhases, 'operation phase'),
    progress: nullable(item.progress, (entry) => {
      const progress = record(entry);
      return { completed: integer(progress.completed, 'completed progress'), total: nullable(progress.total, (total) => integer(total, 'total progress')) };
    }),
    message: nullable(item.message, (entry) => string(entry, 'operation message')),
    error: nullable(item.error, parseError),
  };
}

export function parseSnapshot(value: unknown): AppSnapshot {
  const item = record(value);
  if (!Array.isArray(item.transcript) || !Array.isArray(item.operations)) throw new Error('snapshot collections must be arrays');
  return {
    revision: integer(item.revision, 'snapshot revision'),
    workspace: enumValue(item.workspace, workspaces, 'workspace'),
    transcript: item.transcript.map(parseBlock),
    session: parseSession(item.session),
    pending_session_switch: nullable(item.pending_session_switch, parsePending),
    settings: parseSettings(item.settings),
    operations: item.operations.map(parseOperation),
  };
}

export function parseChange(value: unknown): AppChange {
  const item = record(value);
  const revision = integer(item.revision, 'change revision');
  const type = string(item.type, 'change type');
  switch (type) {
    case 'reset': return { revision, type, value: parseSnapshot(item.value) };
    case 'workspace_selected': return { revision, type, value: enumValue(item.value, workspaces, 'workspace') };
    case 'transcript_appended': return { revision, type, value: parseBlock(item.value) };
    case 'session_changed': return { revision, type, value: parseSession(item.value) };
    case 'pending_session_switch_changed': return { revision, type, value: nullable(item.value, parsePending) };
    case 'settings_changed': return { revision, type, value: parseSettings(item.value) };
    case 'operation_changed': return { revision, type, value: parseOperation(item.value) };
    case 'error': return { revision, type, value: parseError(item.value) };
    default: throw new Error('change type is not supported');
  }
}

export function parseCommandResult(value: unknown): AppCommandResult {
  const item = record(value);
  const status = string(item.status, 'command status');
  if (status === 'applied') return { status, revision: integer(item.revision, 'applied revision') };
  if (status === 'conflict') return { status, current_revision: integer(item.current_revision, 'conflict revision') };
  if (status === 'rejected') return { status, error: parseError(item.error) };
  throw new Error('command status is not supported');
}
