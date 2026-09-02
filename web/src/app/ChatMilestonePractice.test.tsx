import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';

import { App, type UiClient } from './App.tsx';
import type { ClientView } from '../client/browser/client-types.ts';
import { emptyControlledDevelopmentState, type AppCommand, type AppSnapshot } from '../client/contracts.ts';

afterEach(cleanup);

function baseSnapshot(): AppSnapshot {
  return {
    revision: 1, workspace: 'chat', transcript: [],
    session: { id: 'current', title: 'Current', backend: 'stub', model: 'deterministic' },
    saved_sessions: [{ id: 'saved', title: 'Saved turn', backend: 'stub', model: 'deterministic' }], pending_session_switch: null,
    settings: {
      backends: [{ name: 'stub', configured_model: 'deterministic', models: ['deterministic'] }], selected_backend: 'stub', selected_model: 'deterministic', effort: 'high', context_budget: 32000, show_raw_output: false, max_tokens: 4096, working_dir: null,
      style: { plain_language: true, target_grade: 8 },
      voice: { enabled: true, stt_enabled: true, tts_enabled: true, trigger_mode: 'push_to_talk', wake_phrase: 'computer', tts_voice: 'af_sarah', tts_speed: 1 },
      procedure: { localization_backend: null, local_patch_backend: null, frontier_patch_backend: null, index_max_files: 10000, index_max_total_bytes: 67108864, verifier_commands: [] },
    }, operations: [], controlled_development: emptyControlledDevelopmentState(),
  };
}

function practiceClient(initial: AppSnapshot) {
  let view: ClientView = { status: 'online', snapshot: initial, lastError: null, message: null };
  const listeners = new Set<(next: ClientView) => void>();
  const send = vi.fn((command: AppCommand) => {
    if (command.command === 'select_workspace' && view.snapshot) publish({ ...view, snapshot: { ...view.snapshot, revision: view.snapshot.revision + 1, workspace: command.payload.workspace } });
    return Promise.resolve({ status: 'applied' as const, revision: view.snapshot?.revision ?? 0 });
  });
  function publish(next: ClientView) { view = next; listeners.forEach((listener) => listener(view)); }
  const client: UiClient = {
    get view() { return view; },
    subscribe(listener) { listeners.add(listener); listener(view); return () => listeners.delete(listener); },
    start: vi.fn(() => Promise.resolve()), reconnect: vi.fn(() => Promise.resolve()), send,
    uploadAttachment: vi.fn(() => Promise.resolve({ attachment_id: 'image-practice', media_type: 'image/png', size: 4 })),
    clearAttachment: vi.fn(() => Promise.resolve()), close: vi.fn(),
  };
  return { client, publish, send, current: () => view };
}

it('verifies the complete chat milestone through the mounted application paths', async () => {
  vi.stubGlobal('URL', { ...URL, createObjectURL: vi.fn(() => 'blob:practice'), revokeObjectURL: vi.fn() });
  const practice = practiceClient(baseSnapshot());
  render(<App client={practice.client} />);

  const image = new File(['png'], 'practice.png', { type: 'image/png' });
  await userEvent.upload(screen.getByLabelText('Select image'), image);
  expect(await screen.findByText(/Accepted image ready/)).toBeVisible();
  await userEvent.type(screen.getByLabelText('Message'), 'stream this');
  await userEvent.click(screen.getByRole('button', { name: 'Send message' }));
  expect(practice.send).toHaveBeenCalledWith({ command: 'send_message', payload: { text: 'stream this', attachment_id: 'image-practice' } });

  const streaming = baseSnapshot();
  streaming.revision = 8;
  streaming.transcript = [
    { id: 1, type: 'user', text: 'stream this', has_image: true },
    { id: 2, type: 'assistant', spans: [{ type: 'reasoning', text: 'inspect' }, { type: 'text', text: 'done' }] },
    { id: 3, type: 'terminal', outcome: 'completed', message: 'Response complete' },
  ];
  streaming.operations = [{ kind: 'voice', operation_id: 'voice-1', phase: 'running', progress: null, message: 'Listening', error: null }];
  practice.publish({ status: 'online', snapshot: streaming, lastError: null, message: null });
  await waitFor(() => expect(screen.getByRole('log')).toHaveTextContent(/stream this.*inspect.*done.*Response complete/s));
  expect(screen.getByText('Listening')).toBeVisible();
  await userEvent.pointer([{ target: screen.getByRole('button', { name: 'Hold to talk' }), keys: '[MouseLeft>]' }, { keys: '[/MouseLeft]' }]);
  expect(practice.send).toHaveBeenCalledWith({ command: 'start_voice_capture' });
  expect(practice.send).toHaveBeenCalledWith({ command: 'stop_voice_capture' });

  practice.publish({ ...practice.current(), status: 'offline', message: 'Connection lost.' });
  await userEvent.click(await screen.findByRole('button', { name: 'Retry connection' }));
  expect(practice.client.reconnect).toHaveBeenCalledOnce();

  const pending = { ...streaming, revision: 9, workspace: 'sessions' as const, pending_session_switch: { type: 'load' as const, session_id: 'saved' } };
  practice.publish({ status: 'online', snapshot: pending, lastError: null, message: null });
  expect(await screen.findByText(/pending until the active turn ends/)).toBeVisible();
  const switched: AppSnapshot = { ...pending, revision: 10, pending_session_switch: null, session: pending.saved_sessions[0]! };
  practice.publish({ status: 'online', snapshot: switched, lastError: null, message: null });
  expect(await screen.findByText('Current: Saved turn')).toBeVisible();

  const settings = { ...switched, revision: 11, workspace: 'settings' as const };
  practice.publish({ status: 'online', snapshot: settings, lastError: null, message: null });
  await userEvent.selectOptions(await screen.findByLabelText('Effort'), 'max');
  await userEvent.click(screen.getByRole('button', { name: 'Save settings' }));
  const settingsCommand = practice.send.mock.calls.map(([command]) => command).find((command) => command.command === 'update_settings');
  expect(settingsCommand?.command === 'update_settings' && settingsCommand.payload.settings.effort).toBe('max');
  const persisted = { ...settings, revision: 12, settings: { ...settings.settings, effort: 'max', working_dir: 'C:\\practice' } };
  practice.publish({ status: 'online', snapshot: persisted, lastError: null, message: 'Folder selection confirmed.' });
  expect(await screen.findByText(/Working directory: C:\\practice/)).toBeVisible();
  await userEvent.click(screen.getByRole('button', { name: 'Choose folder' }));
  expect(practice.send).toHaveBeenCalledWith({ command: 'pick_working_directory' });
  practice.publish({ status: 'online', snapshot: { ...persisted, revision: 13 }, lastError: null, message: 'Folder selection cancelled. Working directory unchanged.' });
  expect(await screen.findByText(/Folder selection cancelled/)).toBeVisible();
  expect(screen.getByText(/Working directory: C:\\practice/)).toBeVisible();
});
