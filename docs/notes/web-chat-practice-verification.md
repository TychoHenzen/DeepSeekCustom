# Web chat milestone practice verification

Run this deterministic verification from the repository root:

```powershell
rtk npm --prefix web test -- --run src/app/ChatMilestonePractice.test.tsx
```

The test mounts the real `App` and its chat, sessions, settings, attachment, and voice controls. A deterministic client seam publishes authoritative snapshots without network, model, audio-device, or native-dialog dependencies.

The assertions observe:

- ordered user, reasoning, answer, and terminal transcript output;
- an offline state followed by the explicit reconnect path;
- a pending session switch followed by the saved session becoming current;
- a settings update command followed by the persisted value in a newer snapshot;
- a confirmed folder path and a cancelled picker that leaves that path unchanged;
- image selection, preview acceptance, and the attachment identity on the chat command;
- voice readiness plus push-to-talk start and stop commands.

This is a deterministic practice gate. It does not prove browser audio hardware, an operating-system folder dialog, or a live model backend. Those integrations remain covered by their Rust adapter tests and later manual release verification.
