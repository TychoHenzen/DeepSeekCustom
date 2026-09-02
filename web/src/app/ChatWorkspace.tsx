import { useEffect, useRef, useState, type ClipboardEvent, type DragEvent, type FormEvent } from 'react';
import type { UploadedAttachment } from '../client/browser/client-types.ts';
import type { AppCommand, AppCommandResult, ControlledDevelopmentState, OperationState, TranscriptBlock } from '../client/contracts.ts';
import { Transcript } from './Transcript.tsx';

interface ChatWorkspaceProps {
  transcript: TranscriptBlock[];
  sessionId: string;
  controlledDevelopment: ControlledDevelopmentState;
  operation: OperationState | null;
  acceptedAttachmentId?: string | null;
  voiceOperation?: OperationState | null;
  voiceSettings?: { enabled: boolean; stt_enabled: boolean };
  uploadAttachment?: (file: File) => Promise<UploadedAttachment>;
  clearAttachment?: (id: string) => Promise<void>;
  startVoice?: () => Promise<AppCommandResult>;
  stopVoice?: () => Promise<AppCommandResult>;
  send(this: void, command: AppCommand): Promise<AppCommandResult>;
  stop(this: void): Promise<AppCommandResult>;
}

export function ChatWorkspace({ transcript, sessionId, controlledDevelopment, operation, acceptedAttachmentId = null, voiceOperation = null, voiceSettings, uploadAttachment, clearAttachment, startVoice, stopVoice, send, stop }: ChatWorkspaceProps) {
  const [text, setText] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [attachment, setAttachment] = useState<UploadedAttachment | null>(acceptedAttachmentId === null ? null : { attachment_id: acceptedAttachmentId, media_type: 'image/unknown', size: 0 });
  const [preview, setPreview] = useState<string | null>(null);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const [controlledError, setControlledError] = useState<string | null>(null);
  const [controlledSubmitting, setControlledSubmitting] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);
  const running = operation?.phase === 'running';

  async function sendControlled(command: AppCommand) {
    if (controlledSubmitting) return;
    setControlledSubmitting(true);
    setControlledError(null);
    try {
      const result = await send(command);
      if (result.status === 'rejected') setControlledError(result.error.message);
    } catch (error) {
      setControlledError(error instanceof Error ? error.message : String(error));
    } finally {
      setControlledSubmitting(false);
    }
  }

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
    <ControlledDevelopmentPanel
      onCommand={(command) => void sendControlled(command)}
      sessionId={sessionId}
      state={controlledDevelopment}
      submitting={controlledSubmitting}
    />
    {controlledError && <p role="alert">Controlled Development error: {controlledError}</p>}
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

function ControlledDevelopmentPanel({ state, sessionId, submitting, onCommand }: {
  state: ControlledDevelopmentState;
  sessionId: string;
  submitting: boolean;
  onCommand: (command: AppCommand) => void;
}) {
  const awaitingApproval = state.phase === 'awaiting_approval' && state.card !== null;
  const active = state.phase === 'planning' || state.phase === 'awaiting_approval' || state.phase === 'executing';
  const busyReason = submitting ? 'Controlled Development controls are unavailable while the current action is being sent.' : null;
  const approveReason = awaitingApproval ? null : 'Approve is unavailable until the current session has a Work Card awaiting approval.';
  const rejectReason = awaitingApproval ? null : 'Reject is unavailable until the current session has a Work Card awaiting approval.';
  const stopReason = active && state.packet_id !== null ? null : 'Stop is unavailable because the current session has no active controlled packet.';
  const card = state.card;

  return <section aria-labelledby="controlled-development-title" className="controlled-development">
    <header>
      <div>
        <h4 id="controlled-development-title">Controlled Development</h4>
        <p aria-live="polite" role="status">Current phase: {phaseLabel(state.phase)}</p>
      </div>
      <label className="controlled-toggle">
        <input
          aria-describedby={busyReason === null ? undefined : 'controlled-busy-reason'}
          checked={state.enabled}
          disabled={submitting}
          onChange={(event) => onCommand({
            command: 'set_controlled_development_enabled',
            payload: { session_id: sessionId, enabled: event.target.checked },
          })}
          role="switch"
          type="checkbox"
        />
        Controlled Development
      </label>
    </header>

    {card === null ? <p>No Work Card is available for this session.</p> : <section aria-labelledby="work-card-title" className="work-card">
      <h5 id="work-card-title">Work Card</h5>
      <dl>
        <CardField label="ID" value={card.id} />
        <CardField label="Outcome" value={card.outcome} />
        <CardList label="Proof commands" values={card.proof_commands} />
        <CardList label="Production paths" values={card.production_paths} />
        <CardList label="Supporting paths" values={card.supporting_paths} />
        <CardList label="Excluded" values={card.excluded} />
        <CardList label="Complexity exceptions" values={card.complexity_exceptions} />
      </dl>
    </section>}

    <div aria-label="Controlled Development actions" className="controlled-actions">
      <button
        aria-describedby={busyReason ? 'controlled-busy-reason' : approveReason === null ? undefined : 'controlled-approve-reason'}
        disabled={submitting || !awaitingApproval}
        onClick={() => card && onCommand({ command: 'approve_controlled_development', payload: { session_id: sessionId, card_id: card.id } })}
        type="button"
      >Approve Work Card</button>
      <button
        aria-describedby={busyReason ? 'controlled-busy-reason' : rejectReason === null ? undefined : 'controlled-reject-reason'}
        disabled={submitting || !awaitingApproval}
        onClick={() => card && onCommand({ command: 'reject_controlled_development', payload: { session_id: sessionId, card_id: card.id } })}
        type="button"
      >Reject Work Card</button>
      <button
        aria-describedby={busyReason ? 'controlled-busy-reason' : stopReason === null ? undefined : 'controlled-stop-reason'}
        disabled={submitting || stopReason !== null}
        onClick={() => state.packet_id && onCommand({ command: 'stop_controlled_development', payload: { session_id: sessionId, packet_id: state.packet_id } })}
        type="button"
      >Stop controlled work</button>
      {state.retained_evidence && <button
        aria-describedby={busyReason === null ? undefined : 'controlled-busy-reason'}
        disabled={submitting}
        onClick={() => onCommand({ command: 'discard_controlled_development_evidence', payload: { session_id: sessionId } })}
        type="button"
      >Discard retained evidence</button>}
    </div>
    {approveReason && <p className="disabled-reason" id="controlled-approve-reason">{approveReason}</p>}
    {rejectReason && <p className="disabled-reason" id="controlled-reject-reason">{rejectReason}</p>}
    {stopReason && <p className="disabled-reason" id="controlled-stop-reason">{stopReason}</p>}
    {busyReason && <p className="disabled-reason" id="controlled-busy-reason">{busyReason}</p>}

    {state.structural_errors.length > 0 && <section aria-label="Work Card errors" role="alert">
      <h5>Work Card errors</h5>
      <ul>{state.structural_errors.map((error) => <li key={`${error.field}:${error.message}`}><strong>{error.field}:</strong> {error.message}</li>)}</ul>
    </section>}
    {state.blocker && <p role="alert">Blocked: {state.blocker}</p>}

    <section aria-labelledby="changed-paths-title">
      <h5 id="changed-paths-title">Changed paths</h5>
      {state.changed_paths.length === 0 ? <p>None recorded.</p> : <ul>{state.changed_paths.map((path) => <li key={path}><code>{path}</code></li>)}</ul>}
    </section>
    <section aria-labelledby="proof-results-title">
      <h5 id="proof-results-title">Proof results</h5>
      {state.proof_results.length === 0 ? <p>No proof results yet.</p> : <ul>{state.proof_results.map((result) => <li key={result.command}><code>{result.command}</code>: {result.disposition}{result.exit_code === null ? '' : `, exit ${result.exit_code}`}</li>)}</ul>}
    </section>
    <section aria-labelledby="controlled-result-title">
      <h5 id="controlled-result-title">Result</h5>
      <p>{state.compact_result ?? 'No compact result is available yet.'}</p>
    </section>
    <p className="controlled-limitation"><strong>Remaining limitation:</strong> {state.limitation}</p>
  </section>;
}

function CardField({ label, value }: { label: string; value: string }) {
  return <div><dt>{label}</dt><dd>{value}</dd></div>;
}

function CardList({ label, values }: { label: string; values: string[] }) {
  return <div><dt>{label}</dt><dd>{values.length === 0 ? 'None.' : <ul>{values.map((value) => <li key={value}><code>{value}</code></li>)}</ul>}</dd></div>;
}

function phaseLabel(phase: ControlledDevelopmentState['phase']): string {
  return phase.split('_').map((word) => word[0]?.toUpperCase() + word.slice(1)).join(' ');
}
