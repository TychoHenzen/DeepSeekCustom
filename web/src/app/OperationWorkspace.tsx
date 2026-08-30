import { useState, type FormEvent } from 'react';

import type { AppCommand, AppCommandResult, OperationKind, OperationState } from '../client/contracts.ts';

type WorkspaceKind = Extract<OperationKind, 'autopilot' | 'cascade' | 'evolve' | 'procedure'>;

export interface OperationWorkspaceProps {
  kind: WorkspaceKind;
  operation: OperationState | null;
  activeOperation: OperationState | null;
  send(this: void, command: AppCommand): Promise<AppCommandResult>;
  backends?: string[];
  selectedBackend?: string | null;
  selectedModel?: string | null;
}

const labels: Record<WorkspaceKind, string> = {
  autopilot: 'Autopilot',
  cascade: 'Cascade',
  evolve: 'Evolve',
  procedure: 'Procedure',
};

export function OperationWorkspace({ kind, operation, activeOperation, send, backends = [], selectedBackend = null, selectedModel = null }: OperationWorkspaceProps) {
  const [primary, setPrimary] = useState('');
  const [secondary, setSecondary] = useState(kind === 'autopilot' ? '3' : '');
  const [backend, setBackend] = useState(selectedBackend ?? backends[0] ?? '');
  const [countA, setCountA] = useState(kind === 'cascade' ? '5' : '10');
  const [countB, setCountB] = useState(kind === 'cascade' ? '1' : '6');
  const [countC, setCountC] = useState('1');
  const [countD, setCountD] = useState('5');
  const [command, setCommand] = useState('');
  const [optionalCommand, setOptionalCommand] = useState('');
  const [hints, setHints] = useState('');
  const [validation, setValidation] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const active = activeOperation?.phase === 'running' || activeOperation?.phase === 'awaiting_review';
  const ownsActiveOperation = active && activeOperation?.kind === kind;
  const blockedBy = active && !ownsActiveOperation ? labels[activeOperation.kind as WorkspaceKind] ?? activeOperation.kind : null;
  const title = labels[kind];

  if (kind === 'procedure') {
    return <ProcedureWorkspace activeOperation={activeOperation} backends={backends} operation={operation} selectedBackend={selectedBackend} selectedModel={selectedModel} send={send} />;
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    const appCommand = commandFor(kind, primary.trim(), secondary.trim(), {
      backend, countA, countB, countC, countD, command, optionalCommand, hints,
    });
    if ('error' in appCommand) {
      setValidation(appCommand.error);
      return;
    }
    setValidation(null);
    setSubmitting(true);
    try {
      const result = await send(appCommand);
      if (result.status === 'rejected') setValidation(result.error.message);
    } finally {
      setSubmitting(false);
    }
  }

  return <section aria-labelledby={`${kind}-title`} className="operation-workspace">
    <h3 id={`${kind}-title`}>{title} operation</h3>
    <form aria-label={`Start ${title}`} onSubmit={(event) => void submit(event)}>
      <label htmlFor={`${kind}-primary`}>{primaryLabel(kind)}</label>
      <textarea id={`${kind}-primary`} onChange={(event) => setPrimary(event.target.value)} value={primary} />
      {kind === 'autopilot' && <>
        <label htmlFor={`${kind}-secondary`}>Iterations</label>
        <input id={`${kind}-secondary`} min={1} onChange={(event) => setSecondary(event.target.value)} type="number" value={secondary} />
      </>}
      {(kind === 'cascade' || kind === 'evolve') && <SearchFields
        backend={backend} backends={backends} command={command} countA={countA}
        countB={countB} countC={countC} countD={countD} hints={hints} kind={kind}
        optionalCommand={optionalCommand} setBackend={setBackend} setCommand={setCommand}
        setCountA={setCountA} setCountB={setCountB} setCountC={setCountC}
        setCountD={setCountD} setHints={setHints} setOptionalCommand={setOptionalCommand}
      />}
      {validation !== null && <p className="validation-message" role="alert">{validation}</p>}
      <button aria-describedby={blockedBy === null ? undefined : `${kind}-blocked`} disabled={submitting || active} type="submit">Start {title}</button>
      {blockedBy !== null && <p className="disabled-reason" id={`${kind}-blocked`}>{blockedBy} is active. Stop or finish it before starting {title}.</p>}
    </form>
    <OperationProgress operation={operation} onStop={() => send({ command: 'stop_operation', payload: { kind } })} showStop={ownsActiveOperation} />
  </section>;
}

function ProcedureWorkspace({ operation, activeOperation, send, backends = [], selectedBackend = null, selectedModel = null }: Omit<OperationWorkspaceProps, 'kind'>) {
  const [changeId, setChangeId] = useState('');
  const [taskId, setTaskId] = useState('');
  const [mode, setMode] = useState<'localize' | 'preview' | 'whole_change' | 'apply'>('localize');
  const [localizationRunId, setLocalizationRunId] = useState('');
  const [previewId, setPreviewId] = useState('');
  const [route, setRoute] = useState<'automatic' | 'force_local' | 'force_frontier'>('automatic');
  const [localBackend, setLocalBackend] = useState(selectedBackend ?? backends[0] ?? '');
  const [localModel, setLocalModel] = useState(selectedModel ?? '');
  const [frontierBackend, setFrontierBackend] = useState(selectedBackend ?? backends[0] ?? '');
  const [frontierModel, setFrontierModel] = useState(selectedModel ?? '');
  const [validation, setValidation] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [reviewingRunId, setReviewingRunId] = useState<string | null>(null);
  const active = activeOperation?.phase === 'running' || activeOperation?.phase === 'awaiting_review';
  const ownsActiveOperation = active && activeOperation?.kind === 'procedure';
  const blockedBy = active && !ownsActiveOperation
    ? labels[activeOperation.kind as WorkspaceKind] ?? activeOperation.kind
    : null;
  const reviewRunId = operation?.phase === 'awaiting_review' ? operation.operation_id : null;
  const reviewInFlight = reviewRunId !== null && reviewingRunId === reviewRunId;

  async function submit(event: FormEvent) {
    event.preventDefault();
    const required = [changeId.trim()];
    if (mode !== 'whole_change') required.push(taskId.trim());
    if (mode === 'preview' || mode === 'apply') required.push(localizationRunId.trim());
    if (mode === 'preview' || mode === 'whole_change') required.push(localBackend.trim(), localModel.trim(), frontierBackend.trim(), frontierModel.trim());
    if (mode === 'apply') required.push(previewId.trim());
    if (required.some((value) => value.length === 0)) {
      setValidation('All fields for the selected Procedure mode are required.');
      return;
    }
    const command: AppCommand = mode === 'localize'
      ? { command: 'run_procedure', payload: { change_id: changeId.trim(), task_id: taskId.trim() } }
      : mode === 'preview'
        ? { command: 'preview_procedure', payload: { localization_run_id: localizationRunId.trim(), change_id: changeId.trim(), task_id: taskId.trim(), route, local_backend: localBackend.trim(), local_model: localModel.trim(), frontier_backend: frontierBackend.trim(), frontier_model: frontierModel.trim() } }
        : mode === 'whole_change'
          ? { command: 'run_whole_change_procedure', payload: { change_id: changeId.trim(), route, localization_backend: localBackend.trim(), local_backend: localBackend.trim(), local_model: localModel.trim(), frontier_backend: frontierBackend.trim(), frontier_model: frontierModel.trim() } }
          : { command: 'apply_procedure', payload: { localization_run_id: localizationRunId.trim(), preview_id: previewId.trim(), change_id: changeId.trim(), task_id: taskId.trim() } };
    setValidation(null);
    setSubmitting(true);
    try {
      const result = await send(command);
      if (result.status === 'rejected') setValidation(result.error.message);
    } finally {
      setSubmitting(false);
    }
  }

  async function review(decision: 'approve' | 'reject') {
    if (reviewRunId === null) return;
    const selectedRunId = reviewRunId;
    setReviewingRunId(selectedRunId);
    try {
      const result = await send({ command: 'review_procedure', payload: { run_id: selectedRunId, decision } });
      if (result.status === 'rejected') setValidation(result.error.message);
    } finally {
      setReviewingRunId((current) => current === selectedRunId ? null : current);
    }
  }

  return <section aria-labelledby="procedure-title" className="operation-workspace procedure-workspace">
    <h3 id="procedure-title">Procedure operation</h3>
    <form aria-label="Start Procedure" onSubmit={(event) => void submit(event)}>
      <fieldset className="operation-parameters">
        <legend>Run selection</legend>
        <label htmlFor="procedure-mode">Run mode</label>
        <select id="procedure-mode" onChange={(event) => setMode(event.target.value as typeof mode)} value={mode}><option value="localize">Localize task</option><option value="preview">Preview patch</option><option value="whole_change">Whole change</option><option value="apply">Apply preview</option></select>
        <label htmlFor="procedure-change">Change ID</label>
        <input id="procedure-change" onChange={(event) => setChangeId(event.target.value)} value={changeId} />
        <label htmlFor="procedure-task">Task ID</label>
        <input id="procedure-task" onChange={(event) => setTaskId(event.target.value)} value={taskId} />
        {(mode === 'preview' || mode === 'apply') && <><label htmlFor="procedure-localization-run">Localization run ID</label><input id="procedure-localization-run" onChange={(event) => setLocalizationRunId(event.target.value)} value={localizationRunId} /></>}
        {mode === 'apply' && <><label htmlFor="procedure-preview-id">Preview ID</label><input id="procedure-preview-id" onChange={(event) => setPreviewId(event.target.value)} value={previewId} /></>}
        {(mode === 'preview' || mode === 'whole_change') && <>
          <label htmlFor="procedure-route">Route</label><select id="procedure-route" onChange={(event) => setRoute(event.target.value as typeof route)} value={route}><option value="automatic">Automatic</option><option value="force_local">Force local</option><option value="force_frontier">Force frontier</option></select>
          <label htmlFor="procedure-local-backend">Local backend</label><input id="procedure-local-backend" onChange={(event) => setLocalBackend(event.target.value)} value={localBackend} />
          <label htmlFor="procedure-local-model">Local model</label><input id="procedure-local-model" onChange={(event) => setLocalModel(event.target.value)} value={localModel} />
          <label htmlFor="procedure-frontier-backend">Frontier backend</label><input id="procedure-frontier-backend" onChange={(event) => setFrontierBackend(event.target.value)} value={frontierBackend} />
          <label htmlFor="procedure-frontier-model">Frontier model</label><input id="procedure-frontier-model" onChange={(event) => setFrontierModel(event.target.value)} value={frontierModel} />
        </>}
        <p className="field-help">This run localizes the selected OpenSpec task. Route, patch, diff, report, and failure evidence appears below in execution order.</p>
      </fieldset>
      {validation !== null && <p className="validation-message" role="alert">{validation}</p>}
      <button aria-describedby={blockedBy === null ? undefined : 'procedure-blocked'} disabled={submitting || active} type="submit">Start Procedure</button>
      {blockedBy !== null && <p className="disabled-reason" id="procedure-blocked">{blockedBy} is active. Stop or finish it before starting Procedure.</p>}
    </form>
    <OperationProgress operation={operation} onStop={() => send({ command: 'stop_operation', payload: { kind: 'procedure' } })} showStop={ownsActiveOperation && operation?.phase === 'running'} />
    {reviewRunId !== null && <section aria-label="Procedure review" className="procedure-review">
      <h4>Review Procedure run {reviewRunId}</h4>
      <p>Complete review evidence</p>
      <pre aria-label="Complete review evidence">{operation?.message ?? 'No review evidence was supplied.'}</pre>
      <div className="review-actions">
        <button disabled={reviewInFlight} onClick={() => void review('approve')} type="button">Approve this run</button>
        <button disabled={reviewInFlight} onClick={() => void review('reject')} type="button">Reject this run</button>
      </div>
    </section>}
  </section>;
}

function OperationProgress({ operation, onStop, showStop }: { operation: OperationState | null; onStop: () => Promise<AppCommandResult>; showStop: boolean }) {
  if (operation === null || operation.phase === 'idle') return <p className="operation-empty">No operation has run in this workspace.</p>;
  const terminal = ['completed', 'failed', 'interrupted', 'cancelled'].includes(operation.phase);
  return <section aria-labelledby="operation-progress-title" className="operation-progress">
    <h4 id="operation-progress-title">Progress</h4>
    <ol className="progress-timeline">
      <li><strong>{operation.phase.replace('_', ' ')}</strong>{operation.progress && ` ${operation.progress.completed} of ${operation.progress.total ?? 'unknown'}`}</li>
    </ol>
    <div aria-label="Operation log" className="operation-log" role="log"><pre>{operation.message ?? 'No log output.'}</pre></div>
    {operation.error !== null && <p className="validation-message" role="alert">{operation.error.message}</p>}
    {showStop && <button onClick={() => void onStop()} type="button">Stop operation</button>}
    {terminal && <section aria-label="Result summary" className={`result-summary result-${operation.phase}`}>
      <h4>Result</h4><p>{operation.message ?? `Operation ${operation.phase}.`}</p>
    </section>}
  </section>;
}

function primaryLabel(kind: WorkspaceKind): string {
  if (kind === 'procedure') return 'Change ID';
  if (kind === 'autopilot') return 'Task';
  return 'Prompt';
}

interface SearchValues { backend: string; countA: string; countB: string; countC: string; countD: string; command: string; optionalCommand: string; hints: string }

function commandFor(kind: WorkspaceKind, primary: string, secondary: string, values: SearchValues): AppCommand | { error: string } {
  if (primary.length === 0) return { error: `${primaryLabel(kind)} is required.` };
  switch (kind) {
    case 'autopilot': {
      const iterations = Number(secondary);
      if (!Number.isSafeInteger(iterations) || iterations < 1) return { error: 'Iterations must be a positive whole number.' };
      return { command: 'start_autopilot', payload: { task: primary, iterations } };
    }
    case 'cascade': {
      if (values.backend.trim().length === 0) return { error: 'Backend is required.' };
      const n = boundedInteger(values.countA, 1, 16, 'Attempts');
      if (typeof n === 'string') return { error: n };
      const voteK = boundedInteger(values.countB, 1, 8, 'Vote margin');
      if (typeof voteK === 'string') return { error: voteK };
      return { command: 'start_cascade', payload: { prompt: primary, backend: values.backend, n, vote_k: voteK, check_cmd: optional(values.command), diversity_hints: lines(values.hints), escalate_backend: optional(values.optionalCommand) } };
    }
    case 'evolve': {
      if (values.backend.trim().length === 0) return { error: 'Backend is required.' };
      if (values.command.trim().length === 0) return { error: 'Fitness command is required.' };
      const generations = boundedInteger(values.countA, 1, 50, 'Generations'); if (typeof generations === 'string') return { error: generations };
      const population = boundedInteger(values.countB, 1, 20, 'Population'); if (typeof population === 'string') return { error: population };
      const islands = boundedInteger(values.countC, 1, 8, 'Islands'); if (typeof islands === 'string') return { error: islands };
      const migration = boundedInteger(values.countD, 0, 20, 'Migration interval'); if (typeof migration === 'string') return { error: migration };
      return { command: 'start_evolve', payload: { prompt: primary, backend: values.backend, generations, population, fitness_cmd: values.command.trim(), feature_cmd: optional(values.optionalCommand), islands, migration_interval: migration, mutation_hints: lines(values.hints) } };
    }
    case 'procedure':
      if (secondary.length === 0) return { error: 'Task ID is required.' };
      return { command: 'run_procedure', payload: { change_id: primary, task_id: secondary } };
  }
}

function optional(value: string): string | null { return value.trim() || null; }
function lines(value: string): string[] { return value.split(/\r?\n/).map((line) => line.trim()).filter(Boolean); }
function boundedInteger(value: string, min: number, max: number, label: string): number | string {
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) && parsed >= min && parsed <= max ? parsed : `${label} must be a whole number from ${min} to ${max}.`;
}

type Setter = (value: string) => void;
interface SearchFieldsProps extends SearchValues { kind: 'cascade' | 'evolve'; backends: string[]; setBackend: Setter; setCountA: Setter; setCountB: Setter; setCountC: Setter; setCountD: Setter; setCommand: Setter; setOptionalCommand: Setter; setHints: Setter }
function SearchFields(props: SearchFieldsProps) {
  const id = props.kind;
  return <fieldset className="operation-parameters"><legend>{props.kind === 'cascade' ? 'Search parameters' : 'Evolution parameters'}</legend>
    <label htmlFor={`${id}-backend`}>Backend</label>
    <select id={`${id}-backend`} onChange={(event) => props.setBackend(event.target.value)} value={props.backend}><option value="">Select backend</option>{props.backends.map((name) => <option key={name} value={name}>{name}</option>)}</select>
    <label htmlFor={`${id}-count-a`}>{props.kind === 'cascade' ? 'Attempts' : 'Generations'}</label><input id={`${id}-count-a`} min="1" onChange={(event) => props.setCountA(event.target.value)} type="number" value={props.countA} />
    <label htmlFor={`${id}-count-b`}>{props.kind === 'cascade' ? 'Vote margin' : 'Population'}</label><input id={`${id}-count-b`} min="1" onChange={(event) => props.setCountB(event.target.value)} type="number" value={props.countB} />
    {props.kind === 'evolve' && <><label htmlFor={`${id}-count-c`}>Islands</label><input id={`${id}-count-c`} min="1" onChange={(event) => props.setCountC(event.target.value)} type="number" value={props.countC} /><label htmlFor={`${id}-count-d`}>Migration interval</label><input id={`${id}-count-d`} min="0" onChange={(event) => props.setCountD(event.target.value)} type="number" value={props.countD} /></>}
    <label htmlFor={`${id}-command`}>{props.kind === 'cascade' ? 'Check command (optional)' : 'Fitness command'}</label><input id={`${id}-command`} onChange={(event) => props.setCommand(event.target.value)} value={props.command} />
    <label htmlFor={`${id}-optional-command`}>{props.kind === 'cascade' ? 'Escalation backend (optional)' : 'Feature command (optional)'}</label><input id={`${id}-optional-command`} onChange={(event) => props.setOptionalCommand(event.target.value)} value={props.optionalCommand} />
    <label htmlFor={`${id}-hints`}>{props.kind === 'cascade' ? 'Diversity hints' : 'Mutation hints'}</label><textarea id={`${id}-hints`} onChange={(event) => props.setHints(event.target.value)} value={props.hints} />
  </fieldset>;
}
