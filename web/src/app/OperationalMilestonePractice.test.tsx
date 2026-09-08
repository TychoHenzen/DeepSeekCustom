import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';

import type { AppCommand, OperationKind, OperationState } from '../client/contracts.ts';
import { OperationWorkspace } from './OperationWorkspace.tsx';

afterEach(cleanup);

const running = (kind: OperationKind): OperationState => ({ kind, operation_id: `${kind}-run`, phase: 'running', progress: { completed: 1, total: 3 }, message: 'stage: running', error: null });

it('practises deterministic operational success, stop, failure, review, and terminal runs', async () => {
  const send = vi.fn<(command: AppCommand) => Promise<{ status: 'applied'; revision: number }>>().mockResolvedValue({ status: 'applied', revision: 20 });
  const starts = [
    { kind: 'autopilot' as const, label: 'Autopilot', fill: async () => userEvent.type(screen.getByLabelText('Task'), 'repeat') },
    { kind: 'cascade' as const, label: 'Cascade', fill: async () => userEvent.type(screen.getByLabelText('Prompt'), 'search') },
    { kind: 'evolve' as const, label: 'Evolve', fill: async () => { await userEvent.type(screen.getByLabelText('Prompt'), 'evolve'); await userEvent.type(screen.getByLabelText('Fitness command'), 'score'); } },
    { kind: 'procedure' as const, label: 'Procedure', fill: async () => { await userEvent.type(screen.getByLabelText('Change ID'), 'change'); await userEvent.type(screen.getByLabelText('Task ID'), '5.7'); } },
  ];

  for (const item of starts) {
    const view = render(<OperationWorkspace activeOperation={null} backends={['worker']} kind={item.kind} operation={null} selectedBackend="worker" send={send} />);
    await item.fill();
    await userEvent.click(screen.getByRole('button', { name: `Start ${item.label}` }));
    expect(send.mock.calls.at(-1)?.[0].command).toMatch(/^start_|^run_procedure$/);
    view.unmount();
  }

  const live = running('autopilot');
  const view = render(<OperationWorkspace activeOperation={live} kind="autopilot" operation={live} send={send} />);
  await userEvent.click(screen.getByRole('button', { name: 'Stop operation' }));
  expect(send).toHaveBeenLastCalledWith({ command: 'stop_operation', payload: { kind: 'autopilot' } });

  view.rerender(<OperationWorkspace activeOperation={null} kind="autopilot" operation={{ ...live, phase: 'completed', progress: { completed: 3, total: 3 }, message: 'outcome: succeeded' }} send={send} />);
  expect(screen.getByRole('region', { name: 'Result summary' })).toHaveTextContent('succeeded');
  view.rerender(<OperationWorkspace activeOperation={null} kind="autopilot" operation={{ ...live, phase: 'failed', message: 'outcome: failed', error: { code: 'service_failed', message: 'scripted failure', recoverable: true, field: null } }} send={send} />);
  expect(screen.getByRole('alert')).toHaveTextContent('scripted failure');

  const review: OperationState = { ...running('procedure'), operation_id: 'review-run', phase: 'awaiting_review', progress: { completed: 3, total: 3 }, message: 'stage: review\nevidence: deterministic report' };
  view.rerender(<OperationWorkspace activeOperation={review} kind="procedure" operation={review} send={send} />);
  await userEvent.click(screen.getByRole('button', { name: 'Reject this run' }));
  expect(send).toHaveBeenLastCalledWith({ command: 'review_procedure', payload: { run_id: 'review-run', decision: 'reject' } });
  view.rerender(<OperationWorkspace activeOperation={null} kind="procedure" operation={{ ...review, phase: 'interrupted', message: 'outcome: interrupted' }} send={send} />);
  expect(screen.getByRole('region', { name: 'Result summary' })).toHaveTextContent('interrupted');
});
