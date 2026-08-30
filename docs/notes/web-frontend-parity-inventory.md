# Web frontend parity inventory

This inventory records the native presentation boundary before application-state
extraction. It is based on `crates/deepseek-custom/src/gui/mod.rs`, its sibling
modules, `crates/deepseek-custom/src/main.rs`, and the external GUI integration
tests. The actor and web milestones use this as the migration map.

## `DeepSeekGui` fields

| Field | Current source and behavior | Destination | Final deletion or retention |
| --- | --- | --- | --- |
| `rx_events` | Receives `RoutedEvent` values from every backend and repeat/search path. `drain_events` consumes it once per native frame. | Actor event ingress. | Delete the GUI receiver after the actor owns the subscription. |
| `tx_input` | Sends `AgentCommand::UserTurn`, `NewSession`, `LoadSession`, and `SwitchBackend`. | Typed agent service port. | Delete the GUI sender. |
| `handles` | `AgentHandles` contains shared interrupt, effort, voice mode, context budget, model, working directory, Cascade counters, and style flags. | Actor-owned runtime settings port backed by the existing `SharedFlags` and factory handles. | Retain domain handles. Delete GUI ownership. |
| `settings` | In-memory `Settings`; controls mutate it and `persist_settings` writes `<project_root>/settings.json`. | Settings service and visible settings DTO. | Retain the domain schema and persistence path. |
| `project_root` | Fixed root for sessions, settings, Procedure, Autopilot policy, and service construction. | Application context and domain service ports. | Retain as an immutable boundary. |
| `transcript` | `Transcript` receives user blocks, routed stream events, notices, images, voice errors, and repeat notices. It is autosaved with session records. | Actor-owned presentation-neutral transcript projection serialized in snapshots and changes. | Retain projection behavior. Delete native rendering dependency. |
| `input_buffer` | Text input draft. Voice transcripts replace it before `send_input`. | Web chat component draft. Submitted content becomes an actor command. | Delete native field. |
| `input_focused` | Native focus state used only by PTT key suppression. | Browser focus and keyboard event handling in the web component. | Delete native field. |
| `attachment` | One pending `ImageAttachment`; Ctrl+V and native file drops populate it, and submission sends it with text. | Attachment service and typed chat command payload. | Retain attachment validation and backend behavior. Delete egui slot UI. |
| `backends` | `BackendPicker` owns names, selected backend/model, fetched model lists, and backend-switch command construction. | Backend selection service port and visible backend DTO. | Retain backend registry and factory contracts. Delete picker rendering state. |
| `voice` | `VoiceUi` owns voice channels, state, settings controls, reply buffer, and PTT policy. | Voice service port plus actor-owned visible voice state. | Retain Rust voice service. Delete native voice panel and key polling. |
| `autopilot` | Form values, repeat channel and interrupt flag, and `AutopilotProgress`; progress is updated by repeat events. | Autopilot service port and actor operation state. | Retain `RepeatCommand` and runner. Delete native tab state and paint. |
| `cascade` | Cascade form values, search channel and shared stop flag, and `SearchProgress`. | Search service port and actor operation state. | Retain Cascade domain runner. Delete native tab state and paint. |
| `evolve` | Evolve form values, search channel and shared stop flag, and `SearchProgress`. | Search service port and actor operation state. | Retain Evolve domain runner. Delete native tab state and paint. |
| `procedure` | Procedure form, active run, progress receiver, review state, reports, model lists, and interrupt flag. | Procedure service port and actor-owned operation/review state. | Retain Procedure runners, reports, and review contracts. Delete native tab state and paint. |
| `sessions` | `SessionState` owns current id/meta, saved list, API messages, Claude session id, and session-store reads/writes. | Session domain service behind actor commands and snapshot DTOs. | Retain session record format and store. Delete GUI wrapper ownership. |
| `active_tab` | Native central-panel selection for Chat, Autopilot, Cascade, Evolve, Procedure, and Sessions. | Actor selected-workspace field and web navigation component. | Delete native enum use after web cutover. |
| `settings_visible` | Tab key toggles the native settings sidebar. It also suppresses PTT. | Web settings route/panel visibility and browser focus policy. | Delete native toggle. |
| `show_raw_output` | Chooses rendered transcript versus raw transcript in the Chat panel. Persisted through `Settings.show_raw_output`. | Visible settings DTO and web transcript component. | Retain setting. Delete native rendering branch. |
| `effort` | Seeds from the shared atomic flag, updates that flag, and persists `Settings.effort`. | Visible settings DTO and typed settings command. | Retain shared runtime effect. |
| `context_budget` | Seeds from the shared atomic flag, updates it, and persists `Settings.context_budget`. | Visible settings DTO and typed settings command. | Retain shared runtime effect. |
| `plain_language` | Seeds from the style shared flag, updates it, and persists `Settings.style.plain_language_enabled`. | Visible settings DTO and typed settings command. | Retain shared runtime effect. |
| `plain_language_grade` | Seeds from the style grade flag, updates it, and persists `Settings.style.target_grade`. | Visible settings DTO and typed settings command. | Retain shared runtime effect. |
| `working_dir_display` | Displays and updates the mutable tool working directory. A confirmed native picker result updates the shared path and settings. | Folder-picker service port plus visible settings DTO. | Retain `working_dir` versus fixed `project_root` boundary. Delete native display state. |
| `token_count` | Shows the latest completed turn's total token count. | Actor operation/transcript statistics. | Retain visible statistic. Delete native status row. |
| `total_cache_hit_tokens` | Accumulates cache-hit tokens across completed main turns. | Actor session/runtime statistics. | Retain visible statistic. |
| `total_cache_miss_tokens` | Accumulates cache-miss tokens across completed main turns. | Actor session/runtime statistics. | Retain visible statistic. |
| `session_status` | Native status text transitions through `Ready`, `Running...`, and `Interrupted`. | Actor operation state and web live status. | Retain semantics. Delete native status row. |
| `turn_active` | Blocks immediate session switches and marks a pending switch until `TurnEnd`, `Interrupted`, or `RepeatFinished`. | Actor command arbitration and session transition state. | Retain deferred-switch behavior. Delete GUI field. |
| `pending_switch` | Stores `PendingSwitch::New` or `PendingSwitch::Load(SessionId)` and applies it after terminal event processing and autosave. | Actor-owned pending session transition. | Retain behavior. Delete GUI enum location after moving it. |
| `follow_output` | Causes the native transcript scroll area to follow newly applied events. | Web transcript view state and SSE client behavior. | Delete native scroll flag. |
| `unsaved_changes` | Marks transcript changes and controls autosave timing. Cleared after `TurnEnd` or timed save. | Actor persistence state and session service. | Retain autosave semantics. Delete GUI field. |
| `saved_at` | `Instant` used with `TIMED_SAVE_INTERVAL` to schedule a native-frame timed save. | Actor clock/persistence service, with a deterministic clock seam in tests. | Delete native frame timer. |
| `md_cache` | `CommonMarkCache` used only by egui markdown rendering. | Web markdown component, if needed. | Delete from Rust production after web rendering is in place. |

`DeepSeekGui` has 34 top-level fields. `AgentHandles` is counted as one field
above, with its ten shared values mapped individually because each has a distinct
runtime consumer.

## Command paths

| Native source | Current command and route | Actor destination |
| --- | --- | --- |
| Chat input and voice transcript | `send_input` sends `AgentCommand::UserTurn { text, image }` on `tx_input`. `main.rs` receives it and calls the current backend. | `AppCommand::SendMessage` through the agent port. The actor first appends the user projection and marks the turn active. |
| New Chat | `start_new_session` calls `SessionState::start_new`, then sends `AgentCommand::NewSession`. | `AppCommand::NewSession` through the session and agent ports. |
| Saved session row | `load_session` defers when `turn_active`; otherwise `SessionState::load` returns `AgentCommand::LoadSession`. | `AppCommand::LoadSession` with actor-owned pending transition. |
| Backend picker | `BackendPicker::switch_backend` returns `BackendSwitch`, saves outgoing state, and sends `AgentCommand::SwitchBackend`. | `AppCommand::SelectBackend` through the backend port. |
| Autopilot start/stop | `AutopilotTab` sends `RepeatCommand { task, iterations }`; Escape calls its repeat interrupt flag. | `AppCommand::StartAutopilot` and `StopOperation` through the repeat port. |
| Cascade start/stop | `CascadeTab` sends `SearchCommand::Cascade(CascadeParams)`; Escape uses the shared search interrupt flag. | `AppCommand::StartCascade` and `StopOperation` through the search port. |
| Evolve start/stop | `EvolveTab` sends `SearchCommand::Evolve(EvolveParams)`; Escape uses the shared search interrupt flag. | `AppCommand::StartEvolve` and `StopOperation` through the search port. |
| Procedure run | `ProcedureTab` sends `ProcedureCommand::Run`, `Sampled`, or `WholeChange`. | Typed Procedure actor port preserving existing request types. |
| Procedure preview/apply | `ProcedureTab` sends `ProcedureCommand::Preview` or `Apply`. | Typed Procedure actor port preserving preview/apply contracts. |
| Procedure review | `ProcedureTab` sends `ProcedureCommand::Review { run_id, decision }` after approval/rejection controls. | Run-scoped `AppCommand::ReviewProcedure` through the Procedure port. |
| Procedure stop | Escape and tab/session transitions call `ProcedureTab::request_stop`, setting its interrupt flag. | `AppCommand::StopOperation` through the Procedure port. |
| Voice PTT | `handle_ptt` maps native Space or Ctrl+Space transitions to `VoiceCommand::StartListening` or `StopListening`. | Typed voice port. Browser key events call the same port while the app has focus. |
| Voice reply | `on_turn_end` calls `VoiceUi::speak_accumulated_reply`, which sends `VoiceCommand::Speak`. Escape sends `StopSpeaking`. | Typed voice port and actor voice state. |
| Voice controls | `VoiceUi::render_section` sends `SetEnabled`, `SetSttEnabled`, `SetTtsEnabled`, `SetTriggerMode`, `SetWakePhrase`, `SetVoice`, and `SetSpeed`. | Typed settings and voice ports. |
| Settings controls | Settings panel mutates `Settings`, shared atomics, and the working directory, then `persist_settings` saves on close. | Validated typed settings commands with one persistence service. |
| Working-directory picker | `FolderPickerRequest::pick_folder` opens the native `rfd` dialog and `apply_working_dir_selection` updates only `working_dir`. | Typed folder-picker port using `spawn_blocking` in the web adapter milestone. |
| Model list refresh | Backend and Procedure pickers spawn background model fetches and drain `(backend, models)` channels. | Backend/Procedure service events translated to actor changes. |
| Native global keys | Tab toggles settings, Ctrl+Q closes the window, and Escape sets interrupt flags, emits a notice, and stops speaking. | Browser navigation, process shutdown, and typed stop commands. |

## Event paths

| Event | Current native path | Actor projection destination |
| --- | --- | --- |
| `Text` | Main empty-route event calls `voice.push_reply_text`, then `Transcript::apply_routed_event`. | Append assistant text and update voice reply buffer. |
| `Reasoning` | `Transcript::apply_routed_event` creates or extends reasoning spans. | Append reasoning block/span. |
| `ToolCallStart` | Transcript creates an in-progress tool block. Main path logs the call. | Append running tool block. |
| `ToolCallEnd` | Transcript fills the matching tool block and records error state. Main path logs success/failure. | Complete tool block with bounded visible output. |
| `Info` | Transcript appends a notice/info block. | Append informational change. |
| `Error` | Transcript appends an error block. It is not terminal because attachment/backend errors can be mid-turn. | Append error change without clearing active operation. |
| `Interrupted` | Sets `session_status` to `Interrupted`, clears voice reply, applies transcript notice, and ends a turn. | Terminal interrupted operation change and session arbitration. |
| `TurnEnd` | Updates token/cache counters, speaks the reply, autosaves, clears dirty state, sets `Ready`, and ends a non-Autopilot turn. | Terminal turn change, statistics, voice command, and session persistence. |
| `ConversationSnapshot` | Updates `SessionState` API messages and Claude session id before autosave. | Actor session snapshot state, not browser-visible secrets. |
| `SessionReset` | Saves outgoing conversation, starts a new one, clears cache counters and voice reply. | Session transition change and reset statistics. |
| `RepeatIterationStart` | Saves outgoing session, starts a fresh iteration session, updates Autopilot progress, and keeps `turn_active`. | Autopilot operation and session transition changes. |
| `RepeatFinished` | Updates Autopilot terminal progress and releases deferred session switching. | Autopilot terminal operation change. |
| `SearchProgress` | Routes by `SearchKind` to Cascade or Evolve `SearchProgress::Running`. | Search operation snapshot change. |
| `SearchFinished` | Routes by `SearchKind` to the matching tab's finished state. | Search terminal operation change. |
| `RoutedEvent` route metadata | Empty routes receive main-session side effects. Non-empty routes skip them and update nested subagent transcript blocks only. | Preserve route-aware transcript projection and side-effect exclusion. |
| `VoiceEvent::StateChanged` | `VoiceUi::handle_event` changes voice state. | Visible voice operation state. |
| `VoiceEvent::Transcript` | `VoiceUi::handle_event` returns text, which replaces the input buffer and submits a turn. | Voice transcript command path. |
| `VoiceEvent::WakeDetected` | No visible state change in the native UI. | Preserve as a voice event if the web DTO needs it, otherwise record as non-visible. |
| `VoiceEvent::Error` | Adds a warning notice and leaves input submission untouched. | Voice error change and visible notice. |
| Procedure progress | `ProcedureTab::drain_progress` consumes `ProcedureProgress` and updates run, preview, apply, review, and terminal state. It does not enter the chat transcript. | Procedure operation state and review evidence. |
| model-list results | Backend and Procedure tabs consume fetched model lists and drop stale backend results. | Visible backend/model option changes. |

The current event serialization point is `DeepSeekGui::dispatch_event`. The
actor must preserve its ordering: apply main-event side effects, project the
routed event, mark dirty/follow-output, then apply a deferred session switch
only after the terminal event and autosave.

## Persisted values and boundaries

The native controls persist through `Settings::save(project_root)` to the
existing `settings.json` schema. The web DTO must expose only the visible values
below, never `api_key`, backend credential maps, environment values, or raw
credential sources.

| Existing persisted area | Values used by the native GUI | Destination |
| --- | --- | --- |
| Shared runtime | `effort`, `context_budget`, `show_raw_output`, and `working_dir`. | Visible settings DTO plus typed settings service. |
| Style | `style.plain_language_enabled` and `style.target_grade`. | Visible style settings and shared style flags. |
| Backend selection | `default_backend`, each backend model, and backend-specific configuration. | Visible backend name/model options. Never serialize secret backend fields. |
| Voice | `voice.enabled`, `stt_enabled`, `tts_enabled`, `trigger_mode`, `wake_phrase`, `tts_voice`, and `tts_speed`. Model paths are local configuration and must not be exposed as secrets or raw paths unless a later contract requires a redacted status. | Visible voice controls and typed voice settings. |
| Autopilot | `autopilot.task`, `autopilot.iterations`, `autopilot.policy_path`, and answerer model configuration. | Autopilot form and service configuration. |
| Cascade | Prompt, backend, `n`, `vote_k`, check command, diversity hints, and escalation backend. | Cascade form and search service. |
| Evolve | Prompt, backend, generations, population, fitness/feature commands, islands, migration interval, and mutation hints. | Evolve form and search service. |
| Procedure | Selected localizer, local patch and frontier backends, repository-index limits, verifier commands, repair/sample caps, and warning thresholds. | Procedure form and typed service port. |
| Sessions | `SessionRecord` files under the session store rooted at `project_root`, including transcript, API messages, Claude session id, title, metadata, and current backend/model. | Session service and actor snapshot. |
| Autopilot log | Completed repeat steps under `<project_root>/.autopilot/decisions.log`. | Existing Autopilot domain service. |
| Procedure reports | Procedure report files under the existing report store. | Existing Procedure domain service. |

`project_root` remains fixed for settings, sessions, Autopilot, Procedure, and
reports. `working_dir` remains mutable only for agent filesystem tools. A folder
selection must never change `project_root`.

## Native interactions

| Interaction | Current implementation | Migration destination or deletion |
| --- | --- | --- |
| `eframe::run_native` window | `main.rs` constructs `DeepSeekGui` and starts a blocking native window. | Keep operational through task 1.7. Delete only at final web cutover. |
| egui repaint loop | `DeepSeekGui::update` drains Procedure/events/voice/model lists, saves, handles keys, renders, and requests repaint every 50 ms. | Actor event loop owns state changes. Web transport publishes revisions. |
| Tab and settings sidebar | egui selectable tabs and a Tab key toggle. | Semantic web navigation and settings panel. |
| Ctrl+Q | Sends `ViewportCommand::Close`. | Web process lifecycle or browser close behavior. No remote shutdown command. |
| Escape | Sets agent/repeat/search/Procedure interrupt flags, sends `StopSpeaking`, and adds an interrupt notice. | Typed Stop command routed through actor ports. |
| Space and Ctrl+Space PTT | egui key state plus focus and settings visibility suppressors. | Focused browser keyboard handler calling the voice port. |
| Ctrl+V clipboard | Windows `GetAsyncKeyState` edge detection and `arboard` image read, encoded to PNG. | Multipart/paste upload adapter preserving accepted image limits. Delete Win32 polling. |
| Native file drop | egui dropped filesystem paths decoded through the existing image path helper. | Browser drop/select/paste upload endpoint. |
| Folder picker | `rfd::FileDialog` with one native dialog request. | Web command to a Rust `spawn_blocking` picker service. |
| Markdown rendering | `egui_commonmark::CommonMarkCache` and egui widgets. | TypeScript markdown component or bounded plain-text fallback. Delete egui cache. |
| Image rendering | `egui_extras::install_image_loaders` plus egui image bytes. | Browser image preview and transcript image component. |
| Audio capture/playback | Rust `VoiceService`, capture factory, Whisper, Kokoro, and TTS worker. | Retain in Rust behind a typed voice port. |
| Child process cleanup | Backend CLI, MCP, and Procedure paths use existing process-group boundaries. | Retain domain process ownership. Actor shutdown must close owned services. |

## Existing external GUI tests

All tests below are modules of the single external integration target at
`crates/deepseek-custom-tests/tests/it/main.rs`. The current inventory contains
297 `#[test]` functions across 12 pre-existing GUI files. The separate
`gui_characterization.rs` migration-boundary module adds five tests.

| Test file | Current contract covered | Migration destination |
| --- | --- | --- |
| `gui.rs` | `DeepSeekGui` construction, event routing, transcript projection, turn lifecycle, settings seeding, voice submission, cache/token counters, and raw formatting helpers. | Actor characterization and DTO/actor integration tests. |
| `gui_transcript.rs` | Transcript IDs, span coalescing, all block kinds, routed subagents, nesting, serialization, repeat events, pins, and terminal states. | Presentation-neutral transcript projection tests. |
| `gui_session_state.rs` | Session initialization, snapshots, autosave, title derivation, new/load/delete, empty-save behavior, and transcript restoration. | Actor session-service tests. |
| `gui_sessions_tab.rs` | Relative-time formatting for session rows. | Web/session DTO tests. |
| `gui_attachment.rs` | Attachment slot lifecycle, PNG encoding, invalid buffers, and Ctrl+V edge detection. | Upload/attachment service tests. |
| `gui_backend_picker.rs` | Backend/model defaults, selection, model list replacement/staleness, settings writes, and switch commands. | Backend port and visible-settings tests. |
| `gui_autopilot_tab.rs` | Repeat form defaults, channel/flag attachment, stop behavior, progress labels, and settings round trips. | Autopilot port and actor operation tests. |
| `gui_cascade_tab.rs` | Cascade form defaults, persisted values, validation, channel/stop wiring, progress, hints, and optional check command. | Cascade/search port tests. |
| `gui_evolve_tab.rs` | Evolve form defaults, persisted values, dispatch-count calculation, channel/stop wiring, and progress. | Evolve/search port tests. |
| `gui_search_view.rs` | Search running/finished view models, counts, scores, sparklines, previews, and idle state. | Search snapshot DTO tests and web view tests. |
| `gui_procedure_tab.rs` | Procedure selection, preview/run/apply/review controls, evidence rendering, terminal states, stale-event rejection, progress isolation, reports, repair, metrics, and trace export. | Procedure actor port and review-state tests. |
| `gui_voice_ui.rs` | Voice settings, channel wiring, event handling, reply speech, PTT policy, labels/colors, commands, and settings writers. | Voice port and visible voice-state tests. |

The native-only paint assertions and egui color/widget assumptions are deletion
candidates after equivalent actor, HTTP, web, or browser coverage exists. The
domain contracts in these tests remain required during the migration.

## Deletion list for final cutover

Do not delete these items in this milestone. This list identifies the final
deletion surface so later work can prove parity first.

- `crates/deepseek-custom/src/gui/draw.rs`, `draw_block.rs`, `panels.rs`, and native paint branches.
- `eframe`, `egui`, `egui_commonmark`, `egui_extras`, and native UI-only dependencies.

## Final native-test audit

The cutover audit mapped every external native GUI module before deletion.
Presentation-neutral transcript and session contracts moved with their source modules.
The remaining paint, widget, clipboard, and keyboard contracts are browser behavior now.

| Native test module | Maintained contract after cutover |
|---|---|
| `gui.rs`, `gui_characterization.rs` | `application_actor.rs`, `application_session.rs`, `web_server.rs`, and `web_browser.rs` cover ordered events, operation ownership, settings, sessions, interruption, and workspace behavior. |
| `gui_transcript.rs` | Moved to `application_transcript.rs`; browser transcript rendering and follow-output remain in `web_browser.rs`. |
| `gui_session_state.rs` | Moved to `application_session_state.rs`; actor and browser session lifecycle coverage remains in `application_actor.rs` and `web_browser.rs`. |
| `gui_attachment.rs` | `image_bytes.rs`, attachment HTTP tests in `web_server.rs`, and upload/preview coverage in `web_browser.rs`; native clipboard polling was deleted. |
| `gui_backend_picker.rs` | `application_services.rs`, `application_actor.rs`, and Settings browser scenarios cover visible options, persistence, and backend/model commands. |
| `gui_autopilot_tab.rs` | Actor command tests plus Autopilot browser start, progress, stop, and validation scenarios. |
| `gui_cascade_tab.rs`, `gui_evolve_tab.rs`, `gui_search_view.rs` | Actor search-port tests plus Cascade and Evolve browser validation, progress, result, and stop scenarios. |
| `gui_procedure_tab.rs` | Procedure actor/HTTP contracts and Procedure browser preview, review, apply, repair, metrics, trace, stale-event, and terminal-state scenarios. |
| `gui_sessions_tab.rs` | Session DTO/actor tests and Sessions browser new, load, delete, current-row, and deferred-switch scenarios. |
| `gui_voice_ui.rs` | Voice service tests, voice HTTP port tests, and browser push-to-talk state and control scenarios. |

No contract remained solely behind native paint accessors. The deleted-only surface was
egui layout, colors, viewport keys, clipboard polling, native image paint, and widget-local state.
- Native clipboard polling, egui dropped-file handling, egui image loaders, and paint-only test accessors.
- `DeepSeekGui`, `ActiveTab`, and `PendingSwitch` from the native module after actor ownership is complete.
- GUI-specific tests that only assert widgets, colors, layout, or paint implementation details.
- Native startup wiring in `main.rs`, only after the web startup and browser gates pass.
