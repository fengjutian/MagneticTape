import { invoke } from "@tauri-apps/api/core";
import { applyAudioDevices, applyView, formatElapsed, renderApp, renderTranscript } from "./components";
import type { AiConfiguration, AiResponse, AsrConfiguration, AudioDeviceSnapshot, AudioLevels, LiveTranscript, RecorderViewState, RecordingListItem, SessionState, Transcript, TranscriptSegment } from "./types";
import "./styles.css";

const appElement = document.querySelector<HTMLElement>("#app");
if (!appElement) throw new Error("Missing application root");
const app: HTMLElement = appElement;
app.innerHTML = renderApp();

let backendState: SessionState;
let pending = false;
let tickHandle: number | undefined;
let levelHandle: number | undefined;
let currentTranscript: Transcript | null = null;
let liveDesired = false;
let livePollHandle: number | undefined;
let livePollBusy = false;
let liveSegments: TranscriptSegment[] = [];
let liveError: string | undefined;

const liveButton = app.querySelector<HTMLButtonElement>(".live-caption")!;
function setLiveIndicator(active: boolean): void {
  liveButton.classList.toggle("active", active);
  liveButton.querySelector<HTMLElement>(".live-state")!.textContent = active ? "开" : "关";
}

function renderLive(): void {
  const body = app.querySelector<HTMLElement>(".content-body");
  if (!body) return;
  if (view.status !== "recording" && view.status !== "paused") return;
  if (liveSegments.length === 0) {
    if (liveError) {
      body.replaceChildren();
      const failure = document.createElement("p");
      failure.className = "live-error";
      failure.textContent = `实时字幕暂不可用：${liveError}`;
      body.append(failure);
    }
    return;
  }
  renderTranscript(body, {
    provider: "live",
    model: "",
    language: view.language,
    durationMs: 0,
    text: "",
    segments: liveSegments,
    createdAtEpochMs: 0,
  });
  if (liveError) {
    const failure = document.createElement("p");
    failure.className = "live-error";
    failure.textContent = `实时字幕错误：${liveError}`;
    body.append(failure);
  }
  body.scrollTop = body.scrollHeight;
}

async function pollLive(): Promise<void> {
  if (livePollBusy) return;
  livePollBusy = true;
  try {
    const live = await invoke<LiveTranscript>("poll_live_transcription");
    liveSegments = live.segments;
    liveError = live.error;
    renderLive();
  } catch {
    // A later poll retries; recording must not be interrupted by ASR errors.
  } finally {
    livePollBusy = false;
  }
}

function startLivePolling(): void {
  if (livePollHandle !== undefined) return;
  void pollLive();
  livePollHandle = window.setInterval(() => void pollLive(), 3_000);
}

function stopLivePolling(): void {
  window.clearInterval(livePollHandle);
  livePollHandle = undefined;
}

async function beginLive(): Promise<void> {
  try {
    const live = await invoke<LiveTranscript>("start_live_transcription", {
      provider: "minimax",
      language: view.language,
    });
    liveSegments = live.segments;
    liveError = live.error;
    setLiveIndicator(true);
    startLivePolling();
  } catch (error) {
    liveDesired = false;
    setLiveIndicator(false);
    view.errorMessage = `实时字幕启动失败：${error instanceof Error ? error.message : String(error)}`;
    applyView(app, view);
  }
}

liveButton.addEventListener("click", () => {
  if (view.status === "recording" || view.status === "paused") {
    if (livePollHandle !== undefined) {
      stopLivePolling();
      liveDesired = false;
      setLiveIndicator(false);
    } else {
      liveDesired = true;
      void beginLive();
    }
    return;
  }
  liveDesired = !liveDesired;
  setLiveIndicator(false);
});
let view: RecorderViewState = { title: "新录音", status: "idle", elapsedMs: 0, language: "auto", translation: "none", retainAudio: true, volumeLevel: 0, sessionId: null, microphoneChunks: [], systemChunks: [] };

function syncBackend(next: SessionState): void {
  backendState = next;
  view = { ...view, title: next.title, status: next.status, elapsedMs: next.elapsedMs, sessionId: next.sessionId, microphonePath: next.microphonePath, systemPath: next.systemPath, microphoneChunks: next.microphoneChunks, systemChunks: next.systemChunks, droppedBlocks: next.droppedBlocks, recovered: next.recovered, errorMessage: undefined };
  restartClock();
  applyView(app, view);
  if (next.status === "finished") {
    stopLivePolling();
    liveSegments = [];
    liveError = undefined;
    liveDesired = false;
    setLiveIndicator(false);
    currentTranscript = null;
    void loadSessionNotes();
    void loadTranscript();
  }
  renderLive();
}

async function loadSessionNotes(): Promise<void> {
  const editor = app.querySelector<HTMLTextAreaElement>(".meeting-notes");
  if (!editor) return;
  try {
    editor.value = await invoke<string>("get_session_notes");
  } catch (error) {
    const status = app.querySelector<HTMLElement>(".notes-state");
    if (status) status.textContent = `读取失败：${error instanceof Error ? error.message : String(error)}`;
  }
}

async function loadTranscript(): Promise<void> {
  const body = app.querySelector<HTMLElement>(".transcript-body");
  const status = app.querySelector<HTMLElement>(".transcript-state");
  if (!body || !status) return;
  try {
    const transcript = await invoke<Transcript | null>("get_transcript");
    if (!transcript) return;
    currentTranscript = transcript;
    renderTranscript(body, transcript);
    status.textContent = `${transcript.model} · ${transcript.segments.length} 段`;
  } catch {
    // No transcript has been generated for this session yet.
  }
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
    if (name === "stop_session") {
      stopLevelPolling();
      stopLivePolling();
      liveSegments = [];
      liveError = undefined;
      setLiveIndicator(false);
    }
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
    syncBackend(await invoke<SessionState>("start_session", { microphoneId, systemOutputId, title: view.title }));
    startLevelPolling();
    if (liveDesired) void beginLive();
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

const historyDialog = app.querySelector<HTMLDialogElement>(".history-dialog")!;
const historyList = app.querySelector<HTMLElement>(".history-list")!;
const historySearch = app.querySelector<HTMLInputElement>(".history-search")!;
async function refreshHistory(): Promise<void> {
  historyList.replaceChildren();
  try {
    const recordings = await invoke<RecordingListItem[]>("list_recordings");
    if (recordings.length === 0) {
      const empty = document.createElement("p");
      empty.className = "history-empty";
      empty.textContent = "还没有本地录音。";
      historyList.append(empty);
      return;
    }
    for (const recording of recordings) {
      const item = document.createElement("article");
      item.className = "history-item";
      item.dataset.sessionId = recording.sessionId;
      const button = document.createElement("button");
      button.className = "history-open";
      button.type = "button";
      const title = document.createElement("strong");
      title.textContent = recording.title;
      const details = document.createElement("span");
      const status = recording.status === "completed" ? "已完成" : recording.status === "recovered" ? "已恢复" : "意外中断";
      details.textContent = `${new Date(recording.startedAtEpochMs).toLocaleString("zh-CN")} · ${formatElapsed(recording.durationMs)} · ${formatBytes(recording.sizeBytes)} · ${status}`;
      const id = document.createElement("small");
      id.textContent = recording.sessionId;
      button.append(title, details, id);
      const remove = document.createElement("button");
      remove.className = "history-delete";
      remove.type = "button";
      remove.title = "移入 Windows 回收站";
      remove.textContent = "删除";
      item.append(button, remove);
      historyList.append(item);
    }
  } catch (error) {
    const failure = document.createElement("p");
    failure.className = "history-empty failure";
    failure.textContent = `无法读取历史录音：${error instanceof Error ? error.message : String(error)}`;
    historyList.append(failure);
  }
}
function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}
app.querySelector<HTMLButtonElement>(".history-button")!.addEventListener("click", () => {
  historySearch.value = "";
  historyDialog.showModal();
  void refreshHistory();
});
historySearch.addEventListener("input", () => {
  const query = historySearch.value.trim().toLocaleLowerCase("zh-CN");
  historyList.querySelectorAll<HTMLElement>(".history-item").forEach((item) => {
    item.hidden = query.length > 0 && !item.textContent?.toLocaleLowerCase("zh-CN").includes(query);
  });
});
app.querySelector<HTMLButtonElement>(".history-close")!.addEventListener("click", () => historyDialog.close());
historyList.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".history-open");
  const item = button?.closest<HTMLElement>(".history-item");
  if (!button || !item?.dataset.sessionId) return;
  button.disabled = true;
  try {
    syncBackend(await invoke<SessionState>("open_recording", { sessionId: item.dataset.sessionId }));
    historyDialog.close();
  } catch (error) {
    view.errorMessage = `无法打开历史录音：${error instanceof Error ? error.message : String(error)}`;
    applyView(app, view);
  } finally {
    button.disabled = false;
  }
});
historyList.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".history-delete");
  const item = button?.closest<HTMLElement>(".history-item");
  if (!button || !item?.dataset.sessionId) return;
  const title = item.querySelector("strong")?.textContent ?? "这条录音";
  if (!window.confirm(`确定将“${title}”移入 Windows 回收站吗？`)) return;
  button.disabled = true;
  try {
    await invoke("trash_recording", { sessionId: item.dataset.sessionId });
    syncBackend(await invoke<SessionState>("get_session_state"));
    await refreshHistory();
  } catch (error) {
    view.errorMessage = `无法删除录音：${error instanceof Error ? error.message : String(error)}`;
    applyView(app, view);
  } finally {
    button.disabled = false;
  }
});

const aiDialog = app.querySelector<HTMLDialogElement>(".ai-dialog")!;
const aiProvider = app.querySelector<HTMLSelectElement>(".ai-provider")!;
const aiModel = app.querySelector<HTMLInputElement>(".ai-model")!;
const aiKey = app.querySelector<HTMLInputElement>(".ai-key")!;
const aiResult = app.querySelector<HTMLOutputElement>(".ai-result")!;
const aiPrompt = app.querySelector<HTMLTextAreaElement>(".ai-prompt")!;
const aiConfigStatus = app.querySelector<HTMLElement>(".ai-config-status")!;
const aiClear = app.querySelector<HTMLButtonElement>(".ai-clear")!;
async function refreshAiConfiguration(): Promise<void> {
  try {
    const configurations = await invoke<AiConfiguration[]>("get_ai_configurations");
    const current = configurations.find((configuration) => configuration.provider === aiProvider.value);
    if (current) aiModel.value = current.model;
    aiConfigStatus.textContent = current?.configured ? "API Key 已安全保存" : "尚未配置 API Key";
    aiConfigStatus.className = `ai-config-status ${current?.configured ? "configured" : ""}`;
    aiClear.hidden = !current?.configured;
  } catch (error) {
    aiConfigStatus.textContent = `无法读取凭据状态：${error instanceof Error ? error.message : String(error)}`;
  }
}
app.querySelector<HTMLButtonElement>(".ai-settings")!.addEventListener("click", () => {
  aiDialog.showModal();
  void refreshAiConfiguration();
});
aiProvider.addEventListener("change", () => {
  aiModel.value = aiProvider.value === "minimax" ? "MiniMax-M3.1-Flash" : "deepseek-flash";
  aiResult.textContent = "";
  void refreshAiConfiguration();
});
aiClear.addEventListener("click", async () => {
  aiClear.disabled = true;
  try {
    await invoke("clear_ai", { provider: aiProvider.value });
    aiResult.textContent = "API Key 已从 Windows 凭据管理器清除。";
    await refreshAiConfiguration();
  } catch (error) {
    aiResult.className = "ai-result failure";
    aiResult.textContent = error instanceof Error ? error.message : String(error);
  } finally {
    aiClear.disabled = false;
  }
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
    await refreshAiConfiguration();
  } catch (error) {
    aiResult.className = "ai-result failure";
    aiResult.textContent = error instanceof Error ? error.message : String(error);
  } finally {
    button.disabled = false;
  }
});
app.querySelector<HTMLButtonElement>(".ai-generate")!.addEventListener("click", async (event) => {
  const button = event.currentTarget as HTMLButtonElement;
  if (!aiPrompt.value.trim()) {
    aiResult.className = "ai-result failure";
    aiResult.textContent = "请先输入问题。";
    return;
  }
  button.disabled = true;
  aiResult.className = "ai-result pending";
  aiResult.textContent = "AI 正在生成…";
  try {
    const response = await invoke<AiResponse>("generate_ai", { provider: aiProvider.value, prompt: aiPrompt.value });
    aiResult.className = "ai-result success";
    aiResult.textContent = response.content;
  } catch (error) {
    aiResult.className = "ai-result failure";
    aiResult.textContent = error instanceof Error ? error.message : String(error);
  } finally {
    button.disabled = false;
  }
});
const asrDialog = app.querySelector<HTMLDialogElement>(".asr-dialog")!;
const asrDialogProvider = app.querySelector<HTMLSelectElement>(".asr-dialog-provider")!;
const asrEndpoint = app.querySelector<HTMLInputElement>(".asr-endpoint")!;
const asrModel = app.querySelector<HTMLInputElement>(".asr-model")!;
const asrLanguage = app.querySelector<HTMLSelectElement>(".asr-language")!;
const asrKey = app.querySelector<HTMLInputElement>(".asr-key")!;
const asrStatus = app.querySelector<HTMLElement>(".asr-config-status")!;
const asrResult = app.querySelector<HTMLOutputElement>(".asr-result")!;
const asrDefaults: Record<string, { endpoint: string; model: string }> = {
  minimax: { endpoint: "https://api.minimax.cn/v1/speech_to_text", model: "asr-1.0" },
  silicon_flow: { endpoint: "https://api.siliconflow.cn/v1/audio/transcriptions", model: "FunAudioLLM/SenseVoiceSmall" },
};
function applyAsrDefaults(): void {
  const defaults = asrDefaults[asrDialogProvider.value];
  asrEndpoint.value = defaults.endpoint;
  asrModel.value = defaults.model;
}
async function refreshAsrConfiguration(): Promise<void> {
  try {
    const configurations = await invoke<AsrConfiguration[]>("get_asr_configurations");
    const current = configurations.find((configuration) => configuration.provider === asrDialogProvider.value);
    if (current) {
      asrEndpoint.value = current.endpoint;
      asrModel.value = current.model;
      asrLanguage.value = current.language;
      asrStatus.textContent = current.configured ? "API Key 已安全保存" : "尚未配置 API Key";
    }
    asrKey.value = "";
  } catch (error) {
    asrStatus.textContent = `无法读取配置：${error instanceof Error ? error.message : String(error)}`;
  }
}
asrDialogProvider.addEventListener("change", () => {
  applyAsrDefaults();
  void refreshAsrConfiguration();
});
app.addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".asr-settings");
  if (!button) return;
  asrResult.textContent = "";
  asrDialog.showModal();
  void refreshAsrConfiguration();
});
app.querySelector<HTMLButtonElement>(".asr-save")!.addEventListener("click", async () => {
  asrResult.className = "asr-result pending";
  asrResult.textContent = "正在保存…";
  try {
    const configuration = await invoke<AsrConfiguration>("configure_asr", {
      provider: asrDialogProvider.value,
      apiKey: asrKey.value,
      endpoint: asrEndpoint.value,
      model: asrModel.value,
      language: asrLanguage.value,
    });
    asrKey.value = "";
    asrResult.className = "asr-result success";
    asrResult.textContent = `${configuration.model} 配置已保存`;
    await refreshAsrConfiguration();
  } catch (error) {
    asrResult.className = "asr-result failure";
    asrResult.textContent = error instanceof Error ? error.message : String(error);
  }
});
app.querySelector<HTMLButtonElement>(".asr-clear")!.addEventListener("click", async () => {
  try {
    await invoke("clear_asr", { provider: asrDialogProvider.value });
    asrResult.className = "asr-result success";
    asrResult.textContent = "API Key 已清除";
    await refreshAsrConfiguration();
  } catch (error) {
    asrResult.className = "asr-result failure";
    asrResult.textContent = error instanceof Error ? error.message : String(error);
  }
});
app.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".transcribe");
  if (!button) return;
  const body = app.querySelector<HTMLElement>(".transcript-body");
  const status = app.querySelector<HTMLElement>(".transcript-state");
  const provider = app.querySelector<HTMLSelectElement>(".asr-provider")?.value ?? "minimax";
  if (!body || !status) return;
  button.disabled = true;
  status.textContent = "正在解码并混音…";
  try {
    const transcript = await invoke<Transcript>("transcribe_session", { provider, language: null });
    currentTranscript = transcript;
    renderTranscript(body, transcript);
    status.textContent = `${transcript.model} · ${transcript.segments.length} 段`;
  } catch (error) {
    status.textContent = "转写失败";
    view.errorMessage = `转写失败：${error instanceof Error ? error.message : String(error)}`;
    applyView(app, view);
  } finally {
    button.disabled = false;
  }
});
app.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".translate-transcript");
  if (!button) return;
  if (!currentTranscript) {
    view.errorMessage = "请先完成转写，再生成译文。";
    applyView(app, view);
    return;
  }
  const target = app.querySelector<HTMLSelectElement>(".translate-target")?.value ?? "zh";
  const targetLanguage = target === "zh" ? "中文" : "English";
  const provider = app.querySelector<HTMLSelectElement>(".notes-provider")?.value ?? "minimax";
  const body = app.querySelector<HTMLElement>(".transcript-body");
  const status = app.querySelector<HTMLElement>(".transcript-state");
  if (!body) return;
  button.disabled = true;
  if (status) status.textContent = "AI 正在翻译…";
  try {
    const translation = await invoke<string>("translate_transcript", { targetLanguage, provider });
    body.replaceChildren();
    const heading = document.createElement("p");
    heading.className = "transcript-translation";
    heading.textContent = `译文（${targetLanguage}）`;
    const text = document.createElement("p");
    text.textContent = translation;
    body.append(heading, text);
    if (status) status.textContent = "已翻译";
  } catch (error) {
    if (status) status.textContent = "翻译失败";
    view.errorMessage = `翻译失败：${error instanceof Error ? error.message : String(error)}`;
    applyView(app, view);
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
app.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".save-notes");
  if (!button) return;
  const editor = app.querySelector<HTMLTextAreaElement>(".meeting-notes")!;
  const status = app.querySelector<HTMLElement>(".notes-state")!;
  button.disabled = true;
  status.textContent = "保存中…";
  try {
    await invoke("save_session_notes", { content: editor.value });
    status.textContent = "已保存";
  } catch (error) {
    status.textContent = `保存失败：${error instanceof Error ? error.message : String(error)}`;
  } finally {
    button.disabled = false;
  }
});
app.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".summarize-notes");
  if (!button) return;
  const editor = app.querySelector<HTMLTextAreaElement>(".meeting-notes")!;
  const provider = app.querySelector<HTMLSelectElement>(".notes-provider")!.value;
  const status = app.querySelector<HTMLElement>(".notes-state")!;
  const source = editor.value.trim() || currentTranscript?.text.trim() || "";
  if (!source) {
    status.textContent = "请先完成转写或输入会议笔记";
    return;
  }
  button.disabled = true;
  status.textContent = "AI 正在整理…";
  const prompt = `请将以下会议文本整理为 Markdown 会议纪要。必须包含：核心结论、讨论要点、待办事项（负责人和截止时间未知时明确写“待确认”）、风险与未决问题。不要编造原文没有的信息。\n\n${source}`;
  try {
    const response = await invoke<AiResponse>("generate_ai", { provider, prompt });
    editor.value = response.content;
    await invoke("save_session_notes", { content: response.content });
    status.textContent = `已由 ${response.model} 生成并保存`;
  } catch (error) {
    status.textContent = `生成失败：${error instanceof Error ? error.message : String(error)}`;
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
  if (!cancel && view.sessionId) {
    void invoke<SessionState>("set_session_title", { title: view.title })
      .then(syncBackend)
      .catch((error) => {
        view.errorMessage = `无法保存标题：${error instanceof Error ? error.message : String(error)}`;
        applyView(app, view);
      });
  }
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

