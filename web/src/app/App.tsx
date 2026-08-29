import { useEffect, useState } from 'react';

import type { ApplicationClient, ClientView } from '../client/client.ts';
import { workspaces, type AppCommand, type AppCommandResult, type Workspace } from '../client/contracts.ts';
import { ChatWorkspace } from './ChatWorkspace.tsx';
import { SessionsWorkspace } from './SessionsWorkspace.tsx';

const workspaceLabels: Record<Workspace, string> = {
  chat: 'Chat',
  autopilot: 'Autopilot',
  cascade: 'Cascade',
  evolve: 'Evolve',
  procedure: 'Procedure',
  sessions: 'Sessions',
  tests: 'Tests',
  settings: 'Settings',
};

export interface UiClient {
  readonly view: ClientView;
  subscribe: (listener: (view: ClientView) => void) => () => void;
  start: () => Promise<void>;
  reconnect: () => Promise<void>;
  send: (command: AppCommand) => Promise<AppCommandResult>;
  close: () => void;
}

export interface AppProps {
  client: UiClient;
}

export function App({ client }: AppProps) {
  const [view, setView] = useState(client.view);

  useEffect(() => {
    const unsubscribe = client.subscribe(setView);
    void client.start();
    return () => {
      unsubscribe();
      client.close();
    };
  }, [client]);

  const selectedWorkspace = view.snapshot?.workspace ?? 'chat';
  const workspaceLabel = workspaceLabels[selectedWorkspace];
  const navigationDisabled = view.status !== 'online' || view.snapshot === null;
  const navigationReason = connectionReason(view);

  async function selectWorkspace(workspace: Workspace): Promise<void> {
    if (navigationDisabled || workspace === selectedWorkspace) return;
    try {
      await client.send({ command: 'select_workspace', payload: { workspace } });
    } catch {
      // The client owns and publishes the resulting offline or fatal state.
    }
  }

  return (
    <div className="app-shell">
      <a className="skip-link" href="#workspace">Skip to active workspace</a>
      <header className="app-header">
        <div>
          <p className="eyebrow">Local agent workspace</p>
          <h1>DeepSeekCustom</h1>
        </div>
        <ConnectionStatus view={view} />
      </header>

      {view.status === 'offline' && (
        <section aria-labelledby="connection-recovery-title" className="connection-recovery">
          <h2 id="connection-recovery-title">Connection unavailable</h2>
          <p>{view.message ?? 'The local application service cannot be reached.'}</p>
          <button onClick={() => void client.reconnect()} type="button">Retry connection</button>
        </section>
      )}

      <nav aria-label="Primary workspaces" className="workspace-navigation">
        <ul>
          {workspaces.map((workspace) => {
            const label = workspaceLabels[workspace];
            const reasonId = `navigation-${workspace}-reason`;
            return (
              <li key={workspace}>
                <button
                  aria-current={workspace === selectedWorkspace ? 'page' : undefined}
                  aria-describedby={navigationDisabled ? reasonId : undefined}
                  disabled={navigationDisabled}
                  onClick={() => void selectWorkspace(workspace)}
                  type="button"
                >
                  {label}
                  {workspace === selectedWorkspace && <span className="selected-cue">Active</span>}
                </button>
                {navigationDisabled && <span className="visually-hidden" id={reasonId}>{navigationReason}</span>}
              </li>
            );
          })}
        </ul>
      </nav>

      <main aria-labelledby="workspace-title" className="workspace" id="workspace" tabIndex={-1}>
        <header className="workspace-heading">
          <p className="eyebrow">Active workspace</p>
          <h2 id="workspace-title">{workspaceLabel}</h2>
        </header>
        {selectedWorkspace === 'chat' && view.snapshot !== null ? (
          <ChatWorkspace
            operation={view.snapshot.operations.find((operation) => operation.kind === 'chat') ?? null}
            send={(command) => client.send(command)}
            stop={() => client.send({ command: 'stop_operation', payload: { kind: 'chat' } })}
            transcript={view.snapshot.transcript}
          />
        ) : selectedWorkspace === 'sessions' && view.snapshot !== null ? (
          <SessionsWorkspace
            current={view.snapshot.session}
            pending={view.snapshot.pending_session_switch}
            saved={view.snapshot.saved_sessions}
            send={(command) => client.send(command)}
          />
        ) : <section aria-labelledby="workspace-actions-title" className="workspace-actions">
          <h3 id="workspace-actions-title">{workspaceLabel} actions</h3>
          <p>Controls for this workspace will appear here.</p>
          <button aria-describedby="workspace-action-reason" disabled type="button">
            Start {workspaceLabel}
          </button>
          <p className="disabled-reason" id="workspace-action-reason">
            Unavailable while this workspace is being migrated.
          </p>
        </section>}
      </main>
    </div>
  );
}

function ConnectionStatus({ view }: { view: ClientView }) {
  const label = view.status === 'online'
    ? `Connected. Application revision ${view.snapshot?.revision ?? 0}.`
    : connectionReason(view);

  return (
    <div className={`connection-state connection-state-${view.status}`}>
      <p aria-live="polite" role="status">
        <span aria-hidden="true" className="state-symbol">{statusSymbol(view.status)}</span>
        {label}
      </p>
      {view.lastError !== null && (
        <p role="alert">
          Error: {view.lastError.message}
          {view.lastError.field !== null && ` Field: ${view.lastError.field}.`}
        </p>
      )}
      {view.status === 'online' && view.message !== null && <p role="status">Notice: {view.message}</p>}
      {view.status === 'fatal' && view.message !== null && <p role="alert">Fatal error: {view.message}</p>}
    </div>
  );
}

function connectionReason(view: ClientView): string {
  switch (view.status) {
    case 'connecting': return 'Navigation unavailable while the application connects.';
    case 'offline': return `Navigation unavailable while the application is offline.${view.message === null ? '' : ` ${view.message}`}`;
    case 'fatal': return `Navigation unavailable because the application cannot continue.${view.message === null ? '' : ` ${view.message}`}`;
    case 'online': return view.snapshot === null
      ? 'Navigation unavailable until application state is loaded.'
      : 'Navigation available.';
  }
}

function statusSymbol(status: ClientView['status']): string {
  switch (status) {
    case 'connecting': return '…';
    case 'online': return '✓';
    case 'offline': return '!';
    case 'fatal': return '×';
  }
}

export function browserClient(client: ApplicationClient): UiClient {
  return client;
}
