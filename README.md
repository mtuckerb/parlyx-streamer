# parlyx-streamer

Lightweight Tauri 2 + Rust desktop client for [parlyx](../parlyx). Captures
audio from a chosen input device, streams it in 3-second chunks to a parlyx
server, subscribes to the live diarized transcript over SSE, and lets you
edit speaker labels and transcription text in real time.

## Features

- **Settings**: parlyx base URL, API key, optional webhook callback. Persisted
  to your platform config dir as `settings.json`.
- **Input device picker**: any device cpal can enumerate (mic, line-in,
  virtual loopback). Falls back to system default.
- **Filename / speaker count**: passed to parlyx as the task title plus
  `min_speakers`/`max_speakers` diarization hints.
- **Start / Pause / Stop** with state machine wired to parlyx's
  `/streaming/start | /chunk | /finish | /cancel`.
- **Live editable transcript**: each diarized segment renders with an
  editable speaker label and contenteditable text. Edits are debounced 600 ms
  and PUT to `/streaming/:id/segments/:segId`.
- **Backgrounding**: closing the window hides to background instead of
  exiting, so capture + streaming continue.

## Build / dev

```sh
# one-time
npm install

# dev (Vite + tauri dev — opens the app window with hot reload)
npm run tauri dev

# release build
npm run tauri build

# icon assets (one-time)
npm run tauri icon path/to/source.png
```

`npm run tauri build` produces a platform-native bundle:
- macOS: `.dmg`, `.app`
- Linux: `.deb`, `.AppImage`
- Windows: `.msi`, `.exe`

## Architecture

```
[cpal callback]
  → std::thread (audio.rs)            ── 16 kHz mono f32 chunks via
       │                                 tokio::sync::mpsc::UnboundedSender
       ▼
  tokio task (session.rs)             ── encode_wav → POST /streaming/:id/chunk
       │
  tokio task (session.rs)             ── GET /streaming/:id/events (SSE)
       │
  Tauri event bus                     ── parlyx://stream-event → web UI
       │
  web UI (src/main.ts)                ── render segments, debounced PUT
```

## Notes

- Application audio capture (capturing a specific app's output) is **not**
  done here. Use OS virtual audio routing instead: BlackHole on macOS,
  PulseAudio/PipeWire loopback on Linux, WASAPI loopback (or VoiceMeeter) on
  Windows — then pick that virtual device in the input picker.
- The Rust binary uses pure cross-platform crates (`cpal`, `reqwest` with
  rustls, `rubato`). No native runtime requirements beyond what Tauri itself
  needs (webview2 on Windows, WebKitGTK on Linux, system WebKit on macOS).
- API contract assumes the streaming endpoints from parlyx PRs #6/#8/#12 are
  in place.
