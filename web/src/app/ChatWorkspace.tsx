import { useState, type FormEvent } from 'react';
import type { AppCommandResult, OperationState, TranscriptBlock } from '../client/contracts.ts';
import { Transcript } from './Transcript.tsx';

export interface ChatWorkspaceProps {
  transcript: TranscriptBlock[];
  operation: OperationState | null;
  acceptedAttachmentId?: string | null;
  send(this: void, command: { command: 'send_message'; payload: { text: string; attachment_id: string | null } }): Promise<AppCommandResult>;
  stop(this: void): Promise<AppCommandResult>;
}

export function ChatWorkspace({ transcript, operation, acceptedAttachmentId = null, send, stop }: ChatWorkspaceProps) {
  const [text, setText] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const running = operation?.phase === 'running';

  async function submit(event: FormEvent) {
    event.preventDefault();
    const message = text.trim();
    if (submitting || running || (message.length === 0 && acceptedAttachmentId === null)) return;
    setSubmitting(true);
    try {
      const result = await send({ command: 'send_message', payload: { text: message, attachment_id: acceptedAttachmentId } });
      if (result.status === 'applied') setText('');
    } finally {
      setSubmitting(false);
    }
  }

  return <section aria-labelledby="chat-title" className="chat-workspace">
    <h3 id="chat-title">Conversation</h3>
    <Transcript blocks={transcript} />
    {operation && <p aria-live="polite" className={`turn-state turn-state-${operation.phase}`} role="status">Turn {operation.phase}. {operation.message}</p>}
    {running && <button onClick={() => void stop()} type="button">Stop</button>}
    <form aria-label="Send a chat turn" onSubmit={(event) => void submit(event)}>
      <label htmlFor="chat-message">Message</label>
      <textarea id="chat-message" onChange={(event) => setText(event.target.value)} value={text} />
      {acceptedAttachmentId !== null && <p>Accepted image ready.</p>}
      <button disabled={submitting || running || (text.trim().length === 0 && acceptedAttachmentId === null)} type="submit">Send message</button>
    </form>
  </section>;
}
