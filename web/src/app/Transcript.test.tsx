import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Transcript } from './Transcript.tsx';
import type { TranscriptBlock } from '../client/contracts.ts';

afterEach(cleanup);

describe('transcript', () => {
  it('renders every semantic block type and explicit tool and terminal states', () => {
    const blocks: TranscriptBlock[] = [
      { id: 1, type: 'user', text: 'hello', has_image: true },
      { id: 2, type: 'assistant', spans: [{ type: 'reasoning', text: 'think' }, { type: 'text', text: 'answer' }] },
      { id: 3, type: 'tool_call', tool: 'read', args: '{}', output: null, is_error: false },
      { id: 4, type: 'notice', level: 'warning', message: 'careful' },
      { id: 5, type: 'error', message: 'failed', recoverable: true },
      { id: 6, type: 'image', media_type: 'image/png', data: 'AA==' },
      { id: 7, type: 'terminal', outcome: 'completed', message: 'done' },
    ];
    render(<Transcript blocks={blocks} />);
    expect(screen.getByRole('log')).toHaveTextContent('You');
    expect(screen.getByText('Reasoning')).toBeVisible();
    expect(screen.getByText('Running')).toBeVisible();
    expect(screen.getByRole('status')).toHaveTextContent('careful');
    expect(screen.getByRole('alert')).toHaveTextContent('failed');
    expect(screen.getByRole('img')).toHaveAttribute('src', 'data:image/png;base64,AA==');
    expect(screen.getByText('completed')).toBeVisible();
  });

  it('bounds long content and expands it on request', async () => {
    render(<Transcript blocks={[{ id: 1, type: 'assistant', spans: [{ type: 'text', text: 'x'.repeat(2_100) }] }]} />);
    expect(screen.getByText(/…$/)).toBeVisible();
    await userEvent.click(screen.getByRole('button', { name: 'Expand' }));
    expect(screen.getByText('x'.repeat(2_100))).toBeVisible();
  });

  it('follows new output only while the reader is near the end', () => {
    const scrollTo = vi.fn();
    const { rerender } = render(<Transcript blocks={[]} />);
    const log = screen.getByRole('log');
    Object.defineProperties(log, { scrollHeight: { configurable: true, value: 1_000 }, clientHeight: { configurable: true, value: 200 }, scrollTop: { configurable: true, writable: true, value: 800 } });
    Object.defineProperty(log, 'scrollTo', { configurable: true, value: scrollTo });
    fireEvent.scroll(log);
    rerender(<Transcript blocks={[{ id: 1, type: 'notice', level: 'info', message: 'new' }]} />);
    expect(scrollTo).toHaveBeenCalled();
    scrollTo.mockClear();
    log.scrollTop = 100;
    fireEvent.scroll(log);
    rerender(<Transcript blocks={[{ id: 1, type: 'notice', level: 'info', message: 'new' }, { id: 2, type: 'notice', level: 'info', message: 'later' }]} />);
    expect(scrollTo).not.toHaveBeenCalled();
  });
});
