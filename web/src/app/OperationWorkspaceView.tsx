import type { FormEvent } from 'react';

import type { AppCommandResult, OperationState } from '../client/contracts.ts';

export type InteractiveOperationKind = 'autopilot' | 'cascade' | 'evolve';
type Setter = (value: string) => void;

export interface OperationWorkspaceViewProps {
  kind: InteractiveOperationKind;
  title: string;
  primary: string;
  secondary: string;
  search: SearchFieldsProps;
  validation: string | null;
  submitting: boolean;
  active: boolean;
  blockedBy: string | null;
  operation: OperationState | null;
  ownsActiveOperation: boolean;
  onPrimaryChange: Setter;
  onSecondaryChange: Setter;
  onSubmit(event: FormEvent): void;
  onStop(): Promise<AppCommandResult>;
}

export function OperationWorkspaceView(props: OperationWorkspaceViewProps) {
  const { kind, title } = props;
  return <section aria-labelledby={`${kind}-title`} className="operation-workspace">
    <h3 id={`${kind}-title`}>{title} operation</h3>
    <form aria-label={`Start ${title}`} onSubmit={(event) => props.onSubmit(event)}>
      <label htmlFor={`${kind}-primary`}>{kind === 'autopilot' ? 'Task' : 'Prompt'}</label>
      <textarea id={`${kind}-primary`} onChange={(event) => props.onPrimaryChange(event.target.value)} value={props.primary} />
      {kind === 'autopilot' && <>
        <label htmlFor={`${kind}-secondary`}>Iterations</label>
        <input id={`${kind}-secondary`} min={1} onChange={(event) => props.onSecondaryChange(event.target.value)} type="number" value={props.secondary} />
      </>}
      {(kind === 'cascade' || kind === 'evolve') && <SearchFields {...props.search} kind={kind} />}
      {props.validation !== null && <p className="validation-message" role="alert">{props.validation}</p>}
      <button aria-describedby={props.blockedBy === null ? undefined : `${kind}-blocked`} disabled={props.submitting || props.active} type="submit">Start {title}</button>
      {props.blockedBy !== null && <p className="disabled-reason" id={`${kind}-blocked`}>{props.blockedBy} is active. Stop or finish it before starting {title}.</p>}
    </form>
    <OperationProgress operation={props.operation} onStop={() => props.onStop()} showStop={props.ownsActiveOperation} />
  </section>;
}

export function OperationProgress({ operation, onStop, showStop }: { operation: OperationState | null; onStop: () => Promise<AppCommandResult>; showStop: boolean }) {
  if (operation === null || operation.phase === 'idle') return <p className="operation-empty">No operation has run in this workspace.</p>;
  const terminal = ['completed', 'failed', 'interrupted', 'cancelled'].includes(operation.phase);
  return <section aria-labelledby="operation-progress-title" className="operation-progress">
    <h4 id="operation-progress-title">Progress</h4>
    {operation.operation_id !== null && <p><strong>Run:</strong> <code>{operation.operation_id}</code></p>}
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

interface SearchValues { backend: string; countA: string; countB: string; countC: string; countD: string; command: string; optionalCommand: string; hints: string }
export interface SearchFieldsProps extends SearchValues { backends: string[]; setBackend: Setter; setCountA: Setter; setCountB: Setter; setCountC: Setter; setCountD: Setter; setCommand: Setter; setOptionalCommand: Setter; setHints: Setter }

function SearchFields(props: SearchFieldsProps & { kind: 'cascade' | 'evolve' }) {
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
