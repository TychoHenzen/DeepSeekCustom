import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';

import { TestsWorkspace } from './TestsWorkspace.tsx';

afterEach(cleanup);

it('renders retained identity outcome counts duration command and diagnostic output', async () => {
  render(<TestsWorkspace history={{
    retained_result_warnings: [],
    retained_results: [{
      run_id: 'run-20',
      identity: { name: 'application_actor::updates', scope: { type: 'exact' } },
      command: ['cargo', 'test', 'application_actor::updates'],
      working_dir: 'fixed-project-root',
      started_at_ms: 1_000,
      duration_ms: 42,
      outcome: 'failed',
      counts: { passed: 3, failed: 1, ignored: 2, filtered: 8 },
      exit_code: 101,
      failed_tests: ['application_actor::updates'],
      output: 'assertion failed: expected revision 4',
      omitted_output_bytes: 0,
    }],
  }} />);
  expect(screen.getByRole('heading', { name: 'application_actor::updates' })).toBeVisible();
  expect(screen.getByText('failed')).toBeVisible();
  expect(screen.getByText('42 ms')).toBeVisible();
  expect(screen.getByText('3')).toBeVisible();
  await userEvent.click(screen.getByText('Inspect command and diagnostic output'));
  expect(screen.getByText('cargo test application_actor::updates')).toBeVisible();
  expect(screen.getByText('assertion failed: expected revision 4')).toBeVisible();
});
