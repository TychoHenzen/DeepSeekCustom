import type { AppCommandResult, PendingSessionSwitch, SessionSummary } from '../client/contracts.ts';

interface SessionsWorkspaceProps {
  current: SessionSummary;
  saved: SessionSummary[];
  pending: PendingSessionSwitch | null;
  send(this: void, command: { command: 'new_session' } | { command: 'load_session' | 'delete_session'; payload: { session_id: string } }): Promise<AppCommandResult>;
}

export function SessionsWorkspace({ current, saved, pending, send }: SessionsWorkspaceProps) {
  return <section aria-labelledby="sessions-title">
    <h3 id="sessions-title">Saved sessions</h3>
    <p>Current: {current.title}</p>
    {pending !== null && <p aria-live="polite" role="status">Session change pending until the active turn ends.</p>}
    <button onClick={() => void send({ command: 'new_session' })} type="button">New session</button>
    {saved.length === 0 ? <p>No saved sessions.</p> : <ul>{saved.map((session) => <li key={session.id}>
      <span>{session.title} ({session.backend}/{session.model})</span>
      <button disabled={session.id === current.id} onClick={() => void send({ command: 'load_session', payload: { session_id: session.id } })} type="button">Load</button>
      <button disabled={session.id === current.id} onClick={() => void send({ command: 'delete_session', payload: { session_id: session.id } })} type="button">Delete</button>
    </li>)}</ul>}
  </section>;
}
