import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AudioDevice, Segment, SessionStatus, Settings, StreamEvent } from "./types";

type Tab = "record" | "settings";
type SaveState = "clean" | "saving" | "saved" | "error";

interface AppState {
  tab: Tab;
  settings: Settings;
  devices: AudioDevice[];
  status: SessionStatus;
  segments: Segment[];
  saveStates: Map<string, SaveState>;
  partial: string;
  errorMessage: string | null;
  title: string;
  minSpeakers: number;
  maxSpeakers: number;
  deviceName: string;
  chunksSent: number;
  eventsReceived: number;
}

const state: AppState = {
  tab: "record",
  settings: { parlyx_server_base_url: "", api_key: "", webhook_url: null },
  devices: [],
  status: { status: "Idle" },
  segments: [],
  saveStates: new Map(),
  partial: "",
  errorMessage: null,
  title: "",
  minSpeakers: 2,
  maxSpeakers: 6,
  deviceName: "",
  chunksSent: 0,
  eventsReceived: 0,
};

// Per-event-render performance — avoid full re-render on every transcript /
// status event because it nukes the DOM and steals focus from inputs the user
// is actively typing in. The full render runs on tab switches; surgical
// updates run on each event.
const segmentNodes = new Map<string, HTMLElement>();
const editDebounce = new Map<string, ReturnType<typeof setTimeout>>();
const savedFlashTimers = new Map<string, ReturnType<typeof setTimeout>>();

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
    updateStatusBar();
    updateControls();
  });
  await listen<number>("parlyx://chunk-sent", (evt) => {
    state.chunksSent = evt.payload;
    updateStatusBar();
  });
  await listen<number>("parlyx://event-received", (evt) => {
    state.eventsReceived = evt.payload;
    updateStatusBar();
  });

  fullRender();
}

function handleStreamEvent(evt: StreamEvent) {
  switch (evt.type) {
    case "transcript":
      addSegment({
        id: evt.segment_id,
        speaker: evt.speaker ?? "—",
        text: evt.text,
        timestamp: evt.timestamp,
      });
      state.partial = "";
      updatePartial();
      break;
    case "partial":
      state.partial = evt.text;
      updatePartial();
      break;
    case "speakerrename":
      for (const seg of state.segments) {
        if (seg.speaker === evt.from) {
          seg.speaker = evt.to;
          applyRemoteSpeaker(seg.id, evt.to);
        }
      }
      break;
    case "diarization":
      /* TODO: apply retroactive speaker labels by start/end matching */
      break;
    case "error":
      state.errorMessage = evt.message;
      showErrorBanner();
      break;
    case "complete":
      /* status update will follow via parlyx://status */
      break;
  }
}

function activeStreamId(): string | null {
  if (state.status.status === "Recording" || state.status.status === "Paused") {
    return state.status.stream_id;
  }
  return null;
}

function setSaveState(segId: string, st: SaveState) {
  state.saveStates.set(segId, st);
  const node = segmentNodes.get(segId);
  if (!node) return;
  const dot = node.querySelector<HTMLElement>(".save-dot");
  if (!dot) return;
  dot.className = `save-dot ${st}`;
  dot.title = st;
  // Auto-fade "saved" back to "clean" after 1.5 s so the row settles.
  if (st === "saved") {
    const prev = savedFlashTimers.get(segId);
    if (prev) clearTimeout(prev);
    savedFlashTimers.set(
      segId,
      setTimeout(() => setSaveState(segId, "clean"), 1500),
    );
  }
}

async function scheduleSegmentSave(segId: string) {
  const existing = editDebounce.get(segId);
  if (existing) clearTimeout(existing);
  const sid = activeStreamId();
  if (!sid) return;
  const seg = state.segments.find((s) => s.id === segId);
  if (!seg) return;
  setSaveState(segId, "saving");
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
      setSaveState(segId, "saved");
    } catch (e) {
      state.errorMessage = `segment save failed: ${e}`;
      setSaveState(segId, "error");
      showErrorBanner();
    }
  }, 600);
  editDebounce.set(segId, timer);
}

async function saveSettings() {
  try {
    await invoke("save_settings", { settings: state.settings });
    state.errorMessage = null;
    showErrorBanner();
  } catch (e) {
    state.errorMessage = `save_settings failed: ${e}`;
    showErrorBanner();
  }
}

async function startRecording() {
  state.errorMessage = null;
  state.segments = [];
  state.saveStates.clear();
  state.partial = "";
  state.chunksSent = 0;
  state.eventsReceived = 0;
  segmentNodes.clear();
  fullRender();
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
    showErrorBanner();
  }
}

async function pauseRecording() {
  try { await invoke("pause_streaming"); } catch (e) { state.errorMessage = `pause failed: ${e}`; showErrorBanner(); }
}
async function resumeRecording() {
  try { await invoke("resume_streaming"); } catch (e) { state.errorMessage = `resume failed: ${e}`; showErrorBanner(); }
}
async function stopRecording() {
  try { await invoke<SessionStatus>("stop_streaming"); } catch (e) { state.errorMessage = `stop failed: ${e}`; showErrorBanner(); }
}

// ── rendering ─────────────────────────────────────────────────────────────

function fullRender() {
  const app = document.getElementById("app")!;
  app.innerHTML = "";
  app.appendChild(renderTopbar());
  const main = document.createElement("main");
  main.id = "main";
  main.appendChild(buildErrorBanner());
  if (state.tab === "record") main.appendChild(renderRecordTab());
  else main.appendChild(renderSettingsTab());
  app.appendChild(main);
  app.appendChild(renderStatusBar());
  showErrorBanner();
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
      fullRender();
    });
  });
  return top;
}

function buildErrorBanner(): HTMLElement {
  const div = document.createElement("div");
  div.id = "error-banner";
  div.className = "error-banner";
  div.style.display = "none";
  return div;
}
function showErrorBanner() {
  const div = document.getElementById("error-banner");
  if (!div) return;
  if (state.errorMessage) {
    div.textContent = state.errorMessage;
    div.style.display = "";
  } else {
    div.textContent = "";
    div.style.display = "none";
  }
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
  card.id = "config-card";
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
    <div class="controls" id="controls">${renderControlButtons()}</div>
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
  bindControlButtons(card);
  return card;
}

function bindControlButtons(scope: ParentNode) {
  scope.querySelectorAll<HTMLButtonElement>("[data-action]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const action = btn.dataset.action;
      if (action === "start") startRecording();
      else if (action === "pause") pauseRecording();
      else if (action === "resume") resumeRecording();
      else if (action === "stop") stopRecording();
    });
  });
}

function updateControls() {
  const controls = document.getElementById("controls");
  if (!controls) return;
  controls.innerHTML = renderControlButtons();
  bindControlButtons(controls);
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
    <div class="transcript" id="transcript"></div>
    <div class="muted" id="partial-line" style="margin-top:8px;display:none"></div>
  `;
  const list = card.querySelector<HTMLElement>("#transcript")!;
  segmentNodes.clear();
  for (const seg of state.segments) {
    const node = buildSegmentNode(seg);
    list.appendChild(node);
    segmentNodes.set(seg.id, node);
  }
  if (state.segments.length === 0) {
    list.innerHTML = '<div class="muted">// transcript will appear here as the stream is diarized</div>';
  }
  updatePartial();
  return card;
}

function addSegment(seg: Segment) {
  // de-dupe by id (parlyx may re-emit on reconnect)
  if (segmentNodes.has(seg.id)) {
    const existing = state.segments.find((s) => s.id === seg.id);
    if (existing) {
      existing.text = seg.text;
      existing.speaker = seg.speaker;
      existing.timestamp = seg.timestamp;
      applyRemoteSpeaker(seg.id, seg.speaker);
      applyRemoteText(seg.id, seg.text);
    }
    return;
  }
  state.segments.push(seg);
  const list = document.getElementById("transcript");
  if (!list) return;
  if (state.segments.length === 1) list.innerHTML = "";
  const node = buildSegmentNode(seg);
  list.appendChild(node);
  segmentNodes.set(seg.id, node);
  // Scroll into view if we were near the bottom.
  const main = document.getElementById("main");
  if (main) {
    const nearBottom = main.scrollTop + main.clientHeight + 200 >= main.scrollHeight;
    if (nearBottom) node.scrollIntoView({ behavior: "smooth", block: "end" });
  }
}

function buildSegmentNode(seg: Segment): HTMLElement {
  const div = document.createElement("div");
  div.className = "segment";
  div.dataset.seg = seg.id;
  div.innerHTML = `
    <div class="segment-speaker">
      <input type="text" value="${escapeAttr(seg.speaker)}" data-seg="${escapeAttr(seg.id)}"/>
      <div class="ts">${escapeHtml(seg.timestamp)}</div>
    </div>
    <div class="segment-text" contenteditable="true" data-seg="${escapeAttr(seg.id)}">${escapeHtml(seg.text)}</div>
    <span class="save-dot clean" title="clean"></span>
  `;
  const spkInput = div.querySelector<HTMLInputElement>("input")!;
  spkInput.addEventListener("input", () => {
    const s = state.segments.find((x) => x.id === seg.id);
    if (!s) return;
    s.speaker = spkInput.value;
    scheduleSegmentSave(seg.id);
  });
  const textCell = div.querySelector<HTMLElement>(".segment-text")!;
  textCell.addEventListener("input", () => {
    const s = state.segments.find((x) => x.id === seg.id);
    if (!s) return;
    s.text = textCell.innerText;
    scheduleSegmentSave(seg.id);
  });
  return div;
}

/// A remote speaker rename should NOT clobber the user's active edit. Skip if
/// the input is currently focused.
function applyRemoteSpeaker(segId: string, newSpeaker: string) {
  const node = segmentNodes.get(segId);
  if (!node) return;
  const input = node.querySelector<HTMLInputElement>(".segment-speaker input");
  if (!input) return;
  if (document.activeElement === input) return;
  input.value = newSpeaker;
}

function applyRemoteText(segId: string, newText: string) {
  const node = segmentNodes.get(segId);
  if (!node) return;
  const cell = node.querySelector<HTMLElement>(".segment-text");
  if (!cell) return;
  if (document.activeElement === cell) return;
  cell.innerText = newText;
}

function updatePartial() {
  const line = document.getElementById("partial-line");
  if (!line) return;
  if (state.partial) {
    line.style.display = "";
    line.textContent = "… " + state.partial;
  } else {
    line.style.display = "none";
    line.textContent = "";
  }
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
  bar.id = "status-bar";
  bar.innerHTML = statusBarInner();
  return bar;
}

function updateStatusBar() {
  const bar = document.getElementById("status-bar");
  if (!bar) return;
  bar.innerHTML = statusBarInner();
}

function statusBarInner(): string {
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
  return `
    <span class="status-dot ${dotClass}"></span>
    <span>${escapeHtml(label)}</span>
    <span style="margin-left:auto;color:var(--text-muted);font-size:11px">
      chunks: ${state.chunksSent} · events: ${state.eventsReceived}
    </span>
  `;
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]!));
}
function escapeAttr(s: string): string {
  return escapeHtml(s);
}

init();
