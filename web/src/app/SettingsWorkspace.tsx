import { useState, type FormEvent } from 'react';

import type { AppCommand, VisibleSettings } from '../client/contracts.ts';

export function SettingsWorkspace({ settings, send }: {
  settings: VisibleSettings;
  send: (command: AppCommand) => Promise<unknown>;
}) {
  const [draft, setDraft] = useState(settings);
  const backend = draft.backends.find((item) => item.name === draft.selected_backend);

  function submit(event: FormEvent) {
    event.preventDefault();
    void send({ command: 'update_settings', payload: { settings: draft } });
  }

  return <section className="settings-workspace" aria-labelledby="settings-title">
    <h3 id="settings-title">Runtime and saved settings</h3>
    <form onSubmit={submit}>
      <label>Backend<select value={draft.selected_backend ?? ''} onChange={(event) => {
        const selected = draft.backends.find((item) => item.name === event.target.value);
        setDraft({ ...draft, selected_backend: event.target.value || null, selected_model: selected?.configured_model ?? null });
      }}><option value="">Select backend</option>{draft.backends.map((item) => <option key={item.name}>{item.name}</option>)}</select></label>
      <label>Model<select value={draft.selected_model ?? ''} onChange={(event) => setDraft({ ...draft, selected_model: event.target.value || null })}>
        <option value="">Select model</option>{(backend?.models ?? []).map((model) => <option key={model}>{model}</option>)}</select></label>
      <label>Effort<select value={draft.effort} onChange={(event) => setDraft({ ...draft, effort: event.target.value })}>{['none','low','medium','high','max'].map((value) => <option key={value}>{value}</option>)}</select></label>
      <label>Context budget<input min={32000} max={200000} type="number" value={draft.context_budget} onChange={(event) => setDraft({ ...draft, context_budget: event.target.valueAsNumber })}/></label>
      <label>Maximum output tokens<input min={1024} max={65536} type="number" value={draft.max_tokens} onChange={(event) => setDraft({ ...draft, max_tokens: event.target.valueAsNumber })}/></label>
      <label><input type="checkbox" checked={draft.show_raw_output} onChange={(event) => setDraft({ ...draft, show_raw_output: event.target.checked })}/> Show raw output</label>
      <fieldset><legend>Style</legend>
        <label><input type="checkbox" checked={draft.style.plain_language} onChange={(event) => setDraft({ ...draft, style: { ...draft.style, plain_language: event.target.checked } })}/> Plain language</label>
        <label>Target grade<input min={1} max={20} step="0.5" type="number" value={draft.style.target_grade} onChange={(event) => setDraft({ ...draft, style: { ...draft.style, target_grade: event.target.valueAsNumber } })}/></label>
      </fieldset>
      <fieldset><legend>Voice</legend>
        {(['enabled','stt_enabled','tts_enabled'] as const).map((field) => <label key={field}><input type="checkbox" checked={draft.voice[field]} onChange={(event) => setDraft({ ...draft, voice: { ...draft.voice, [field]: event.target.checked } })}/>{field.replaceAll('_',' ')}</label>)}
        <label>Trigger mode<select value={draft.voice.trigger_mode} onChange={(event) => setDraft({ ...draft, voice: { ...draft.voice, trigger_mode: event.target.value } })}><option value="push_to_talk">push to talk</option><option value="wake_word">wake word</option></select></label>
        <label>Wake phrase<input value={draft.voice.wake_phrase} onChange={(event) => setDraft({ ...draft, voice: { ...draft.voice, wake_phrase: event.target.value } })}/></label>
        <label>TTS voice<input value={draft.voice.tts_voice} onChange={(event) => setDraft({ ...draft, voice: { ...draft.voice, tts_voice: event.target.value } })}/></label>
        <label>TTS speed<input min={0.5} max={2} step="0.1" type="number" value={draft.voice.tts_speed} onChange={(event) => setDraft({ ...draft, voice: { ...draft.voice, tts_speed: event.target.valueAsNumber } })}/></label>
      </fieldset>
      <fieldset><legend>Procedure</legend>
        {(['localization_backend','local_patch_backend','frontier_patch_backend'] as const).map((field) => <label key={field}>{field.replaceAll('_',' ')}<select value={draft.procedure[field] ?? ''} onChange={(event) => setDraft({ ...draft, procedure: { ...draft.procedure, [field]: event.target.value || null } })}><option value="">Not selected</option>{draft.backends.map((item) => <option key={item.name}>{item.name}</option>)}</select></label>)}
        <label>Index max files<input min={1} type="number" value={draft.procedure.index_max_files} onChange={(event) => setDraft({ ...draft, procedure: { ...draft.procedure, index_max_files: event.target.valueAsNumber } })}/></label>
        <label>Index max bytes<input min={1} type="number" value={draft.procedure.index_max_total_bytes} onChange={(event) => setDraft({ ...draft, procedure: { ...draft.procedure, index_max_total_bytes: event.target.valueAsNumber } })}/></label>
        <label>Verifier commands<textarea value={draft.procedure.verifier_commands.join('\n')} onChange={(event) => setDraft({ ...draft, procedure: { ...draft.procedure, verifier_commands: event.target.value.split('\n').filter(Boolean) } })}/></label>
      </fieldset>
      <div><span>Working directory: {draft.working_dir ?? 'Project root'}</span> <button type="button" onClick={() => void send({ command: 'pick_working_directory' })}>Choose folder</button></div>
      <button type="submit">Save settings</button>
    </form>
  </section>;
}
