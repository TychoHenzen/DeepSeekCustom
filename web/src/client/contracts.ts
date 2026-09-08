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

export type ControlledDevelopmentPhase =
  | 'off'
  | 'planning'
  | 'awaiting_approval'
  | 'executing'
  | 'completed'
  | 'blocked'
  | 'interrupted';

export interface WorkCard {
  id: string;
  outcome: string;
  proof_commands: string[];
  production_paths: string[];
  supporting_paths: string[];
  excluded: string[];
  complexity_exceptions: string[];
}

export interface ControlledDevelopmentProofResult {
  command: string;
  disposition: string;
  success: boolean | null;
  exit_code: number | null;
}

export interface ControlledDevelopmentRawDetail {
  kind: 'backend_event' | 'verifier_stdout' | 'verifier_stderr' | 'verifier_combined_output' | 'verifier_error' | 'failure';
  name: string;
  content: string;
  truncated_at_source: boolean;
  bytes_seen: number;
}

export interface ControlledDevelopmentState {
  enabled: boolean;
  phase: ControlledDevelopmentPhase;
  packet_id: string | null;
  card: WorkCard | null;
  structural_errors: Array<{ field: string; message: string }>;
  changed_paths: string[];
  proof_results: ControlledDevelopmentProofResult[];
  progress_notice: string;
  completion_summary: string | null;
  compact_result: string | null;
  blocker: string | null;
  raw_details: ControlledDevelopmentRawDetail[];
  retained_evidence: boolean;
  limitation: string;
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
  backends: Array<{ name: string; configured_model: string; models: string[] }>;
  selected_backend: string | null;
  selected_model: string | null;
  effort: string;
  context_budget: number;
  show_raw_output: boolean;
  max_tokens: number;
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
  procedure: {
    localization_backend: string | null;
    local_patch_backend: string | null;
    frontier_patch_backend: string | null;
    index_max_files: number;
    index_max_total_bytes: number;
    verifier_commands: string[];
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

export interface RetainedTestResult {
  run_id: string;
  identity: TestIdentity;
  command: string[];
  working_dir: string;
  started_at_ms: number;
  duration_ms: number;
  outcome: 'passed' | 'failed' | 'cancelled' | 'infrastructure_error';
  counts: { passed: number; failed: number; ignored: number; filtered: number };
  exit_code: number | null;
  failed_tests: string[];
  output: string;
  omitted_output_bytes: number;
}

export type TestScope =
  | { type: 'full_workspace' }
  | { type: 'module'; module: string }
  | { type: 'exact'; module: string; test: string };
export interface TestIdentity { name: string; scope: TestScope }
export interface TestModule { name: string; tests: TestIdentity[] }
export interface TestCatalogue {
  discovered_at_ms: number;
  full_workspace: TestIdentity;
  modules: TestModule[];
}
interface ActiveTestRun {
  run_id: string;
  identity: TestIdentity;
  command: string[];
  working_dir: string;
  started_at_ms: number;
  elapsed_ms: number;
  running: boolean;
  counts: RetainedTestResult['counts'];
  output: string;
  omitted_output_bytes: number;
}
export interface TestControlSnapshot {
  discovery: {
    catalogue: TestCatalogue | null;
    catalogue_stale: boolean;
    failure: null | {
      command: string[];
      working_dir: string;
      exit_code: number | null;
      diagnostic_output: string;
      omitted_output_bytes: number;
    };
  };
  active: ActiveTestRun | null;
  latest_result: RetainedTestResult | null;
  retained_results: RetainedTestResult[];
  retained_result_warnings: string[];
}

export interface AppSnapshot {
  revision: number;
  workspace: Workspace;
  transcript: TranscriptBlock[];
  session: SessionSummary;
  saved_sessions: SessionSummary[];
  pending_session_switch: PendingSessionSwitch | null;
  settings: VisibleSettings;
  operations: OperationState[];
  controlled_development: ControlledDevelopmentState;
  tests?: TestControlSnapshot;
}

export type AppChange =
  | { revision: number; type: 'reset'; value: AppSnapshot }
  | { revision: number; type: 'workspace_selected'; value: Workspace }
  | { revision: number; type: 'transcript_appended'; value: TranscriptBlock }
  | { revision: number; type: 'session_changed'; value: SessionSummary }
  | { revision: number; type: 'saved_sessions_changed'; value: SessionSummary[] }
  | { revision: number; type: 'pending_session_switch_changed'; value: PendingSessionSwitch | null }
  | { revision: number; type: 'settings_changed'; value: VisibleSettings }
  | { revision: number; type: 'operation_changed'; value: OperationState }
  | { revision: number; type: 'controlled_development_changed'; value: ControlledDevelopmentState }
  | { revision: number; type: 'tests_changed'; value: TestControlSnapshot }
  | { revision: number; type: 'error'; value: AppError };

export type AppCommand =
  | { command: 'select_workspace'; payload: { workspace: Workspace } }
  | { command: 'send_message'; payload: { text: string; attachment_id: string | null } }
  | { command: 'set_controlled_development_enabled'; payload: { session_id: string; enabled: boolean } }
  | { command: 'approve_controlled_development'; payload: { session_id: string; card_id: string } }
  | { command: 'reject_controlled_development'; payload: { session_id: string; card_id: string } }
  | { command: 'stop_controlled_development'; payload: { session_id: string; packet_id: string } }
  | { command: 'discard_controlled_development_evidence'; payload: { session_id: string } }
  | { command: 'stop_operation'; payload: { kind: OperationKind } }
  | { command: 'new_session' }
  | { command: 'load_session'; payload: { session_id: string } }
  | { command: 'delete_session'; payload: { session_id: string } }
  | { command: 'select_backend'; payload: { backend: string; model: string } }
  | { command: 'update_settings'; payload: { settings: VisibleSettings } }
  | { command: 'start_autopilot'; payload: { task: string; iterations: number } }
  | { command: 'start_cascade'; payload: { prompt: string; backend: string; n: number; vote_k: number; check_cmd: string | null; diversity_hints: string[]; escalate_backend: string | null } }
  | { command: 'start_evolve'; payload: { prompt: string; backend: string; generations: number; population: number; fitness_cmd: string; feature_cmd: string | null; islands: number; migration_interval: number; mutation_hints: string[] } }
  | { command: 'run_procedure'; payload: { change_id: string; task_id: string } }
  | { command: 'preview_procedure'; payload: { localization_run_id: string; change_id: string; task_id: string; route: 'automatic' | 'force_local' | 'force_frontier'; local_backend: string; local_model: string; frontier_backend: string; frontier_model: string } }
  | { command: 'run_whole_change_procedure'; payload: { change_id: string; route: 'automatic' | 'force_local' | 'force_frontier'; localization_backend: string; local_backend: string; local_model: string; frontier_backend: string; frontier_model: string } }
  | { command: 'apply_procedure'; payload: { localization_run_id: string; preview_id: string; change_id: string; task_id: string } }
  | { command: 'review_procedure'; payload: { run_id: string; decision: 'approve' | 'reject' } }
  | { command: 'start_voice_capture' }
  | { command: 'stop_voice_capture' }
  | { command: 'pick_working_directory' }
  | { command: 'refresh_tests' }
  | { command: 'start_test_run'; payload: { request: { identity: TestIdentity; catalogue_revision: number } } }
  | { command: 'cancel_test_run' };

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
const controlledDevelopmentPhases: readonly ControlledDevelopmentPhase[] = [
  'off',
  'planning',
  'awaiting_approval',
  'executing',
  'completed',
  'blocked',
  'interrupted',
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
  const procedure = record(item.procedure);
  if (!Array.isArray(item.backends)) throw new Error('backends must be an array');
  if (!Array.isArray(procedure.verifier_commands)) throw new Error('verifier commands must be an array');
  return {
    backends: item.backends.map((entry) => {
      const backend = record(entry);
      if (!Array.isArray(backend.models)) throw new Error('backend models must be an array');
      return { name: string(backend.name, 'backend name'), configured_model: string(backend.configured_model, 'configured model'), models: backend.models.map((model) => string(model, 'model')) };
    }),
    selected_backend: nullable(item.selected_backend, (entry) => string(entry, 'selected backend')),
    selected_model: nullable(item.selected_model, (entry) => string(entry, 'selected model')),
    effort: string(item.effort, 'effort'),
    context_budget: integer(item.context_budget, 'context budget'),
    show_raw_output: boolean(item.show_raw_output, 'show raw output'),
    max_tokens: integer(item.max_tokens, 'max tokens'),
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
    procedure: {
      localization_backend: nullable(procedure.localization_backend, (entry) => string(entry, 'localization backend')),
      local_patch_backend: nullable(procedure.local_patch_backend, (entry) => string(entry, 'local patch backend')),
      frontier_patch_backend: nullable(procedure.frontier_patch_backend, (entry) => string(entry, 'frontier patch backend')),
      index_max_files: integer(procedure.index_max_files, 'index max files'),
      index_max_total_bytes: integer(procedure.index_max_total_bytes, 'index max total bytes'),
      verifier_commands: procedure.verifier_commands.map((entry) => string(entry, 'verifier command')),
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

function parseStringArray(value: unknown, name: string): string[] {
  if (!Array.isArray(value)) throw new Error(`${name} must be an array`);
  return value.map((entry) => string(entry, name));
}

function parseWorkCard(value: unknown): WorkCard {
  const card = record(value);
  return {
    id: string(card.id, 'Work Card id'),
    outcome: string(card.outcome, 'Work Card outcome'),
    proof_commands: parseStringArray(card.proof_commands, 'Work Card proof command'),
    production_paths: parseStringArray(card.production_paths, 'Work Card production path'),
    supporting_paths: parseStringArray(card.supporting_paths, 'Work Card supporting path'),
    excluded: parseStringArray(card.excluded, 'Work Card exclusion'),
    complexity_exceptions: parseStringArray(card.complexity_exceptions, 'Work Card complexity exception'),
  };
}

const controlledDevelopmentRawDetailKinds = ['backend_event', 'verifier_stdout', 'verifier_stderr', 'verifier_combined_output', 'verifier_error', 'failure'] as const;

function parseControlledDevelopment(value: unknown): ControlledDevelopmentState {
  const state = record(value);
  if (!Array.isArray(state.structural_errors) || !Array.isArray(state.proof_results) || !Array.isArray(state.raw_details)) {
    throw new Error('controlled development evidence must be arrays');
  }
  return {
    enabled: boolean(state.enabled, 'controlled development enabled'),
    phase: enumValue(state.phase, controlledDevelopmentPhases, 'controlled development phase'),
    packet_id: nullable(state.packet_id, (entry) => string(entry, 'controlled development packet id')),
    card: nullable(state.card, parseWorkCard),
    structural_errors: state.structural_errors.map((entry) => {
      const error = record(entry);
      return { field: string(error.field, 'structural error field'), message: string(error.message, 'structural error message') };
    }),
    changed_paths: parseStringArray(state.changed_paths, 'controlled development changed path'),
    proof_results: state.proof_results.map((entry) => {
      const result = record(entry);
      return {
        command: string(result.command, 'proof command'),
        disposition: string(result.disposition, 'proof disposition'),
        success: nullable(result.success, (success) => boolean(success, 'proof success')),
        exit_code: nullable(result.exit_code, (exitCode) => number(exitCode, 'proof exit code')),
      };
    }),
    progress_notice: string(state.progress_notice, 'controlled development progress notice'),
    completion_summary: nullable(state.completion_summary, (entry) => string(entry, 'controlled development completion summary')),
    compact_result: nullable(state.compact_result, (entry) => string(entry, 'controlled development result')),
    blocker: nullable(state.blocker, (entry) => string(entry, 'controlled development blocker')),
    raw_details: state.raw_details.map((entry) => {
      const detail = record(entry);
      return {
        kind: enumValue(detail.kind, controlledDevelopmentRawDetailKinds, 'controlled development raw detail kind'),
        name: string(detail.name, 'controlled development raw detail name'),
        content: string(detail.content, 'controlled development raw detail content'),
        truncated_at_source: boolean(detail.truncated_at_source, 'controlled development raw detail truncation state'),
        bytes_seen: number(detail.bytes_seen, 'controlled development raw detail byte count'),
      };
    }),
    retained_evidence: boolean(state.retained_evidence, 'controlled development retained evidence'),
    limitation: string(state.limitation, 'controlled development limitation'),
  };
}

export function emptyControlledDevelopmentState(): ControlledDevelopmentState {
  return {
    enabled: false,
    phase: 'off',
    packet_id: null,
    card: null,
    structural_errors: [],
    changed_paths: [],
    proof_results: [],
    progress_notice: 'Phase: Off. No Work Card is recorded. 0 changed path(s) and 0 proof result(s) are recorded. No failure is recorded.',
    completion_summary: null,
    compact_result: null,
    blocker: null,
    raw_details: [],
    retained_evidence: false,
    limitation: 'Approved proof commands run in the disposable workspace, but can still address absolute paths outside it.',
  };
}

function parseRetainedTestResult(value: unknown): RetainedTestResult {
  const item = record(value);
  const identity = record(item.identity);
  const counts = record(item.counts);
  if (!Array.isArray(item.command) || !Array.isArray(item.failed_tests)) throw new Error('retained test result collections must be arrays');
  return {
    run_id: string(item.run_id, 'test run id'),
    identity: { name: string(identity.name, 'test identity'), scope: parseTestScope(identity.scope) },
    command: item.command.map((entry) => string(entry, 'test command argument')),
    working_dir: string(item.working_dir, 'test working directory'),
    started_at_ms: integer(item.started_at_ms, 'test start time'),
    duration_ms: integer(item.duration_ms, 'test duration'),
    outcome: enumValue(item.outcome, ['passed', 'failed', 'cancelled', 'infrastructure_error'] as const, 'test outcome'),
    counts: {
      passed: integer(counts.passed, 'passed count'),
      failed: integer(counts.failed, 'failed count'),
      ignored: integer(counts.ignored, 'ignored count'),
      filtered: integer(counts.filtered, 'filtered count'),
    },
    exit_code: nullable(item.exit_code, (entry) => number(entry, 'test exit code')),
    failed_tests: item.failed_tests.map((entry) => string(entry, 'failed test name')),
    output: string(item.output, 'test output'),
    omitted_output_bytes: integer(item.omitted_output_bytes, 'omitted output bytes'),
  };
}

function parseTestScope(value: unknown): TestScope {
  const scope = record(value);
  const type = enumValue(scope.type, ['full_workspace', 'module', 'exact'] as const, 'test scope');
  if (type === 'full_workspace') return { type };
  const module = string(scope.module, 'test module');
  return type === 'module' ? { type, module } : { type, module, test: string(scope.test, 'exact test') };
}

function parseTestIdentity(value: unknown): TestIdentity {
  const identity = record(value);
  return { name: string(identity.name, 'test identity'), scope: parseTestScope(identity.scope) };
}

function parseTestControl(value: unknown): TestControlSnapshot {
  const tests = record(value);
  const discovery = record(tests.discovery);
  const catalogue = nullable(discovery.catalogue, (entry) => {
    const item = record(entry);
    if (!Array.isArray(item.modules)) throw new Error('test catalogue modules must be an array');
    return {
      discovered_at_ms: integer(item.discovered_at_ms, 'catalogue revision'),
      full_workspace: parseTestIdentity(item.full_workspace),
      modules: item.modules.map((moduleValue) => {
        const module = record(moduleValue);
        if (!Array.isArray(module.tests)) throw new Error('module tests must be an array');
        return { name: string(module.name, 'module name'), tests: module.tests.map(parseTestIdentity) };
      }),
    };
  });
  const failure = nullable(discovery.failure, (entry) => {
    const item = record(entry);
    if (!Array.isArray(item.command)) throw new Error('discovery command must be an array');
    return {
      command: item.command.map((part) => string(part, 'discovery argument')),
      working_dir: string(item.working_dir, 'discovery working directory'),
      exit_code: nullable(item.exit_code, (code) => number(code, 'discovery exit code')),
      diagnostic_output: string(item.diagnostic_output, 'discovery output'),
      omitted_output_bytes: integer(item.omitted_output_bytes, 'discovery omitted bytes'),
    };
  });
  if (!Array.isArray(tests.retained_results) || !Array.isArray(tests.retained_result_warnings)) {
    throw new Error('test result history collections must be arrays');
  }
  return {
    discovery: {
      catalogue,
      catalogue_stale: boolean(discovery.catalogue_stale, 'catalogue stale'),
      failure,
    },
    active: nullable(tests.active, (entry) => {
      const active = parseRetainedTestResult({
        ...record(entry),
        duration_ms: record(entry).elapsed_ms,
        outcome: 'passed',
        exit_code: null,
        failed_tests: [],
      });
      const raw = record(entry);
      return {
        run_id: active.run_id,
        identity: active.identity,
        command: active.command,
        working_dir: active.working_dir,
        started_at_ms: active.started_at_ms,
        elapsed_ms: integer(raw.elapsed_ms, 'test elapsed time'),
        running: boolean(raw.running, 'test running'),
        counts: active.counts,
        output: active.output,
        omitted_output_bytes: active.omitted_output_bytes,
      };
    }),
    latest_result: nullable(tests.latest_result, parseRetainedTestResult),
    retained_results: tests.retained_results.map(parseRetainedTestResult),
    retained_result_warnings: tests.retained_result_warnings.map((entry) => string(entry, 'test result warning')),
  };
}

export function parseSnapshot(value: unknown): AppSnapshot {
  const item = record(value);
  if (!Array.isArray(item.transcript) || !Array.isArray(item.saved_sessions) || !Array.isArray(item.operations)) throw new Error('snapshot collections must be arrays');
  return {
    revision: integer(item.revision, 'snapshot revision'),
    workspace: enumValue(item.workspace, workspaces, 'workspace'),
    transcript: item.transcript.map(parseBlock),
    session: parseSession(item.session),
    saved_sessions: item.saved_sessions.map(parseSession),
    pending_session_switch: nullable(item.pending_session_switch, parsePending),
    settings: parseSettings(item.settings),
    operations: item.operations.map(parseOperation),
    controlled_development: item.controlled_development === undefined
      ? emptyControlledDevelopmentState()
      : parseControlledDevelopment(item.controlled_development),
    tests: item.tests === undefined ? emptyTestControl() : parseTestControl(item.tests),
  };
}

function emptyTestControl(): TestControlSnapshot {
  return {
    discovery: { catalogue: null, catalogue_stale: false, failure: null },
    active: null,
    latest_result: null,
    retained_results: [],
    retained_result_warnings: [],
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
    case 'saved_sessions_changed': {
      if (!Array.isArray(item.value)) throw new Error('saved sessions must be an array');
      return { revision, type, value: item.value.map(parseSession) };
    }
    case 'pending_session_switch_changed': return { revision, type, value: nullable(item.value, parsePending) };
    case 'settings_changed': return { revision, type, value: parseSettings(item.value) };
    case 'operation_changed': return { revision, type, value: parseOperation(item.value) };
    case 'controlled_development_changed': return { revision, type, value: parseControlledDevelopment(item.value) };
    case 'tests_changed': return { revision, type, value: parseTestControl(item.value) };
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
