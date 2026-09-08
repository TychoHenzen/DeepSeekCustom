import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { SessionsWorkspace } from './SessionsWorkspace.tsx';

afterEach(cleanup);

it('shows deferred state and sends new, load, and delete commands', async () => {
  const send = vi.fn(() => Promise.resolve({ status: 'applied' as const, revision: 4 }));
  render(<SessionsWorkspace
    current={{ id: 'current', title: 'Current', backend: 'stub', model: 'test' }}
    pending={{ type: 'new' }}
    saved={[{ id: 'saved', title: 'Saved', backend: 'ollama', model: 'local' }]}
    send={send}
  />);

  expect(screen.getByRole('status')).toHaveTextContent('pending until the active turn ends');
  await userEvent.click(screen.getByRole('button', { name: 'New session' }));
  await userEvent.click(screen.getByRole('button', { name: 'Load' }));
  await userEvent.click(screen.getByRole('button', { name: 'Delete' }));
  expect(send.mock.calls).toEqual([
    [{ command: 'new_session' }],
    [{ command: 'load_session', payload: { session_id: 'saved' } }],
    [{ command: 'delete_session', payload: { session_id: 'saved' } }],
  ]);
});
