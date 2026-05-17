import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AudioDevice, Segment, SessionStatus, Settings, StreamEvent } from "./types";

type Tab = "record" | "settings";

interface AppState {
  tab: Tab;
  settings: Settings;
  devices: AudioDevice[];
  status: SessionStatus;
  segments: Segment[];
  partial: string;
  errorMessage: string | null;
  title: string;
  minSpeakers: number;
  maxSpeakers: number;
  deviceName: string;
}

const state: AppState = {
  tab: "record",
  settings: { parlyx_server_base_url: "", api_key: "", webhook_url: null },
  devices: [],
  status: { status: "Idle" },
  segments: [],
  partial: "",
  errorMessage: null,
  title: "",
  minSpeakers: 2,
  maxSpeakers: 6,
  deviceName: "",
};

const editDebounce = new Map<string, ReturnType<typeof setTimeout>>();

async function init() {
  try {
    state.settings = await invoke<Settings>("load_settings");
  } catch (e) {
    state.errorMessage = `failed to load settings: ${e}`;
  }
  try {
    state.devices = await invoke<AudioDevice[]>("list_input_devices");
    const def = state.devices.find((d) => d.is_default);
    if (def) state.deviceName = def.name;
  } catch (e) {
    state.errorMessage = `failed to list devices: ${e}`;
  }
  try {
    state.status = await invoke<SessionStatus>("current_state");
  } catch {
    /* default Idle is fine */
  }

  await listen<StreamEvent>("parlyx://stream-event", (evt) => handleStreamEvent(evt.payload));
  await listen<SessionStatus>("parlyx://status", (evt) => {
    state.status = evt.payload;
    if (evt.payload.status === "Error") state.errorMessage = evt.payload.message;
    render();
  });

  render();
}

function handleStreamEvent(evt: StreamEvent) {
  switch (evt.type) {
    case "transcript":
      state.segments.push({
        id: evt.segment_id,
        speaker: evt.speaker ?? "—",
        text: evt.text,
        timestamp: evt.timestamp,
      });
      state.partial = "";
      break;
    case "partial":
      state.partial = evt.text;
      break;
    case "speaker_rename":
      for (const seg of state.segments) {
        if (seg.speaker === evt.from) seg.speaker = evt.to;
      }
      break;
    case "diarization":
      /* TODO: apply retroactive speaker labels to existing segments */
      break;
    case "error":
      state.errorMessage = evt.message;
      break;
    case "complete":
      /* status update will follow via parlyx://status */
      break;
  }
  render();
}

function activeStreamId(): string | null {
  if (state.status.status === "Recording" || state.status.status === "Paused") {
    return state.status.stream_id;
  }
  return null;
}

async function scheduleSegmentSave(segId: string) {
  const existing = editDebounce.get(segId);
  if (existing) clearTimeout(existing);
  const sid = activeStreamId();
  if (!sid) return;
  const seg = state.segments.find((s) => s.id === segId);
  if (!seg) return;
  const timer = setTimeout(async () => {
    try {
      await invoke("update_segment", {
        args: {
          stream_id: sid,
          segment_id: segId,
          text: seg.text,
          speaker: seg.speaker,
        },
      });
    } catch (e) {
      state.errorMessage = `segment save failed: ${e}`;
      render();
    }
  }, 600);
  editDebounce.set(segId, timer);
}

async function saveSettings() {
  try {
    await invoke("save_settings", { settings: state.settings });
    state.errorMessage = null;
  } catch (e) {
    state.errorMessage = `save_settings failed: ${e}`;
  }
  render();
}

async function startRecording() {
  state.errorMessage = null;
  state.segments = [];
  state.partial = "";
  try {
    await invoke<SessionStatus>("start_streaming", {
      args: {
        title: state.title || null,
        min_speakers: state.minSpeakers || null,
        max_speakers: state.maxSpeakers || null,
        device_name: state.deviceName || null,
      },
    });
  } catch (e) {
    state.errorMessage = `start failed: ${e}`;
    render();
  }
}

async function pauseRecording() {
  try {
    await invoke("pause_streaming");
  } catch (e) {
    state.errorMessage = `pause failed: ${e}`;
    render();
  }
}

async function resumeRecording() {
  try {
    await invoke("resume_streaming");
  } catch (e) {
    state.errorMessage = `resume failed: ${e}`;
    render();
  }
}

async function stopRecording() {
  try {
    await invoke<SessionStatus>("stop_streaming");
  } catch (e) {
    state.errorMessage = `stop failed: ${e}`;
    render();
  }
}

// ── rendering ─────────────────────────────────────────────────────────────

function render() {
  const app = document.getElementById("app")!;
  app.innerHTML = "";
  app.appendChild(renderTopbar());
  const main = document.createElement("main");
  if (state.errorMessage) {
    const banner = document.createElement("div");
    banner.className = "error-banner";
    banner.textContent = state.errorMessage;
    main.appendChild(banner);
  }
  if (state.tab === "record") main.appendChild(renderRecordTab());
  else main.appendChild(renderSettingsTab());
  app.appendChild(main);
  app.appendChild(renderStatusBar());
}

function renderTopbar(): HTMLElement {
  const top = document.createElement("div");
  top.className = "topbar";
  top.innerHTML = `
    <h1>parlyx streamer</h1>
    <div class="tabs">
      <button class="tab ${state.tab === "record" ? "active" : ""}" data-tab="record">record</button>
      <button class="tab ${state.tab === "settings" ? "active" : ""}" data-tab="settings">settings</button>
    </div>
  `;
  top.querySelectorAll<HTMLButtonElement>(".tab").forEach((btn) => {
    btn.addEventListener("click", () => {
      state.tab = btn.dataset.tab as Tab;
      render();
    });
  });
  return top;
}

function renderRecordTab(): HTMLElement {
  const wrap = document.createElement("div");
  wrap.appendChild(renderConfigCard());
  wrap.appendChild(renderTranscriptCard());
  return wrap;
}

function renderConfigCard(): HTMLElement {
  const card = document.createElement("div");
  card.className = "card";
  card.innerHTML = `
    <h2>recording</h2>
    <div class="form-row">
      <label>title / filename</label>
      <input type="text" id="title" value="${escapeAttr(state.title)}" placeholder="meeting-2026-05-17"/>
    </div>
    <div class="form-row">
      <label>input device</label>
      <select id="device">
        <option value="">(default)</option>
        ${state.devices.map((d) => `<option value="${escapeAttr(d.name)}" ${d.name === state.deviceName ? "selected" : ""}>${escapeHtml(d.name)}${d.is_default ? " — default" : ""}</option>`).join("")}
      </select>
    </div>
    <div class="form-row">
      <label>speakers (min / max)</label>
      <div style="display:flex;gap:8px">
        <input type="number" id="min_spk" value="${state.minSpeakers}" min="1" max="20" style="width:80px"/>
        <input type="number" id="max_spk" value="${state.maxSpeakers}" min="1" max="20" style="width:80px"/>
      </div>
    </div>
    <div class="controls">${renderControlButtons()}</div>
  `;
  card.querySelector<HTMLInputElement>("#title")!.addEventListener("input", (e) => {
    state.title = (e.target as HTMLInputElement).value;
  });
  card.querySelector<HTMLSelectElement>("#device")!.addEventListener("change", (e) => {
    state.deviceName = (e.target as HTMLSelectElement).value;
  });
  card.querySelector<HTMLInputElement>("#min_spk")!.addEventListener("change", (e) => {
    state.minSpeakers = parseInt((e.target as HTMLInputElement).value || "0", 10);
  });
  card.querySelector<HTMLInputElement>("#max_spk")!.addEventListener("change", (e) => {
    state.maxSpeakers = parseInt((e.target as HTMLInputElement).value || "0", 10);
  });
  card.querySelectorAll<HTMLButtonElement>("[data-action]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const action = btn.dataset.action;
      if (action === "start") startRecording();
      else if (action === "pause") pauseRecording();
      else if (action === "resume") resumeRecording();
      else if (action === "stop") stopRecording();
    });
  });
  return card;
}

function renderControlButtons(): string {
  switch (state.status.status) {
    case "Idle":
    case "Stopped":
    case "Error":
      return `<button class="btn btn-primary" data-action="start">● start</button>`;
    case "Starting":
      return `<button class="btn" disabled>starting…</button>`;
    case "Recording":
      return `
        <button class="btn btn-warn" data-action="pause">‖ pause</button>
        <button class="btn btn-danger" data-action="stop">■ stop</button>
      `;
    case "Paused":
      return `
        <button class="btn btn-primary" data-action="resume">▶ resume</button>
        <button class="btn btn-danger" data-action="stop">■ stop</button>
      `;
    case "Stopping":
      return `<button class="btn" disabled>stopping…</button>`;
  }
}

function renderTranscriptCard(): HTMLElement {
  const card = document.createElement("div");
  card.className = "card";
  card.innerHTML = `
    <h2>transcript</h2>
    <div class="transcript" id="transcript">${
      state.segments.length === 0
        ? '<div class="muted">// transcript will appear here as the stream is diarized</div>'
        : state.segments.map(renderSegmentHTML).join("")
    }</div>
    ${state.partial ? `<div class="muted" style="margin-top:8px">… ${escapeHtml(state.partial)}</div>` : ""}
  `;
  card.querySelectorAll<HTMLInputElement>(".segment-speaker input").forEach((input) => {
    input.addEventListener("input", () => {
      const segId = input.dataset.seg!;
      const seg = state.segments.find((s) => s.id === segId);
      if (seg) {
        seg.speaker = input.value;
        scheduleSegmentSave(segId);
      }
    });
  });
  card.querySelectorAll<HTMLElement>(".segment-text").forEach((cell) => {
    cell.addEventListener("input", () => {
      const segId = cell.dataset.seg!;
      const seg = state.segments.find((s) => s.id === segId);
      if (seg) {
        seg.text = cell.innerText;
        scheduleSegmentSave(segId);
      }
    });
  });
  return card;
}

function renderSegmentHTML(seg: Segment): string {
  return `
    <div class="segment">
      <div class="segment-speaker">
        <input type="text" value="${escapeAttr(seg.speaker)}" data-seg="${escapeAttr(seg.id)}"/>
        <div class="ts">${escapeHtml(seg.timestamp)}</div>
      </div>
      <div class="segment-text" contenteditable="true" data-seg="${escapeAttr(seg.id)}">${escapeHtml(seg.text)}</div>
    </div>
  `;
}

function renderSettingsTab(): HTMLElement {
  const card = document.createElement("div");
  card.className = "card";
  card.innerHTML = `
    <h2>parlyx server</h2>
    <div class="form-row">
      <label>base_url</label>
      <input type="url" id="base_url" value="${escapeAttr(state.settings.parlyx_server_base_url)}" placeholder="http://10.1.0.75:5555"/>
    </div>
    <div class="form-row">
      <label>api_key</label>
      <input type="password" id="api_key" value="${escapeAttr(state.settings.api_key)}" placeholder="parlyx api key"/>
    </div>
    <div class="form-row">
      <label>webhook_url</label>
      <input type="url" id="webhook_url" value="${escapeAttr(state.settings.webhook_url ?? "")}" placeholder="(optional) https://…"/>
    </div>
    <div class="controls">
      <button class="btn btn-primary" id="btn-save-settings">save</button>
    </div>
    <div class="muted" style="margin-top:8px">// saved to your platform config dir</div>
  `;
  card.querySelector<HTMLInputElement>("#base_url")!.addEventListener("input", (e) => {
    state.settings.parlyx_server_base_url = (e.target as HTMLInputElement).value.trim();
  });
  card.querySelector<HTMLInputElement>("#api_key")!.addEventListener("input", (e) => {
    state.settings.api_key = (e.target as HTMLInputElement).value.trim();
  });
  card.querySelector<HTMLInputElement>("#webhook_url")!.addEventListener("input", (e) => {
    const v = (e.target as HTMLInputElement).value.trim();
    state.settings.webhook_url = v ? v : null;
  });
  card.querySelector<HTMLButtonElement>("#btn-save-settings")!.addEventListener("click", saveSettings);
  return card;
}

function renderStatusBar(): HTMLElement {
  const bar = document.createElement("div");
  bar.className = "status-bar";
  let dotClass = "idle";
  let label = "idle";
  switch (state.status.status) {
    case "Recording":
      dotClass = "recording";
      label = `recording — stream ${state.status.stream_id.slice(0, 8)} — task ${state.status.task_id.slice(0, 8)}`;
      break;
    case "Paused":
      dotClass = "paused";
      label = `paused — stream ${state.status.stream_id.slice(0, 8)}`;
      break;
    case "Starting":
      label = "starting…";
      break;
    case "Stopping":
      label = "stopping…";
      break;
    case "Stopped":
      label = state.status.task_id ? `stopped — task ${state.status.task_id.slice(0, 8)}` : "stopped";
      break;
    case "Error":
      label = `error — ${state.status.message}`;
      break;
  }
  bar.innerHTML = `<span class="status-dot ${dotClass}"></span><span>${escapeHtml(label)}</span>`;
  return bar;
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]!));
}
function escapeAttr(s: string): string {
  return escapeHtml(s);
}

init();
