import { useState, type FormEvent } from 'react';

import type { AppCommand, AppCommandResult, OperationKind, OperationState } from '../client/contracts.ts';

type WorkspaceKind = Extract<OperationKind, 'autopilot' | 'cascade' | 'evolve' | 'procedure'>;

export interface OperationWorkspaceProps {
  kind: WorkspaceKind;
  operation: OperationState | null;
  activeOperation: OperationState | null;
  send(this: void, command: AppCommand): Promise<AppCommandResult>;
}

const labels: Record<WorkspaceKind, string> = {
  autopilot: 'Autopilot',
  cascade: 'Cascade',
  evolve: 'Evolve',
  procedure: 'Procedure',
};

export function OperationWorkspace({ kind, operation, activeOperation, send }: OperationWorkspaceProps) {
  const [primary, setPrimary] = useState('');
  const [secondary, setSecondary] = useState(kind === 'autopilot' ? '3' : '');
  const [validation, setValidation] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const active = activeOperation?.phase === 'running' || activeOperation?.phase === 'awaiting_review';
  const ownsActiveOperation = active && activeOperation?.kind === kind;
  const blockedBy = active && !ownsActiveOperation ? labels[activeOperation.kind as WorkspaceKind] ?? activeOperation.kind : null;
  const title = labels[kind];

  async function submit(event: FormEvent) {
    event.preventDefault();
    const command = commandFor(kind, primary.trim(), secondary.trim());
    if ('error' in command) {
      setValidation(command.error);
      return;
    }
    setValidation(null);
    setSubmitting(true);
    try {
      const result = await send(command);
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
      {(kind === 'autopilot' || kind === 'procedure') && <>
        <label htmlFor={`${kind}-secondary`}>{kind === 'autopilot' ? 'Iterations' : 'Task ID'}</label>
        <input id={`${kind}-secondary`} min={kind === 'autopilot' ? 1 : undefined} onChange={(event) => setSecondary(event.target.value)} type={kind === 'autopilot' ? 'number' : 'text'} value={secondary} />
      </>}
      {validation !== null && <p className="validation-message" role="alert">{validation}</p>}
      <button aria-describedby={blockedBy === null ? undefined : `${kind}-blocked`} disabled={submitting || active} type="submit">Start {title}</button>
      {blockedBy !== null && <p className="disabled-reason" id={`${kind}-blocked`}>{blockedBy} is active. Stop or finish it before starting {title}.</p>}
    </form>
    <OperationProgress operation={operation} onStop={() => send({ command: 'stop_operation', payload: { kind } })} showStop={ownsActiveOperation} />
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

function commandFor(kind: WorkspaceKind, primary: string, secondary: string): AppCommand | { error: string } {
  if (primary.length === 0) return { error: `${primaryLabel(kind)} is required.` };
  switch (kind) {
    case 'autopilot': {
      const iterations = Number(secondary);
      if (!Number.isSafeInteger(iterations) || iterations < 1) return { error: 'Iterations must be a positive whole number.' };
      return { command: 'start_autopilot', payload: { task: primary, iterations } };
    }
    case 'cascade': return { command: 'start_cascade', payload: { prompt: primary } };
    case 'evolve': return { command: 'start_evolve', payload: { prompt: primary } };
    case 'procedure':
      if (secondary.length === 0) return { error: 'Task ID is required.' };
      return { command: 'run_procedure', payload: { change_id: primary, task_id: secondary } };
  }
}
