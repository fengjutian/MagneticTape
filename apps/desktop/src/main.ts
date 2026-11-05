import { invoke } from "@tauri-apps/api/core";
import { applyAudioDevices, applyView, renderApp } from "./components";
import type { AiResponse, AudioDeviceSnapshot, AudioLevels, RecorderViewState, SessionState } from "./types";
import "./styles.css";

const appElement = document.querySelector<HTMLElement>("#app");
if (!appElement) throw new Error("Missing application root");
const app: HTMLElement = appElement;
app.innerHTML = renderApp();

let backendState: SessionState;
let pending = false;
let tickHandle: number | undefined;
let levelHandle: number | undefined;
let view: RecorderViewState = { title: "新录音", status: "idle", elapsedMs: 0, language: "auto", translation: "none", retainAudio: true, volumeLevel: 0, sessionId: null };

function syncBackend(next: SessionState): void {
  backendState = next;
  view = { ...view, status: next.status, elapsedMs: next.elapsedMs, sessionId: next.sessionId, microphonePath: next.microphonePath, systemPath: next.systemPath, droppedBlocks: next.droppedBlocks, recovered: next.recovered, errorMessage: undefined };
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
    if (name === "stop_session") stopLevelPolling();
  } catch (error) {
    view.status = backendState?.status ?? "error";
    view.errorMessage = error instanceof Error ? error.message : String(error);
    applyView(app, view);
  } finally {
    pending = false;
  }
}

async function startRecording(): Promise<void> {
  if (pending) return;
  const microphoneId = app.querySelector<HTMLSelectElement>(".microphone-select")!.value;
  const systemOutputId = app.querySelector<HTMLSelectElement>(".system-output-select")!.value;
  if (!microphoneId || !systemOutputId) {
    view.errorMessage = "需要可用的麦克风和系统输出设备才能开始监测。";
    applyView(app, view);
    return;
  }
  pending = true;
  try {
    syncBackend(await invoke<SessionState>("start_session", { microphoneId, systemOutputId }));
    startLevelPolling();
  } catch (error) {
    view.errorMessage = `无法启动音频监测：${error instanceof Error ? error.message : String(error)}`;
    applyView(app, view);
  } finally {
    pending = false;
  }
}

function startLevelPolling(): void {
  stopLevelPolling();
  levelHandle = window.setInterval(async () => {
    if (view.status !== "recording") return;
    try {
      const levels = await invoke<AudioLevels>("get_audio_levels");
      const level = Math.max(levels.microphone, levels.system);
      app.style.setProperty("--audio-level", level.toFixed(3));
      app.querySelector<HTMLElement>(".waveform")!.title = `麦克风 ${Math.round(levels.microphone * 100)}% · 系统声音 ${Math.round(levels.system * 100)}%`;
    } catch {
      app.style.setProperty("--audio-level", "0");
    }
  }, 100);
}

function stopLevelPolling(): void {
  window.clearInterval(levelHandle);
  app.style.setProperty("--audio-level", "0");
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

app.querySelector<HTMLButtonElement>(".start-button")!.addEventListener("click", () => void startRecording());
app.querySelector<HTMLButtonElement>(".pause-button")!.addEventListener("click", () => void command(view.status === "paused" ? "resume_session" : "pause_session"));
app.querySelector<HTMLButtonElement>(".stop-button")!.addEventListener("click", () => void command("stop_session"));
app.querySelector<HTMLButtonElement>(".refresh-devices")!.addEventListener("click", () => void refreshAudioDevices());

const aiDialog = app.querySelector<HTMLDialogElement>(".ai-dialog")!;
const aiProvider = app.querySelector<HTMLSelectElement>(".ai-provider")!;
const aiModel = app.querySelector<HTMLInputElement>(".ai-model")!;
const aiKey = app.querySelector<HTMLInputElement>(".ai-key")!;
const aiResult = app.querySelector<HTMLOutputElement>(".ai-result")!;
app.querySelector<HTMLButtonElement>(".ai-settings")!.addEventListener("click", () => aiDialog.showModal());
aiProvider.addEventListener("change", () => {
  aiModel.value = aiProvider.value === "minimax" ? "MiniMax-M3.1-Flash" : "deepseek-flash";
  aiResult.textContent = "";
});
app.querySelector<HTMLButtonElement>(".ai-test")!.addEventListener("click", async (event) => {
  const button = event.currentTarget as HTMLButtonElement;
  button.disabled = true;
  aiResult.className = "ai-result pending";
  aiResult.textContent = "正在连接…";
  try {
    const provider = aiProvider.value;
    await invoke("configure_ai", { provider, apiKey: aiKey.value, model: aiModel.value });
    const response = await invoke<AiResponse>("test_ai", { provider });
    aiKey.value = "";
    aiResult.className = "ai-result success";
    aiResult.textContent = `${response.model}：${response.content}`;
  } catch (error) {
    aiResult.className = "ai-result failure";
    aiResult.textContent = error instanceof Error ? error.message : String(error);
  } finally {
    button.disabled = false;
  }
});
app.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".reveal-recording");
  if (!button?.dataset.path) return;
  button.disabled = true;
  try {
    await invoke("reveal_recording", { path: button.dataset.path });
  } catch (error) {
    view.errorMessage = `无法定位录音文件：${error instanceof Error ? error.message : String(error)}`;
    applyView(app, view);
  } finally {
    button.disabled = false;
  }
});

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

const recovered = await invoke<SessionState | null>("recover_latest_session");
syncBackend(recovered ?? await invoke<SessionState>("get_session_state"));
await refreshAudioDevices();

