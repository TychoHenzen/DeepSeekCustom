import type {
  AppCommand,
  AppCommandResult,
  AppError,
  AppSnapshot,
} from '../contracts.ts';

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

export interface BrowserClient {
  readonly view: ClientView;
  subscribe: (listener: (view: ClientView) => void) => () => void;
  start: () => Promise<void>;
  reconnect: () => Promise<void>;
  send: (command: AppCommand) => Promise<AppCommandResult>;
  uploadAttachment: (file: File) => Promise<UploadedAttachment>;
  clearAttachment: (id: string) => Promise<void>;
  close: () => void;
}
