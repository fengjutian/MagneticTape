import { invoke } from "@tauri-apps/api/core";
import { applyAudioDevices, applyView, renderApp } from "./components";
import type { AudioDeviceSnapshot, RecorderViewState, SessionState } from "./types";
import "./styles.css";

const appElement = document.querySelector<HTMLElement>("#app");
if (!appElement) throw new Error("Missing application root");
const app: HTMLElement = appElement;
app.innerHTML = renderApp();

let backendState: SessionState;
let pending = false;
let tickHandle: number | undefined;
let view: RecorderViewState = { title: "新录音", status: "idle", elapsedMs: 0, language: "auto", translation: "none", retainAudio: true, volumeLevel: 0, sessionId: null };

function syncBackend(next: SessionState): void {
  backendState = next;
  view = { ...view, status: next.status, elapsedMs: next.elapsedMs, sessionId: next.sessionId, errorMessage: undefined };
  restartClock();
  applyView(app, view);
}

function restartClock(): void {
  window.clearInterval(tickHandle);
  if (backendState?.status !== "recording") return;
  const baseElapsed = backendState.elapsedMs;
  const syncedAt = Date.now();
  tickHandle = window.setInterval(() => {
    view.elapsedMs = baseElapsed + Date.now() - syncedAt;
    applyView(app, view);
  }, 250);
}

async function command(name: "start_session" | "pause_session" | "resume_session" | "stop_session"): Promise<void> {
  if (pending) return;
  pending = true;
  view.errorMessage = undefined;
  if (name === "stop_session") {
    view.status = "stopping";
    window.clearInterval(tickHandle);
    applyView(app, view);
  }
  try {
    syncBackend(await invoke<SessionState>(name));
  } catch (error) {
    view.status = backendState?.status ?? "error";
    view.errorMessage = error instanceof Error ? error.message : String(error);
    applyView(app, view);
  } finally {
    pending = false;
  }
}

async function refreshAudioDevices(): Promise<void> {
  const button = app.querySelector<HTMLButtonElement>(".refresh-devices")!;
  button.disabled = true;
  button.textContent = "检测中…";
  try {
    applyAudioDevices(app, await invoke<AudioDeviceSnapshot>("list_audio_devices"));
  } catch (error) {
    view.errorMessage = `无法检测音频设备：${error instanceof Error ? error.message : String(error)}`;
    applyView(app, view);
  } finally {
    button.disabled = false;
    button.textContent = "重新检测";
  }
}

app.querySelector<HTMLButtonElement>(".start-button")!.addEventListener("click", () => void command("start_session"));
app.querySelector<HTMLButtonElement>(".pause-button")!.addEventListener("click", () => void command(view.status === "paused" ? "resume_session" : "pause_session"));
app.querySelector<HTMLButtonElement>(".stop-button")!.addEventListener("click", () => void command("stop_session"));
app.querySelector<HTMLButtonElement>(".refresh-devices")!.addEventListener("click", () => void refreshAudioDevices());

const titleDisplay = app.querySelector<HTMLButtonElement>(".title-display")!;
const titleInput = app.querySelector<HTMLInputElement>(".title-input")!;
let originalTitle = view.title;
function finishTitleEdit(cancel = false): void {
  view.title = cancel ? originalTitle : titleInput.value.trim() || "新录音";
  titleInput.hidden = true;
  titleDisplay.hidden = false;
  applyView(app, view);
}
titleDisplay.addEventListener("click", () => {
  originalTitle = view.title;
  titleDisplay.hidden = true;
  titleInput.hidden = false;
  titleInput.value = view.title;
  titleInput.focus();
  titleInput.select();
});
titleInput.addEventListener("blur", () => {
  if (!titleInput.hidden) finishTitleEdit();
});
titleInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") { event.preventDefault(); finishTitleEdit(); }
  if (event.key === "Escape") { event.preventDefault(); finishTitleEdit(true); }
});

app.querySelectorAll<HTMLSelectElement>("select[data-option]").forEach((select) => select.addEventListener("change", () => {
  if (select.dataset.option === "language") view.language = select.value;
  if (select.dataset.option === "translation") view.translation = select.value;
  if (select.dataset.option === "audio") view.retainAudio = select.value === "retain";
}));

document.addEventListener("keydown", (event) => {
  const target = event.target as HTMLElement;
  if (target.matches("input, textarea, select, [contenteditable=true]")) return;
  if (event.code === "Space" && (view.status === "recording" || view.status === "paused")) {
    event.preventDefault();
    void command(view.status === "paused" ? "resume_session" : "pause_session");
  }
  if (event.ctrlKey && event.key === "Enter" && (view.status === "recording" || view.status === "paused")) {
    event.preventDefault();
    void command("stop_session");
  }
});

syncBackend(await invoke<SessionState>("get_session_state"));
await refreshAudioDevices();

