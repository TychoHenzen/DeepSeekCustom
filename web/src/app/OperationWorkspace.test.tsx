import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';

import type { AppCommand } from '../client/contracts.ts';
import { OperationWorkspace } from './OperationWorkspace.tsx';

afterEach(cleanup);

it('validates and starts an operation through its existing command', async () => {
  const send = vi.fn(() => Promise.resolve({ status: 'applied' as const, revision: 4 }));
  render(<OperationWorkspace activeOperation={null} kind="autopilot" operation={null} send={send} />);
  await userEvent.click(screen.getByRole('button', { name: 'Start Autopilot' }));
  expect(screen.getByRole('alert')).toHaveTextContent('Task is required');
  await userEvent.type(screen.getByLabelText('Task'), 'repair tests');
  await userEvent.clear(screen.getByLabelText('Iterations'));
  await userEvent.type(screen.getByLabelText('Iterations'), '4');
  await userEvent.click(screen.getByRole('button', { name: 'Start Autopilot' }));
  expect(send).toHaveBeenCalledWith({ command: 'start_autopilot', payload: { task: 'repair tests', iterations: 4 } });
});

it('shows bounded progress, stop, terminal result, and cross-operation exclusion', async () => {
  const send = vi.fn(() => Promise.resolve({ status: 'applied' as const, revision: 8 }));
  const running = { kind: 'cascade' as const, operation_id: 'cascade-1', phase: 'running' as const, progress: { completed: 2, total: 5 }, message: 'candidate two\nlong output', error: null };
  const { rerender } = render(<OperationWorkspace activeOperation={running} kind="cascade" operation={running} send={send} />);
  expect(screen.getByRole('log')).toHaveTextContent('candidate two');
  expect(screen.getByText(/2 of 5/)).toBeVisible();
  await userEvent.click(screen.getByRole('button', { name: 'Stop operation' }));
  expect(send).toHaveBeenCalledWith({ command: 'stop_operation', payload: { kind: 'cascade' } });

  rerender(<OperationWorkspace activeOperation={running} kind="evolve" operation={null} send={send} />);
  expect(screen.getByRole('button', { name: 'Start Evolve' })).toBeDisabled();
  expect(screen.getByText(/Cascade is active/)).toBeVisible();

  rerender(<OperationWorkspace activeOperation={null} kind="cascade" operation={{ ...running, phase: 'completed', progress: { completed: 5, total: 5 }, message: 'five candidates complete' }} send={send} />);
  expect(screen.getByRole('region', { name: 'Result summary' })).toHaveTextContent('five candidates complete');
  expect(screen.queryByRole('button', { name: 'Stop operation' })).not.toBeInTheDocument();
});

it('submits the complete validated Cascade parameter set', async () => {
  const send = vi.fn(() => Promise.resolve({ status: 'applied' as const, revision: 9 }));
  render(<OperationWorkspace activeOperation={null} backends={['cheap', 'strong']} kind="cascade" operation={null} selectedBackend="cheap" send={send} />);
  await userEvent.type(screen.getByLabelText('Prompt'), 'solve this');
  await userEvent.clear(screen.getByLabelText('Vote margin'));
  await userEvent.type(screen.getByLabelText('Vote margin'), '2');
  await userEvent.type(screen.getByLabelText('Check command (optional)'), 'cargo test');
  await userEvent.type(screen.getByLabelText('Escalation backend (optional)'), 'strong');
  await userEvent.type(screen.getByLabelText('Diversity hints'), 'simple\ndifferent');
  await userEvent.click(screen.getByRole('button', { name: 'Start Cascade' }));
  expect(send).toHaveBeenCalledWith({ command: 'start_cascade', payload: { prompt: 'solve this', backend: 'cheap', n: 5, vote_k: 2, check_cmd: 'cargo test', diversity_hints: ['simple', 'different'], escalate_backend: 'strong' } });
});

it('requires Evolve fitness and submits bounded counters', async () => {
  const send = vi.fn(() => Promise.resolve({ status: 'applied' as const, revision: 10 }));
  render(<OperationWorkspace activeOperation={null} backends={['worker']} kind="evolve" operation={null} selectedBackend="worker" send={send} />);
  await userEvent.type(screen.getByLabelText('Prompt'), 'improve this');
  await userEvent.click(screen.getByRole('button', { name: 'Start Evolve' }));
  expect(screen.getByRole('alert')).toHaveTextContent('Fitness command is required');
  await userEvent.type(screen.getByLabelText('Fitness command'), 'score');
  await userEvent.clear(screen.getByLabelText('Migration interval'));
  await userEvent.type(screen.getByLabelText('Migration interval'), '0');
  await userEvent.click(screen.getByRole('button', { name: 'Start Evolve' }));
  expect(send).toHaveBeenCalledWith({ command: 'start_evolve', payload: { prompt: 'improve this', backend: 'worker', generations: 10, population: 6, fitness_cmd: 'score', feature_cmd: null, islands: 1, migration_interval: 0, mutation_hints: [] } });
});

it('submits every operational command and presents progress, terminal outcomes, and narrow conflict blocking', async () => {
  // covers: deepseek-custom/web-application :: Existing operational workspaces remain available :: User runs an operational workflow
  const send = vi.fn<(command: AppCommand) => Promise<{ status: 'applied'; revision: number }>>();
  send.mockResolvedValue({ status: 'applied', revision: 11 });
  const cases = [
    { kind: 'autopilot' as const, title: 'Autopilot', fill: async () => { await userEvent.type(screen.getByLabelText('Task'), 'repeat task'); }, command: 'start_autopilot', backends: [] },
    { kind: 'cascade' as const, title: 'Cascade', fill: async () => { await userEvent.type(screen.getByLabelText('Prompt'), 'search task'); }, command: 'start_cascade', backends: ['worker'] },
    { kind: 'evolve' as const, title: 'Evolve', fill: async () => { await userEvent.type(screen.getByLabelText('Prompt'), 'evolve task'); await userEvent.type(screen.getByLabelText('Fitness command'), 'score'); }, command: 'start_evolve', backends: ['worker'] },
    { kind: 'procedure' as const, title: 'Procedure', fill: async () => { await userEvent.type(screen.getByLabelText('Change ID'), 'web-change'); await userEvent.type(screen.getByLabelText('Task ID'), '5.4'); }, command: 'run_procedure', backends: [] },
  ];

  for (const item of cases) {
    const view = render(<OperationWorkspace activeOperation={null} backends={item.backends} kind={item.kind} operation={null} selectedBackend={item.backends[0] ?? null} send={send} />);
    await item.fill();
    await userEvent.click(screen.getByRole('button', { name: `Start ${item.title}` }));
    expect(send).toHaveBeenLastCalledWith(expect.objectContaining({ command: item.command }));
    view.unmount();
  }

  const running = { kind: 'procedure' as const, operation_id: 'run-live', phase: 'running' as const, progress: { completed: 3, total: 5 }, message: 'route: local\npatch: ready\ndiff: +change', error: null };
  const view = render(<OperationWorkspace activeOperation={running} kind="procedure" operation={running} send={send} />);
  expect(screen.getByRole('log')).toHaveTextContent('route: local');
  expect(screen.getByText(/3 of 5/)).toBeVisible();
  expect(screen.getByRole('button', { name: 'Stop operation' })).toBeEnabled();
  view.rerender(<OperationWorkspace activeOperation={null} kind="procedure" operation={{ ...running, phase: 'failed', error: { code: 'service_failed', message: 'verification failed', field: null, recoverable: true } }} send={send} />);
  expect(screen.getByRole('region', { name: 'Result summary' })).toHaveTextContent('route: local');
  expect(screen.getByRole('alert')).toHaveTextContent('verification failed');
  view.rerender(<OperationWorkspace activeOperation={running} kind="autopilot" operation={null} send={send} />);
  expect(screen.getByRole('button', { name: 'Start Autopilot' })).toBeDisabled();
  expect(screen.getByText(/Procedure is active/)).toBeVisible();
});

it('shows complete Procedure evidence and binds each review decision to the displayed run', async () => {
  // covers: deepseek-custom/web-application :: Existing operational workspaces remain available :: Procedure waits for review
  const send = vi.fn<(command: AppCommand) => Promise<{ status: 'applied'; revision: number }>>();
  send.mockResolvedValue({ status: 'applied', revision: 12 });
  const awaiting = { kind: 'procedure' as const, operation_id: 'run-current', phase: 'awaiting_review' as const, progress: { completed: 4, total: 4 }, message: 'route: frontier\ntarget: src/main.rs\nevidence: symbol matched\ndiff: @@ -1 +1 @@\nreport: .deepseek/procedure-runs/run-current.json', error: null };
  const view = render(<OperationWorkspace activeOperation={awaiting} kind="procedure" operation={awaiting} send={send} />);
  expect(screen.getByLabelText('Complete review evidence')).toHaveTextContent('route: frontier');
  expect(screen.getByLabelText('Complete review evidence')).toHaveTextContent('diff: @@ -1 +1 @@');
  expect(screen.queryByRole('button', { name: 'Stop operation' })).not.toBeInTheDocument();
  await userEvent.click(screen.getByRole('button', { name: 'Approve this run' }));
  expect(send).toHaveBeenLastCalledWith({ command: 'review_procedure', payload: { run_id: 'run-current', decision: 'approve' } });

  view.rerender(<OperationWorkspace activeOperation={{ ...awaiting, operation_id: 'run-new' }} kind="procedure" operation={{ ...awaiting, operation_id: 'run-new' }} send={send} />);
  await userEvent.click(screen.getByRole('button', { name: 'Reject this run' }));
  expect(send).toHaveBeenLastCalledWith({ command: 'review_procedure', payload: { run_id: 'run-new', decision: 'reject' } });

  view.rerender(<OperationWorkspace activeOperation={null} kind="procedure" operation={{ ...awaiting, phase: 'completed' }} send={send} />);
  expect(screen.queryByRole('region', { name: 'Procedure review' })).not.toBeInTheDocument();
});

it('selects and validates every maintained Procedure run mode', async () => {
  const send = vi.fn<(command: AppCommand) => Promise<{ status: 'applied'; revision: number }>>();
  send.mockResolvedValue({ status: 'applied', revision: 13 });
  const common = { activeOperation: null, kind: 'procedure' as const, operation: null, send, backends: ['local'], selectedBackend: 'local', selectedModel: 'model' };

  let view = render(<OperationWorkspace {...common} />);
  await userEvent.selectOptions(screen.getByLabelText('Run mode'), 'preview');
  await userEvent.click(screen.getByRole('button', { name: 'Start Procedure' }));
  expect(screen.getByRole('alert')).toHaveTextContent('All fields');
  await userEvent.type(screen.getByLabelText('Change ID'), 'change');
  await userEvent.type(screen.getByLabelText('Task ID'), '1.1');
  await userEvent.type(screen.getByLabelText('Localization run ID'), '11111111-1111-1111-1111-111111111111');
  await userEvent.click(screen.getByRole('button', { name: 'Start Procedure' }));
  const preview = send.mock.calls.at(-1)?.[0];
  expect(preview?.command).toBe('preview_procedure');
  if (preview?.command !== 'preview_procedure') throw new Error('expected preview command');
  expect(preview.payload).toMatchObject({ route: 'automatic', local_backend: 'local', local_model: 'model' });
  view.unmount();

  view = render(<OperationWorkspace {...common} />);
  await userEvent.selectOptions(screen.getByLabelText('Run mode'), 'whole_change');
  await userEvent.type(screen.getByLabelText('Change ID'), 'change');
  await userEvent.click(screen.getByRole('button', { name: 'Start Procedure' }));
  expect(send).toHaveBeenLastCalledWith(expect.objectContaining({ command: 'run_whole_change_procedure' }));
  view.unmount();

  render(<OperationWorkspace {...common} />);
  await userEvent.selectOptions(screen.getByLabelText('Run mode'), 'apply');
  await userEvent.type(screen.getByLabelText('Change ID'), 'change');
  await userEvent.type(screen.getByLabelText('Task ID'), '1.1');
  await userEvent.type(screen.getByLabelText('Localization run ID'), '11111111-1111-1111-1111-111111111111');
  await userEvent.type(screen.getByLabelText('Preview ID'), '22222222-2222-2222-2222-222222222222');
  await userEvent.click(screen.getByRole('button', { name: 'Start Procedure' }));
  expect(send).toHaveBeenLastCalledWith(expect.objectContaining({ command: 'apply_procedure' }));
});
