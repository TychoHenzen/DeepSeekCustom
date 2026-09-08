import { describe, expect, it, vi } from 'vitest';

import { BrowserEventClient } from './browser/BrowserEventClient.ts';
import type {
  ClientDependencies,
  EventMessage,
  EventStream,
} from './browser/client-types.ts';
import { emptyControlledDevelopmentState, type AppSnapshot } from './contracts.ts';

const requestTokenHeader = 'x-deepseek-request-token';

class FakeEvents implements EventStream {
  readonly listeners = new Map<string, (event: EventMessage) => void>();
  closed = false;
  onerror: (() => void) | null = null;
  onopen: (() => void) | null = null;

  addEventListener(type: 'change' | 'reset', listener: (event: EventMessage) => void): void {
    this.listeners.set(type, listener);
  }

  emit(type: 'change' | 'reset', value: unknown): void {
    this.listeners.get(type)?.({ data: JSON.stringify(value) });
  }

  emitRaw(type: 'change' | 'reset', data: string): void {
    this.listeners.get(type)?.({ data });
  }

  close(): void {
    this.closed = true;
  }
}

function snapshot(revision = 0, workspace: AppSnapshot['workspace'] = 'chat'): AppSnapshot {
  return {
    revision,
    workspace,
    transcript: [],
    session: { id: 'session-1', title: 'Current', backend: 'stub', model: 'deterministic' },
    pending_session_switch: null,
    saved_sessions: [],
    settings: {
      backends: [{ name: 'stub', configured_model: 'deterministic', models: ['deterministic'] }],
      selected_backend: 'stub',
      selected_model: 'deterministic',
      effort: 'high',
      context_budget: 32000,
      show_raw_output: false,
      max_tokens: 4096,
      working_dir: null,
      style: { plain_language: true, target_grade: 8 },
      voice: {
        enabled: false,
        stt_enabled: false,
        tts_enabled: false,
        trigger_mode: 'push_to_talk',
        wake_phrase: 'computer',
        tts_voice: 'af_sarah',
        tts_speed: 1,
      },
      procedure: { localization_backend: null, local_patch_backend: null, frontier_patch_backend: null, index_max_files: 10000, index_max_total_bytes: 67108864, verifier_commands: [] },
    },
    operations: [],
    controlled_development: emptyControlledDevelopmentState(),
  };
}

function response(body: unknown, status = 200, headers?: HeadersInit): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json', ...headers },
  });
}

function harness(responses: Array<Response | Error>) {
  const streams: FakeEvents[] = [];
  const eventUrls: string[] = [];
  const scheduled: Array<() => void> = [];
  const calls: Array<{ input: string; init?: RequestInit }> = [];
  const dependencies: ClientDependencies = {
    fetch: vi.fn((input: string, init?: RequestInit): Promise<Response> => {
      calls.push(init === undefined ? { input } : { input, init });
      const next = responses.shift();
      if (next === undefined) return Promise.reject(new Error(`unexpected fetch ${input}`));
      if (next instanceof Error) return Promise.reject(next);
      return Promise.resolve(next);
    }),
    openEvents: (url) => {
      eventUrls.push(url);
      const stream = new FakeEvents();
      streams.push(stream);
      return stream;
    },
    scheduleReconnect: (callback) => scheduled.push(callback),
  };
  return { client: new BrowserEventClient(dependencies), calls, eventUrls, scheduled, streams };
}

async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

describe('BrowserEventClient', () => {
  it('bootstraps a complete snapshot, captures the header token, and opens replay after its revision', async () => {
    const test = harness([response(snapshot(4), 200, { [requestTokenHeader]: 'process-token' })]);

    await test.client.start();

    expect(test.client.view).toMatchObject({ status: 'online', snapshot: { revision: 4 } });
    expect(test.calls[0]).toMatchObject({ input: '/api/bootstrap', init: { credentials: 'same-origin' } });
    expect(test.eventUrls).toEqual(['/api/events?after=4']);
  });

  it('sends the current revision and token without trying to set the browser-owned Origin header', async () => {
    const test = harness([
      response(snapshot(), 200, { [requestTokenHeader]: 'process-token' }),
      response({ status: 'applied', revision: 1 }),
      response(snapshot(1, 'settings')),
    ]);
    await test.client.start();

    const result = await test.client.send({ command: 'select_workspace', payload: { workspace: 'settings' } });

    expect(result).toEqual({ status: 'applied', revision: 1 });
    const command = test.calls[1];
    expect(command?.input).toBe('/api/commands');
    const body = command?.init?.body;
    expect(typeof body).toBe('string');
    expect(JSON.parse(body as string)).toEqual({
      revision: 0,
      command: 'select_workspace',
      payload: { workspace: 'settings' },
    });
    expect(new Headers(command?.init?.headers).get(requestTokenHeader)).toBe('process-token');
    expect(new Headers(command?.init?.headers).has('Origin')).toBe(false);
    expect(test.client.view.snapshot?.workspace).toBe('settings');
  });

  it('refreshes a conflict once and never repeats the rejected command', async () => {
    const test = harness([
      response(snapshot(), 200, { [requestTokenHeader]: 'process-token' }),
      response({ status: 'conflict', current_revision: 3 }, 409),
      response(snapshot(3, 'procedure')),
    ]);
    await test.client.start();

    const result = await test.client.send({ command: 'select_workspace', payload: { workspace: 'tests' } });

    expect(result).toEqual({ status: 'conflict', current_revision: 3 });
    expect(test.calls.map((call) => call.input)).toEqual(['/api/bootstrap', '/api/commands', '/api/snapshot']);
    expect(test.client.view.snapshot).toMatchObject({ revision: 3, workspace: 'procedure' });
    expect(test.client.view.message).toBe(
      'The command was not applied because application state changed. The latest state is now shown.',
    );
  });

  it('applies ordered events exactly once and refreshes when a revision gap appears', async () => {
    const test = harness([
      response(snapshot(), 200, { [requestTokenHeader]: 'process-token' }),
      response(snapshot(4, 'evolve')),
    ]);
    await test.client.start();
    const stream = test.streams[0];

    stream?.emit('change', { revision: 1, type: 'workspace_selected', value: 'cascade' });
    stream?.emit('change', { revision: 1, type: 'workspace_selected', value: 'settings' });
    await settle();
    expect(test.client.view.snapshot).toMatchObject({ revision: 1, workspace: 'cascade' });

    stream?.emit('change', { revision: 4, type: 'workspace_selected', value: 'tests' });
    await vi.waitFor(() => {
      expect(test.client.view.snapshot).toMatchObject({ revision: 4, workspace: 'evolve' });
    });
    expect(test.calls.map((call) => call.input)).toContain('/api/snapshot');
  });

  it('preserves terminal transcript fields received through the event stream', async () => {
    const test = harness([
      response(snapshot(), 200, { [requestTokenHeader]: 'process-token' }),
    ]);
    await test.client.start();

    test.streams[0]?.emit('change', {
      revision: 1,
      type: 'transcript_appended',
      value: {
        id: 7,
        type: 'terminal',
        outcome: 'completed',
        message: 'Response complete',
      },
    });
    await settle();

    expect(test.client.view.snapshot?.transcript).toEqual([{
      id: 7,
      type: 'terminal',
      outcome: 'completed',
      message: 'Response complete',
    }]);
  });

  it('parses controlled state changes and sends card and session identities unchanged', async () => {
    const initial = snapshot();
    const controlled = {
      enabled: true,
      phase: 'awaiting_approval',
      packet_id: 'card-9',
      card: {
        id: 'card-9', outcome: 'One file changes.', proof_commands: ['cargo check'],
        production_paths: ['src/lib.rs'], supporting_paths: ['tests/lib.test.ts'],
        excluded: ['settings.json'], complexity_exceptions: [],
      },
      structural_errors: [], changed_paths: [], proof_results: [],
      progress_notice: 'Phase: Awaiting approval.', completion_summary: null, compact_result: null,
      blocker: null, raw_details: [], retained_evidence: false, limitation: 'Visible limitation.',
    };
    const test = harness([
      response(initial, 200, { [requestTokenHeader]: 'process-token' }),
      response({ status: 'applied', revision: 2 }),
      response({ ...initial, revision: 2, controlled_development: controlled }),
    ]);
    await test.client.start();

    const result = await test.client.send({
      command: 'approve_controlled_development',
      payload: { session_id: 'session-1', card_id: 'card-9' },
    });

    expect(result).toEqual({ status: 'applied', revision: 2 });
    expect(JSON.parse(test.calls[1]?.init?.body as string)).toEqual({
      revision: 0,
      command: 'approve_controlled_development',
      payload: { session_id: 'session-1', card_id: 'card-9' },
    });
    expect(test.client.view.snapshot?.controlled_development).toMatchObject({
      phase: 'awaiting_approval', packet_id: 'card-9', card: { id: 'card-9' },
    });
  });

  it('replaces all state on reset and reconnects from the last applied revision', async () => {
    const test = harness([response(snapshot(2), 200, { [requestTokenHeader]: 'process-token' })]);
    await test.client.start();
    const first = test.streams[0];
    first?.emit('reset', { revision: 8, type: 'reset', value: snapshot(8, 'tests') });
    await settle();
    expect(test.client.view.snapshot).toMatchObject({ revision: 8, workspace: 'tests' });

    first?.onerror?.();
    expect(test.client.view.status).toBe('offline');
    expect(test.scheduled).toHaveLength(1);
    test.scheduled[0]?.();
    expect(test.eventUrls).toEqual(['/api/events?after=2', '/api/events?after=8']);
  });

  it('keeps network failures recoverable and retries bootstrap explicitly', async () => {
    const test = harness([
      new Error('server unavailable'),
      response(snapshot(5), 200, { [requestTokenHeader]: 'new-token' }),
    ]);

    await test.client.start();
    expect(test.client.view).toMatchObject({ status: 'offline', message: 'bootstrap failed: server unavailable' });

    await test.client.reconnect();
    expect(test.client.view).toMatchObject({ status: 'online', snapshot: { revision: 5 } });
  });

  it('makes malformed bootstrap and event contracts visible as fatal state', async () => {
    const bootstrap = harness([response({ revision: 'wrong' }, 200, { [requestTokenHeader]: 'token' })]);
    await bootstrap.client.start();
    expect(bootstrap.client.view.status).toBe('fatal');

    const events = harness([response(snapshot(), 200, { [requestTokenHeader]: 'token' })]);
    await events.client.start();
    events.streams[0]?.emitRaw('change', '{not-json');
    await settle();
    expect(events.client.view).toMatchObject({ status: 'fatal' });
    expect(events.client.view.message).toContain('event contract is incompatible');
  });
});
