import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { emptyControlledDevelopmentState, type ControlledDevelopmentState } from '../client/contracts.ts';
import { ChatWorkspace } from './ChatWorkspace.tsx';

afterEach(cleanup);

it('submits accepted text and image identities and displays ordered running and terminal projection', async () => {
  const send = vi.fn(() => Promise.resolve({ status: 'applied' as const, revision: 9 }));
  const stop = vi.fn(() => Promise.resolve({ status: 'applied' as const, revision: 10 }));
  const { rerender } = render(<ChatWorkspace acceptedAttachmentId="image-1" controlledDevelopment={emptyControlledDevelopmentState()} operation={null} send={send} sessionId="session-1" stop={stop} transcript={[]} />);
  await userEvent.type(screen.getByLabelText('Message'), 'hello');
  await userEvent.click(screen.getByRole('button', { name: 'Send message' }));
  expect(send).toHaveBeenCalledWith({ command: 'send_message', payload: { text: 'hello', attachment_id: 'image-1' } });

  rerender(<ChatWorkspace controlledDevelopment={emptyControlledDevelopmentState()} operation={{ kind: 'chat', operation_id: 'chat-1', phase: 'running', progress: null, message: 'Generating response', error: null }} send={send} sessionId="session-1" stop={stop} transcript={[
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

  rerender(<ChatWorkspace controlledDevelopment={emptyControlledDevelopmentState()} operation={{ kind: 'chat', operation_id: 'chat-1', phase: 'completed', progress: null, message: 'Response complete', error: null }} send={send} sessionId="session-1" stop={stop} transcript={[{ id: 6, type: 'terminal', outcome: 'completed', message: 'Response complete' }]} />);
  expect(screen.getAllByText(/Response complete/)).toHaveLength(2);
  expect(screen.getAllByRole('status').some((status) => status.textContent?.includes('Turn completed'))).toBe(true);
});

// covers: deepseek-custom/controlled-development-mode :: The existing web application exposes Controlled Development :: User reviews and approves a Work Card
it('renders the complete current Work Card and sends session-specific review actions', async () => {
  const send = vi.fn(() => Promise.resolve({ status: 'applied' as const, revision: 9 }));
  const stop = vi.fn(() => Promise.resolve({ status: 'applied' as const, revision: 10 }));
  const controlled: ControlledDevelopmentState = {
    enabled: true,
    phase: 'awaiting_approval',
    packet_id: 'card-7',
    card: {
      id: 'card-7',
      outcome: 'The parser returns one bounded result.',
      proof_commands: ['cargo test parser'],
      production_paths: ['src/parser.rs'],
      supporting_paths: ['tests/parser.test.ts'],
      excluded: ['settings.json'],
      complexity_exceptions: ['No new dependencies'],
    },
    structural_errors: [],
    changed_paths: ['src/parser.rs'],
    proof_results: [{ command: 'cargo test parser', disposition: 'passed', success: true, exit_code: 0 }],
    progress_notice: 'Phase: Awaiting approval. A Work Card is recorded. 1 changed path and 1 proof result are recorded.',
    completion_summary: null,
    compact_result: null,
    blocker: null,
    raw_details: [],
    retained_evidence: false,
    limitation: 'Proof commands can address absolute paths outside the disposable workspace.',
  };

  const { rerender } = render(<ChatWorkspace controlledDevelopment={controlled} operation={null} send={send} sessionId="session-current" stop={stop} transcript={[]} />);

  expect(screen.getByRole('switch', { name: 'Controlled Development' })).toBeChecked();
  expect(screen.getByRole('status', { name: '' })).toHaveTextContent('Current phase: Awaiting Approval');
  expect(screen.getByRole('region', { name: 'Work Card' })).toHaveTextContent(/card-7.*bounded result.*cargo test parser.*src\/parser.rs.*tests\/parser.test.ts.*settings.json.*No new dependencies/s);
  expect(screen.getAllByText('src/parser.rs', { selector: 'code' })).toHaveLength(2);
  expect(screen.getByText(/No completion summary is available yet/)).toBeInTheDocument();
  expect(screen.getByText(/Proof commands can address absolute paths outside/)).toBeInTheDocument();

  await userEvent.click(screen.getByRole('button', { name: 'Approve Work Card' }));
  expect(send).toHaveBeenCalledWith({
    command: 'approve_controlled_development',
    payload: { session_id: 'session-current', card_id: 'card-7' },
  });
  await userEvent.click(screen.getByRole('button', { name: 'Reject Work Card' }));
  expect(send).toHaveBeenCalledWith({
    command: 'reject_controlled_development',
    payload: { session_id: 'session-current', card_id: 'card-7' },
  });
  await userEvent.click(screen.getByRole('button', { name: 'Stop controlled work' }));
  expect(send).toHaveBeenCalledWith({
    command: 'stop_controlled_development',
    payload: { session_id: 'session-current', packet_id: 'card-7' },
  });

  rerender(<ChatWorkspace controlledDevelopment={{ ...controlled, phase: 'completed' }} operation={null} send={send} sessionId="session-current" stop={stop} transcript={[]} />);
  const approve = screen.getByRole('button', { name: 'Approve Work Card' });
  const controlledStop = screen.getByRole('button', { name: 'Stop controlled work' });
  expect(approve).toBeDisabled();
  expect(controlledStop).toBeDisabled();
  expect(approve).toHaveAccessibleDescription(/until the current session has a Work Card awaiting approval/);
  expect(controlledStop).toHaveAccessibleDescription(/no active controlled packet/);
});

// covers: deepseek-custom/controlled-development-mode :: Compact output is bounded without hiding diagnostics :: Visible progress and completion summaries respect their limits
it('renders the bounded typed progress and completion projections without using raw failure text', () => {
  const controlled: ControlledDevelopmentState = {
    ...emptyControlledDevelopmentState(),
    enabled: true,
    phase: 'blocked',
    progress_notice: 'Phase: Blocked. A Work Card is recorded. 2 changed paths and 2 proof results are recorded. A failure is recorded in the blocker field.',
    completion_summary: 'Phase: Blocked. Work Card outcome is shown in full above. 2 exact changed paths are shown in the changed-paths field. Proof results: 1 passed, 1 failed, 0 interrupted, 0 not run. Complete failure evidence is shown in the blocker and raw-details fields.',
    compact_result: 'legacy projection is not rendered',
    blocker: 'complete backend failure output remains in its dedicated field',
  };

  render(<ChatWorkspace controlledDevelopment={controlled} operation={null} send={vi.fn()} sessionId="session-current" stop={vi.fn()} transcript={[]} />);

  expect(screen.getByRole('region', { name: 'Progress' })).toHaveTextContent(controlled.progress_notice);
  expect(screen.getByRole('region', { name: 'Completion summary' })).toHaveTextContent(controlled.completion_summary ?? '');
  expect(screen.getByRole('alert')).toHaveTextContent(controlled.blocker ?? '');
  expect(screen.queryByText('legacy projection is not rendered')).not.toBeInTheDocument();
});

// covers: deepseek-custom/controlled-development-mode :: Compact output is bounded without hiding diagnostics :: Raw diagnostic output remains available
it('keeps complete raw evidence in an accessible native details control that is closed by default', () => {
  const reasoning = 'reasoning token '.repeat(300);
  const tool = 'tool output\n'.repeat(300);
  const assistant = 'assistant text '.repeat(300);
  const verifier = 'verifier output\n'.repeat(300);
  const failure = 'failure evidence '.repeat(300);
  const controlled: ControlledDevelopmentState = {
    ...emptyControlledDevelopmentState(),
    enabled: true,
    phase: 'blocked',
    blocker: failure,
    raw_details: [
      { kind: 'backend_event', name: 'Backend reasoning', content: reasoning, truncated_at_source: false, bytes_seen: reasoning.length },
      { kind: 'backend_event', name: 'Tool event', content: tool, truncated_at_source: false, bytes_seen: tool.length },
      { kind: 'backend_event', name: 'Assistant text', content: assistant, truncated_at_source: false, bytes_seen: assistant.length },
      { kind: 'verifier_combined_output', name: 'Verifier output', content: verifier, truncated_at_source: false, bytes_seen: verifier.length },
      { kind: 'failure', name: 'Failure evidence', content: failure, truncated_at_source: false, bytes_seen: failure.length },
    ],
  };

  const { container } = render(<ChatWorkspace controlledDevelopment={controlled} operation={null} send={vi.fn()} sessionId="session-current" stop={vi.fn()} transcript={[]} />);
  const details = container.querySelector('details[aria-label="Controlled Development raw details"]');
  expect(details).toBeInTheDocument();
  expect(details).not.toHaveAttribute('open');
  expect(screen.getByRole('region', { name: 'Backend reasoning' }).querySelector('pre')?.textContent).toBe(reasoning);
  expect(screen.getByRole('region', { name: 'Tool event' }).querySelector('pre')?.textContent).toBe(tool);
  expect(screen.getByRole('region', { name: 'Assistant text' }).querySelector('pre')?.textContent).toBe(assistant);
  expect(screen.getByRole('region', { name: 'Verifier output' }).querySelector('pre')?.textContent).toBe(verifier);
  expect(screen.getByRole('region', { name: 'Failure evidence' }).querySelector('pre')?.textContent).toBe(failure);
  expect(details?.querySelector('summary')).toHaveTextContent('Raw details for Blocked (5 entries)');
});

it('keeps a rejected controlled action visible next to the panel', async () => {
  const controlled = emptyControlledDevelopmentState();
  const send = vi.fn(() => Promise.resolve({
    status: 'rejected' as const,
    error: {
      code: 'service_failed' as const,
      message: 'controlled service unavailable',
      recoverable: true,
      field: 'controlled_development',
    },
  }));
  render(<ChatWorkspace controlledDevelopment={controlled} operation={null} send={send} sessionId="session-current" stop={vi.fn()} transcript={[]} />);

  await userEvent.click(screen.getByRole('switch', { name: 'Controlled Development' }));

  expect(send).toHaveBeenCalledWith({
    command: 'set_controlled_development_enabled',
    payload: { session_id: 'session-current', enabled: true },
  });
  expect(screen.getByRole('alert')).toHaveTextContent('controlled service unavailable');
});
