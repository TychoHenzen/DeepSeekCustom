import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';

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
