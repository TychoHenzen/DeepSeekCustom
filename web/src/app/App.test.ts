import '@testing-library/jest-dom/vitest';

import { readFileSync } from 'node:fs';
import { createElement } from 'react';

import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { App, type UiClient } from './App.tsx';
import type { ClientView } from '../client/client.ts';
import type { AppCommand, AppSnapshot } from '../client/contracts.ts';

afterEach(cleanup);

const labels = ['Chat', 'Autopilot', 'Cascade', 'Evolve', 'Procedure', 'Sessions', 'Tests', 'Settings'];
const appCss = readFileSync('src/app/app.css', 'utf8');

function snapshot(workspace: AppSnapshot['workspace'] = 'chat'): AppSnapshot {
  return {
    revision: 7,
    workspace,
    transcript: [],
    session: { id: 'session-1', title: 'Current', backend: 'stub', model: 'deterministic' },
    pending_session_switch: null,
    settings: {
      selected_backend: 'stub', selected_model: 'deterministic', effort: 'high', context_budget: 4096,
      show_raw_output: false, working_dir: null, style: { plain_language: true, target_grade: 8 },
      voice: { enabled: false, stt_enabled: false, tts_enabled: false, trigger_mode: 'push_to_talk', wake_phrase: 'computer', tts_voice: 'af_sarah', tts_speed: 1 },
    },
    operations: [],
  };
}

function client(initial: ClientView): UiClient & { send: ReturnType<typeof vi.fn> } {
  let view = initial;
  const listeners = new Set<(next: ClientView) => void>();
  const send = vi.fn((command: AppCommand) => {
    if (command.command === 'select_workspace' && view.snapshot !== null) {
      view = { ...view, snapshot: { ...view.snapshot, revision: view.snapshot.revision + 1, workspace: command.payload.workspace } };
      listeners.forEach((listener) => listener(view));
    }
    return Promise.resolve({ status: 'applied' as const, revision: view.snapshot?.revision ?? 0 });
  });
  return {
    get view() { return view; },
    subscribe(listener) { listeners.add(listener); listener(view); return () => listeners.delete(listener); },
    start: vi.fn(() => Promise.resolve()),
    reconnect: vi.fn(() => Promise.resolve()),
    send,
    close: vi.fn(),
  };
}

describe('application shell', () => {
  // covers: deepseek-custom/web-application :: Chat and saved sessions preserve their lifecycle :: User sends a chat turn
  it('submits a text turn through the current application command path', async () => {
    const appClient = client({ status: 'online', snapshot: snapshot(), lastError: null, message: null });
    render(createElement(App, { client: appClient }));
    await userEvent.type(screen.getByLabelText('Message'), 'hello');
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }));
    expect(appClient.send).toHaveBeenCalledWith({ command: 'send_message', payload: { text: 'hello', attachment_id: null } });
  });

  // covers: deepseek-custom/web-application :: The web frontend is responsive and accessible :: Desktop navigation
  it('exposes all primary workspaces directly and identifies authoritative active state', async () => {
    const appClient = client({ status: 'online', snapshot: snapshot('procedure'), lastError: null, message: null });
    render(createElement(App, { client: appClient }));

    const navigation = screen.getByRole('navigation', { name: 'Primary workspaces' });
    for (const label of labels) {
      expect(screen.getByRole('button', { name: new RegExp(`^${label}`) })).toBeVisible();
    }
    expect(navigation).toContainElement(screen.getByRole('button', { name: /^Procedure/ }));
    expect(screen.getByRole('button', { name: /^Procedure/ })).toHaveAttribute('aria-current', 'page');
    expect(screen.getByRole('main')).toHaveAccessibleName('Procedure');

    await userEvent.click(screen.getByRole('button', { name: /^Settings/ }));
    expect(appClient.send).toHaveBeenCalledWith({ command: 'select_workspace', payload: { workspace: 'settings' } });
    expect(screen.getByRole('button', { name: /^Settings/ })).toHaveAttribute('aria-current', 'page');
    expect(screen.getByRole('main')).toHaveAccessibleName('Settings');
  });

  // covers: deepseek-custom/web-application :: The web frontend is responsive and accessible :: Narrow navigation
  it('keeps navigation and actions in flow and declares a contained 360-pixel layout', () => {
    render(createElement(App, { client: client({ status: 'online', snapshot: snapshot(), lastError: null, message: null }) }));

    expect(screen.getAllByRole('button')).toHaveLength(9);
    expect(screen.getByRole('button', { name: 'Send message' })).toBeVisible();
    expect(appCss).toMatch(/@media \(max-width: 48rem\)[\s\S]*grid-template-columns:\s*repeat\(2, minmax\(0, 1fr\)\)/);
    expect(appCss).toMatch(/body\s*{[^}]*overflow-x:\s*hidden/);
    expect(appCss).toMatch(/\.app-shell\s*{[^}]*min-width:\s*0/);
    expect(appCss).toMatch(/\.workspace pre,[\s\S]*\.workspace table\s*{[^}]*overflow-x:\s*auto/);
    expect(appCss).toMatch(/\.workspace img,[\s\S]*max-width:\s*100%/);
  });

  // covers: deepseek-custom/web-application :: The web frontend is responsive and accessible :: Keyboard and assistive navigation
  it('provides logical focus, named state, live errors, focus styling, and disabled reasons', async () => {
    const user = userEvent.setup();
    render(createElement(App, { client: client({
      status: 'online',
      snapshot: snapshot(),
      lastError: { code: 'invalid_input', message: 'Prompt is required.', recoverable: true, field: 'prompt' },
      message: null,
    }) }));

    await user.tab();
    expect(screen.getByRole('link', { name: 'Skip to active workspace' })).toHaveFocus();
    await user.tab();
    expect(screen.getByRole('button', { name: /^Chat/ })).toHaveFocus();
    await user.tab();
    expect(screen.getByRole('button', { name: /^Autopilot/ })).toHaveFocus();

    expect(screen.getByRole('status')).toHaveTextContent('Connected. Application revision 7.');
    expect(screen.getByRole('alert')).toHaveTextContent('Error: Prompt is required. Field: prompt.');
    const disabledAction = screen.getByRole('button', { name: 'Send message' });
    expect(disabledAction).toBeDisabled();
    expect(screen.getByLabelText('Message')).toBeVisible();
    expect(screen.getByRole('button', { name: /^Chat/ })).toHaveTextContent('Active');
    expect(appCss).toMatch(/button:focus-visible,[\s\S]*outline:\s*3px solid/);
  });

  it('offers an explicit retry only for a recoverable offline state', async () => {
    const appClient = client({
      status: 'offline',
      snapshot: snapshot(),
      lastError: null,
      message: 'Connection lost.',
    });
    const user = userEvent.setup();
    render(createElement(App, { client: appClient }));

    expect(screen.getByRole('heading', { name: 'Connection unavailable' })).toBeVisible();
    await user.click(screen.getByRole('button', { name: 'Retry connection' }));
    expect(appClient.reconnect).toHaveBeenCalledOnce();
  });

  it('shows refreshed conflict state without dispatching the command twice', async () => {
    let view: ClientView = { status: 'online', snapshot: snapshot(), lastError: null, message: null };
    const listeners = new Set<(next: ClientView) => void>();
    const send = vi.fn(() => {
      view = {
        ...view,
        snapshot: { ...snapshot('procedure'), revision: 9 },
        message: 'The command was not applied because application state changed. The latest state is now shown.',
      };
      listeners.forEach((listener) => listener(view));
      return Promise.resolve({ status: 'conflict' as const, current_revision: 9 });
    });
    const appClient: UiClient = {
      get view() { return view; },
      subscribe(listener) { listeners.add(listener); listener(view); return () => listeners.delete(listener); },
      start: vi.fn(() => Promise.resolve()),
      reconnect: vi.fn(() => Promise.resolve()),
      send,
      close: vi.fn(),
    };
    render(createElement(App, { client: appClient }));

    await userEvent.click(screen.getByRole('button', { name: /^Settings/ }));

    expect(send).toHaveBeenCalledOnce();
    expect(screen.getByRole('button', { name: /^Procedure/ })).toHaveAttribute('aria-current', 'page');
    expect(screen.getAllByRole('status').some((status) => status.textContent?.includes('latest state is now shown'))).toBe(true);
    expect(screen.getAllByRole('status').some((status) => status.textContent?.includes('revision 9'))).toBe(true);
  });

  it('keeps fatal contract state distinct and does not offer retry', () => {
    render(createElement(App, { client: client({
      status: 'fatal',
      snapshot: snapshot(),
      lastError: null,
      message: 'snapshot contract is incompatible: revision must be a number',
    }) }));

    expect(screen.getByRole('alert')).toHaveTextContent('Fatal error: snapshot contract is incompatible');
    expect(screen.queryByRole('button', { name: 'Retry connection' })).not.toBeInTheDocument();
  });
});
