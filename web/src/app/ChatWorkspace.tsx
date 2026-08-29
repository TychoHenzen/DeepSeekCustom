import { useEffect, useRef, useState, type ClipboardEvent, type DragEvent, type FormEvent } from 'react';
import type { UploadedAttachment } from '../client/client.ts';
import type { AppCommandResult, OperationState, TranscriptBlock } from '../client/contracts.ts';
import { Transcript } from './Transcript.tsx';

export interface ChatWorkspaceProps {
  transcript: TranscriptBlock[];
  operation: OperationState | null;
  acceptedAttachmentId?: string | null;
  voiceOperation?: OperationState | null;
  voiceSettings?: { enabled: boolean; stt_enabled: boolean };
  uploadAttachment?: (file: File) => Promise<UploadedAttachment>;
  clearAttachment?: (id: string) => Promise<void>;
  startVoice?: () => Promise<AppCommandResult>;
  stopVoice?: () => Promise<AppCommandResult>;
  send(this: void, command: { command: 'send_message'; payload: { text: string; attachment_id: string | null } }): Promise<AppCommandResult>;
  stop(this: void): Promise<AppCommandResult>;
}

export function ChatWorkspace({ transcript, operation, acceptedAttachmentId = null, voiceOperation = null, voiceSettings, uploadAttachment, clearAttachment, startVoice, stopVoice, send, stop }: ChatWorkspaceProps) {
  const [text, setText] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [attachment, setAttachment] = useState<UploadedAttachment | null>(acceptedAttachmentId === null ? null : { attachment_id: acceptedAttachmentId, media_type: 'image/unknown', size: 0 });
  const [preview, setPreview] = useState<string | null>(null);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const running = operation?.phase === 'running';

  useEffect(() => () => { if (preview !== null) URL.revokeObjectURL(preview); }, [preview]);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const message = text.trim();
    const attachmentId = attachment?.attachment_id ?? acceptedAttachmentId;
    if (submitting || running || (message.length === 0 && attachmentId === null)) return;
    setSubmitting(true);
    try {
      const result = await send({ command: 'send_message', payload: { text: message, attachment_id: attachmentId } });
      if (result.status === 'applied') { setText(''); setAttachment(null); setPreview(null); }
    } finally {
      setSubmitting(false);
    }
  }

  async function acceptFile(file: File | undefined) {
    if (!file || !uploadAttachment) return;
    setAttachmentError(null);
    try {
      const uploaded = await uploadAttachment(file);
      if (attachment && clearAttachment) await clearAttachment(attachment.attachment_id);
      setAttachment(uploaded);
      setPreview(URL.createObjectURL(file));
    } catch (error) { setAttachmentError(error instanceof Error ? error.message : String(error)); }
  }

  async function removeAttachment() {
    if (attachment && clearAttachment) await clearAttachment(attachment.attachment_id);
    setAttachment(null); setPreview(null);
  }

  useEffect(() => {
    if (!startVoice || !stopVoice || !voiceSettings?.enabled || !voiceSettings.stt_enabled) return;
    const down = (event: KeyboardEvent) => {
      if (event.code !== 'Space' || event.repeat || !document.hasFocus() || event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement) return;
      event.preventDefault(); void startVoice();
    };
    const up = (event: KeyboardEvent) => {
      if (event.code !== 'Space' || !document.hasFocus() || event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement) return;
      event.preventDefault(); void stopVoice();
    };
    window.addEventListener('keydown', down); window.addEventListener('keyup', up);
    return () => { window.removeEventListener('keydown', down); window.removeEventListener('keyup', up); };
  }, [startVoice, stopVoice, voiceSettings?.enabled, voiceSettings?.stt_enabled]);

  return <section aria-labelledby="chat-title" className="chat-workspace">
    <h3 id="chat-title">Conversation</h3>
    <Transcript blocks={transcript} />
    {operation && <p aria-live="polite" className={`turn-state turn-state-${operation.phase}`} role="status">Turn {operation.phase}. {operation.message}</p>}
    {running && <button onClick={() => void stop()} type="button">Stop</button>}
    <form aria-label="Send a chat turn" onDrop={(event: DragEvent) => { event.preventDefault(); void acceptFile(event.dataTransfer.files[0]); }} onDragOver={(event) => event.preventDefault()} onPaste={(event: ClipboardEvent) => void acceptFile(Array.from(event.clipboardData.files).find((file) => file.type.startsWith('image/')))} onSubmit={(event) => void submit(event)}>
      <label htmlFor="chat-message">Message</label>
      <textarea id="chat-message" onChange={(event) => setText(event.target.value)} value={text} />
      <input accept="image/png,image/jpeg,image/bmp" aria-label="Select image" hidden onChange={(event) => void acceptFile(event.target.files?.[0])} ref={fileInput} type="file" />
      <button onClick={() => fileInput.current?.click()} type="button">Attach image</button>
      {(attachment !== null || acceptedAttachmentId !== null) && <div className="attachment-preview"><p>Accepted image ready. PNG, JPEG, or BMP. Maximum 5 MiB.</p>{preview && <img alt="Pending attachment preview" src={preview} />}<button onClick={() => void removeAttachment()} type="button">Clear image</button></div>}
      {attachmentError && <p role="alert">Image rejected: {attachmentError}</p>}
      <button disabled={submitting || running || (text.trim().length === 0 && attachment === null && acceptedAttachmentId === null)} type="submit">Send message</button>
    </form>
    {voiceSettings && <section aria-label="Voice controls" className="voice-controls">
      <p aria-live="polite">{voiceSettings.enabled && voiceSettings.stt_enabled ? (voiceOperation?.message ?? 'Voice ready') : 'Voice unavailable. Enable voice and speech to text in Settings.'}</p>
      <button disabled={!voiceSettings?.enabled || !voiceSettings.stt_enabled} onPointerDown={() => void startVoice?.()} onPointerUp={() => void stopVoice?.()} type="button">Hold to talk</button>
      <p>With this page focused, hold Space outside a text field to talk.</p>
    </section>}
  </section>;
}
