import '@testing-library/jest-dom/vitest';

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { AppErrorBoundary } from './AppErrorBoundary.tsx';

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function BrokenView(): never {
  throw new Error('render failed');
}

describe('AppErrorBoundary', () => {
  it('replaces a React render failure with a labelled fatal fallback', () => {
    vi.spyOn(console, 'error').mockImplementation(() => undefined);

    render(
      <AppErrorBoundary>
        <BrokenView />
      </AppErrorBoundary>,
    );

    expect(screen.getByRole('alert')).toHaveAccessibleName('The application cannot continue');
    expect(screen.getByText('Fatal frontend error')).toBeVisible();
    expect(screen.queryByRole('button', { name: /retry/i })).not.toBeInTheDocument();
  });
});
