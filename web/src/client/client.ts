import {
  type AppChange,
  type AppCommand,
  type AppCommandResult,
  type AppError,
  type AppSnapshot,
  parseChange,
  parseCommandResult,
  parseSnapshot,
} from './contracts.ts';

export const requestTokenHeader = 'x-deepseek-request-token';

export type ConnectionStatus = 'connecting' | 'online' | 'offline' | 'fatal';

export interface ClientView {
  status: ConnectionStatus;
  snapshot: AppSnapshot | null;
  lastError: AppError | null;
  message: string | null;
}

export interface EventMessage {
  data: string;
}

export interface UploadedAttachment {
  attachment_id: string;
  media_type: string;
  size: number;
}

export interface EventStream {
  close(): void;
  addEventListener(type: 'change' | 'reset', listener: (event: EventMessage) => void): void;
  onerror: (() => void) | null;
  onopen: (() => void) | null;
}

export interface ClientDependencies {
  fetch(input: string, init?: RequestInit): Promise<Response>;
  openEvents(url: string): EventStream;
  scheduleReconnect(callback: () => void, delayMs: number): void;
}

type Listener = (view: ClientView) => void;

export class ApplicationClient {
  readonly #dependencies: ClientDependencies;
  readonly #listeners = new Set<Listener>();
  #requestToken: string | null = null;
  #events: EventStream | null = null;
  #view: ClientView = { status: 'connecting', snapshot: null, lastError: null, message: null };

  constructor(dependencies: ClientDependencies = browserDependencies()) {
    this.#dependencies = dependencies;
  }

  get view(): ClientView {
    return this.#view;
  }

  subscribe(listener: Listener): () => void {
    this.#listeners.add(listener);
    listener(this.#view);
    return () => this.#listeners.delete(listener);
  }

  async start(): Promise<void> {
    this.#setView({ ...this.#view, status: 'connecting', message: null });
    let response: Response;
    try {
      response = await this.#dependencies.fetch('/api/bootstrap', {
        credentials: 'same-origin',
        headers: { Accept: 'application/json' },
      });
    } catch (error) {
      this.#setView({ ...this.#view, status: 'offline', message: `bootstrap failed: ${errorMessage(error)}` });
      return;
    }
    if (!response.ok) {
      this.#setView({ ...this.#view, status: 'offline', message: `bootstrap failed: HTTP ${response.status}` });
      return;
    }
    const token = response.headers.get(requestTokenHeader);
    if (token === null || token.length === 0) {
      this.#fatal(new ContractError('bootstrap request token is missing'), 'bootstrap failed');
      return;
    }
    try {
      const snapshot = parseSnapshot(await response.json());
      this.#requestToken = token;
      this.#setView({ status: 'online', snapshot, lastError: null, message: null });
      this.#connectEvents();
    } catch (error) {
      this.#fatal(error, 'bootstrap contract is incompatible');
    }
  }

  async reconnect(): Promise<void> {
    if (this.#view.status === 'fatal') return;
    if (this.#view.snapshot === null || this.#requestToken === null) {
      await this.start();
      return;
    }
    this.#connectEvents();
  }

  async send(command: AppCommand): Promise<AppCommandResult> {
    const snapshot = this.#view.snapshot;
    if (snapshot === null || this.#requestToken === null) throw new Error('application client is not bootstrapped');
    let response: Response;
    try {
      response = await this.#dependencies.fetch('/api/commands', {
        method: 'POST',
        credentials: 'same-origin',
        headers: {
          Accept: 'application/json',
          'Content-Type': 'application/json',
          [requestTokenHeader]: this.#requestToken,
        },
        body: JSON.stringify({ revision: snapshot.revision, ...command }),
      });
    } catch (error) {
      this.#setView({ ...this.#view, status: 'offline', message: `command failed: ${errorMessage(error)}` });
      throw error;
    }
    let result: AppCommandResult;
    try {
      result = parseCommandResult(await response.json());
    } catch (error) {
      this.#fatal(error, 'command response is incompatible');
      throw error;
    }
    if (response.status === 409 && result.status === 'conflict') {
      await this.#refreshSnapshot(
        'The command was not applied because application state changed. The latest state is now shown.',
      );
      return result;
    }
    if (response.status === 422 && result.status === 'rejected') {
      this.#setView({ ...this.#view, lastError: result.error });
      return result;
    }
    if (!response.ok || result.status !== 'applied') {
      const error = new Error(`command returned HTTP ${response.status}`);
      this.#fatal(error, 'command response status is incompatible');
      throw error;
    }
    if (result.revision > snapshot.revision) await this.#refreshSnapshot();
    return result;
  }

  async uploadAttachment(file: File): Promise<UploadedAttachment> {
    if (this.#requestToken === null) throw new Error('application client is not bootstrapped');
    const body = new FormData();
    body.append('image', file);
    const response = await this.#dependencies.fetch('/api/attachments', {
      method: 'POST', credentials: 'same-origin', headers: { [requestTokenHeader]: this.#requestToken }, body,
    });
    if (!response.ok) throw new Error(await response.text());
    return await response.json() as UploadedAttachment;
  }

  async clearAttachment(id: string): Promise<void> {
    if (this.#requestToken === null) throw new Error('application client is not bootstrapped');
    const response = await this.#dependencies.fetch(`/api/attachments/${encodeURIComponent(id)}`, {
      method: 'DELETE', credentials: 'same-origin', headers: { [requestTokenHeader]: this.#requestToken },
    });
    if (!response.ok && response.status !== 404) throw new Error(await response.text());
  }

  close(): void {
    this.#events?.close();
    this.#events = null;
  }

  async #refreshSnapshot(message: string | null = null): Promise<void> {
    let response: Response;
    try {
      response = await this.#dependencies.fetch('/api/snapshot', {
        credentials: 'same-origin',
        headers: { Accept: 'application/json' },
      });
    } catch (error) {
      this.#setView({ ...this.#view, status: 'offline', message: `snapshot failed: ${errorMessage(error)}` });
      throw error;
    }
    if (!response.ok) {
      const error = new Error(`snapshot returned HTTP ${response.status}`);
      this.#setView({ ...this.#view, status: 'offline', message: error.message });
      throw error;
    }
    try {
      const snapshot = parseSnapshot(await response.json());
      this.#setView({ ...this.#view, status: 'online', snapshot, message });
    } catch (error) {
      this.#fatal(error, 'snapshot contract is incompatible');
      throw error;
    }
  }

  #connectEvents(): void {
    const revision = this.#view.snapshot?.revision;
    if (revision === undefined) return;
    this.#events?.close();
    const stream = this.#dependencies.openEvents(`/api/events?after=${revision}`);
    this.#events = stream;
    stream.addEventListener('change', (event) => void this.#receive(event));
    stream.addEventListener('reset', (event) => void this.#receive(event));
    stream.onopen = () => this.#setView({ ...this.#view, status: 'online', message: null });
    stream.onerror = () => {
      if (this.#events !== stream || this.#view.status === 'fatal') return;
      stream.close();
      this.#events = null;
      this.#setView({ ...this.#view, status: 'offline', message: 'Connection lost. Reconnecting.' });
      this.#dependencies.scheduleReconnect(() => void this.reconnect(), 1_000);
    };
  }

  async #receive(event: EventMessage): Promise<void> {
    try {
      const change = parseChange(JSON.parse(event.data) as unknown);
      const snapshot = this.#view.snapshot;
      if (snapshot === null) throw new ContractError('event arrived before bootstrap');
      if (change.type === 'reset') {
        if (change.value.revision !== change.revision) throw new ContractError('reset revisions do not match');
        this.#setView({ ...this.#view, status: 'online', snapshot: change.value, message: null });
        return;
      }
      if (change.revision <= snapshot.revision) return;
      if (change.revision !== snapshot.revision + 1) {
        await this.#refreshSnapshot();
        return;
      }
      this.#setView({
        ...this.#view,
        status: 'online',
        snapshot: applyChange(snapshot, change),
        lastError: change.type === 'error' ? change.value : this.#view.lastError,
        message: null,
      });
    } catch (error) {
      this.#fatal(error, 'event contract is incompatible');
    }
  }

  #fatal(error: unknown, context: string): void {
    this.close();
    this.#setView({ ...this.#view, status: 'fatal', message: `${context}: ${errorMessage(error)}` });
  }

  #setView(view: ClientView): void {
    this.#view = view;
    this.#listeners.forEach((listener) => listener(view));
  }
}

class ContractError extends Error {}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function applyChange(snapshot: AppSnapshot, change: Exclude<AppChange, { type: 'reset' }>): AppSnapshot {
  switch (change.type) {
    case 'workspace_selected': return { ...snapshot, revision: change.revision, workspace: change.value };
    case 'transcript_appended': return { ...snapshot, revision: change.revision, transcript: [...snapshot.transcript, change.value] };
    case 'session_changed': return { ...snapshot, revision: change.revision, session: change.value };
    case 'saved_sessions_changed': return { ...snapshot, revision: change.revision, saved_sessions: change.value };
    case 'pending_session_switch_changed': return { ...snapshot, revision: change.revision, pending_session_switch: change.value };
    case 'settings_changed': return { ...snapshot, revision: change.revision, settings: change.value };
    case 'operation_changed': {
      const index = snapshot.operations.findIndex((operation) => operation.kind === change.value.kind);
      const operations = index < 0
        ? [...snapshot.operations, change.value]
        : snapshot.operations.map((operation, operationIndex) => operationIndex === index ? change.value : operation);
      return { ...snapshot, revision: change.revision, operations };
    }
    case 'tests_changed': return { ...snapshot, revision: change.revision, tests: change.value };
    case 'error': return { ...snapshot, revision: change.revision };
  }
}

function browserDependencies(): ClientDependencies {
  return {
    fetch: (input, init) => fetch(input, init),
    openEvents: (url) => new BrowserEventStream(url),
    scheduleReconnect: (callback, delayMs) => window.setTimeout(callback, delayMs),
  };
}

class BrowserEventStream implements EventStream {
  readonly #source: EventSource;

  constructor(url: string) {
    this.#source = new EventSource(url);
  }

  set onerror(listener: (() => void) | null) {
    this.#source.onerror = listener;
  }

  set onopen(listener: (() => void) | null) {
    this.#source.onopen = listener;
  }

  addEventListener(type: 'change' | 'reset', listener: (event: EventMessage) => void): void {
    this.#source.addEventListener(type, (event) => listener({ data: (event as MessageEvent<string>).data }));
  }

  close(): void {
    this.#source.close();
  }
}
