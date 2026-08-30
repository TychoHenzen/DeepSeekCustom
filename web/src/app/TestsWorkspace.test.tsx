import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';

import type { TestControlSnapshot } from '../client/contracts.ts';
import { TestsWorkspace } from './TestsWorkspace.tsx';

afterEach(cleanup);

function state(): TestControlSnapshot {
  const identity = { name: 'application_actor::updates', scope: { type: 'exact' as const, module: 'application_actor', test: 'application_actor::updates' } };
  return {
    discovery: { catalogue_stale: false, failure: null, catalogue: { discovered_at_ms: 44, full_workspace: { name: 'Full workspace', scope: { type: 'full_workspace' } }, modules: [{ name: 'application_actor', tests: [identity] }] } },
    active: null,
    latest_result: null,
    retained_result_warnings: [],
    retained_results: [{ run_id: 'run-20', identity, command: ['cargo', 'test', 'application_actor::updates'], working_dir: 'fixed-project-root', started_at_ms: 1_000, duration_ms: 42, outcome: 'passed', counts: { passed: 1, failed: 0, ignored: 0, filtered: 8 }, exit_code: 0, failed_tests: [], output: 'test result: ok', omitted_output_bytes: 0 }],
  };
}

// covers: deepseek-custom/test-suite-control :: Test results do not imply repository readiness :: Selected tests pass
it('labels focused success only as the selected exact-test outcome', () => {
  render(<TestsWorkspace send={vi.fn()} state={state()} />);
  expect(screen.getByText('Exact test application_actor::updates passed')).toBeVisible();
  expect(screen.queryByText(/repository.*(ready|complete)/i)).not.toBeInTheDocument();
  expect(screen.queryByText(/checkpoint.*complete/i)).not.toBeInTheDocument();
});

it('filters catalogue and sends revisioned server-owned exact run and refresh commands', async () => {
  const send = vi.fn().mockResolvedValue({ status: 'applied', revision: 45 });
  render(<TestsWorkspace send={send} state={state()} />);
  await userEvent.type(screen.getByRole('searchbox', { name: 'Filter modules and tests' }), 'updates');
  await userEvent.click(screen.getByRole('button', { name: 'Run exact test' }));
  expect(send).toHaveBeenCalledWith({ command: 'start_test_run', payload: { request: { identity: { name: 'application_actor::updates', scope: { type: 'exact', module: 'application_actor', test: 'application_actor::updates' } }, catalogue_revision: 44 } } });
  await userEvent.click(screen.getByRole('button', { name: 'Refresh catalogue' }));
  expect(send).toHaveBeenLastCalledWith({ command: 'refresh_tests' });
});

it('shows active progress cancellation truncation and reconnect-restored output', async () => {
  const current = state();
  current.active = { run_id: 'active-1', identity: current.discovery.catalogue!.modules[0]!.tests[0]!, command: ['cargo', 'test'], working_dir: 'fixed-project-root', started_at_ms: 10, elapsed_ms: 900, running: true, counts: { passed: 2, failed: 1, ignored: 0, filtered: 0 }, output: 'HEAD\n...[200 bytes omitted by 4 MiB output limit]...\nTAIL', omitted_output_bytes: 200 };
  const send = vi.fn().mockResolvedValue({ status: 'applied', revision: 46 });
  render(<TestsWorkspace send={send} state={current} />);
  expect(screen.getByText(/Running for 900 ms/)).toBeVisible();
  expect(screen.getByText(/200 bytes are omitted/)).toBeVisible();
  expect(screen.getByText(/HEAD/)).toHaveTextContent('TAIL');
  await userEvent.click(screen.getByRole('button', { name: 'Cancel test run' }));
  expect(send).toHaveBeenCalledWith({ command: 'cancel_test_run' });
});
