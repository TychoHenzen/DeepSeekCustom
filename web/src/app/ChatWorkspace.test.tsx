import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { ChatWorkspace } from './ChatWorkspace.tsx';

afterEach(cleanup);

it('submits accepted text and image identities and displays ordered running and terminal projection', async () => {
  const send = vi.fn(() => Promise.resolve({ status: 'applied' as const, revision: 9 }));
  const stop = vi.fn(() => Promise.resolve({ status: 'applied' as const, revision: 10 }));
  const { rerender } = render(<ChatWorkspace acceptedAttachmentId="image-1" operation={null} send={send} stop={stop} transcript={[]} />);
  await userEvent.type(screen.getByLabelText('Message'), 'hello');
  await userEvent.click(screen.getByRole('button', { name: 'Send message' }));
  expect(send).toHaveBeenCalledWith({ command: 'send_message', payload: { text: 'hello', attachment_id: 'image-1' } });

  rerender(<ChatWorkspace operation={{ kind: 'chat', operation_id: 'chat-1', phase: 'running', progress: null, message: 'Generating response', error: null }} send={send} stop={stop} transcript={[
    { id: 1, type: 'user', text: 'hello', has_image: true },
    { id: 2, type: 'assistant', spans: [{ type: 'reasoning', text: 'thinking' }, { type: 'text', text: 'answer' }] },
    { id: 3, type: 'tool_call', tool: 'read', args: '{}', output: 'ok', is_error: false },
    { id: 4, type: 'notice', level: 'info', message: 'notice' },
    { id: 5, type: 'image', media_type: 'image/png', data: 'AA==' },
  ]} />);
  expect(screen.getByRole('log')).toHaveTextContent(/hello.*Reasoning.*thinking.*answer.*Tool: read.*notice.*Image/s);
  expect(screen.getAllByRole('status').some((status) => status.textContent?.includes('Turn running'))).toBe(true);
  expect(screen.getByRole('button', { name: 'Send message' })).toBeDisabled();
  await userEvent.click(screen.getByRole('button', { name: 'Stop' }));
  expect(stop).toHaveBeenCalledOnce();

  rerender(<ChatWorkspace operation={{ kind: 'chat', operation_id: 'chat-1', phase: 'completed', progress: null, message: 'Response complete', error: null }} send={send} stop={stop} transcript={[{ id: 6, type: 'terminal', outcome: 'completed', message: 'Response complete' }]} />);
  expect(screen.getAllByText(/Response complete/)).toHaveLength(2);
  expect(screen.getByRole('status')).toHaveTextContent('Turn completed');
});
