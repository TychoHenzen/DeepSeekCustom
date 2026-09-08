import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { OperationWorkspace } from './OperationWorkspace.tsx';
import { procedureVisualFixtures } from './ProcedureVisualFixtures.ts';

afterEach(cleanup);

describe.each([
  { name: 'desktop', width: 1440 },
  { name: 'narrow', width: 360 },
])('maintained Procedure reports at $name viewport', ({ width }) => {
  it.each(procedureVisualFixtures)('renders $name with bounded semantic evidence', ({ report }) => {
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: width });
    render(<OperationWorkspace activeOperation={report.phase === 'running' || report.phase === 'awaiting_review' ? report : null} kind="procedure" operation={report} send={vi.fn()} />);

    expect(screen.getByRole('heading', { name: 'Procedure operation' })).toBeVisible();
    expect(screen.getByRole('region', { name: 'Procedure evidence' })).toBeVisible();
    expect(screen.getByText(report.operation_id ?? '')).toBeVisible();
    expect(document.querySelector('.operation-log')).toHaveClass('operation-log');
    expect(document.querySelector('.procedure-evidence')).toHaveClass('procedure-evidence');
    if (report.phase === 'awaiting_review') {
      expect(screen.getByRole('region', { name: 'Procedure review' })).toBeVisible();
      expect(screen.getByRole('button', { name: 'Approve this run' })).toBeEnabled();
    }
    if (['completed', 'failed', 'interrupted'].includes(report.phase)) {
      expect(screen.getByRole('region', { name: 'Result summary' })).toBeVisible();
    }
  });
});
