import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  applyAudioDevices,
  applyInsights,
  applyTab,
  applyTasks,
  applyView,
  formatElapsed,
  renderApp,
  renderTranscript,
  syncTranscriptActions,
  type WorkspaceTab,
} from "./components";
import { TimelinePlayer } from "./player";
import {
  mergeSettings,
  resolveLiveProvider,
  settingsFromUi,
  viewSettings,
  withCloudProfile,
  withLocalWhisper,
} from "./settings";
import {
  applyTranscriptionProgress,
  beginTranscription,
  canCancelTranscription,
  idleTranscription,
  isTranscriptionBusy,
  requestTranscriptionCancel,
  transcriptionError,
  transcriptionFailed,
  transcriptionStatus,
  transcriptionSucceeded,
  type TranscriptionState,
} from "./transcription";
import "@awesome.me/webawesome/dist/components/button/button.js";
import type { AiConfiguration, AiResponse, AppSettings, AsrConfiguration, AudioDeviceSnapshot, AudioHealth, AudioLevels, CleanupResult, ExportResult, JobProgress, LibraryStats, LiveTranscript, LocalWhisperStatus, PlaybackTracks, RecorderViewState, RecordingListItem, SessionState, Transcript, TranscriptSegment, WhisperDownloadProgress } from "./types";
import "./styles.css";
import "./workspace-v2.css";

// Playwright runs the real UI in Chromium; install the in-memory Tauri bridge
// before any command is invoked.
if (import.meta.env.VITE_E2E === "1") {
  const { installTauriMock } = await import("./e2e/mock-tauri");
  installTauriMock();
}

const appElement = document.querySelector<HTMLElement>("#app");
if (!appElement) throw new Error("Missing application root");
const app: HTMLElement = appElement;
app.innerHTML = renderApp();

let backendState: SessionState;
let pending = false;
let tickHandle: number | undefined;
let levelHandle: number | undefined;
let healthHandle: number | undefined;
let currentTranscript: Transcript | null = null;
let currentTranslation: { code: string; content: string } | null = null;
let transcription: TranscriptionState = idleTranscription;
let playbackPlayer: TimelinePlayer | null = null;
let playbackRequest = 0;
let activeTranscriptLine: HTMLElement | null = null;
let liveDesired = false;
let livePollHandle: number | undefined;
let livePollBusy = false;
let livePendingChunks = 0;
let liveProcessedChunks = 0;
let livePartialText = "";
let liveSegments: TranscriptSegment[] = [];
let liveError: string | undefined;
let liveTranslations: string[] = [];
let liveTranslatedCount = 0;
let liveTranslationBusy = false;
let liveTranslationError: string | undefined;
let liveTranslationRetryAfter = 0;
let liveTranslationGeneration = 0;
let liveAiProvider: AiResponse["provider"] = "minimax";
let sessionNotes = "";
let activeTab: WorkspaceTab = "transcript";
let liveAutoFollow = true;
let liveUnread = 0;
let lastLiveSegmentCount = 0;
let chatBusy = false;
let chatMessages: Array<{ role: "user" | "assistant"; text: string; pending?: boolean }> = [];
let settings: AppSettings = {
  microphoneId: null,
  systemOutputId: null,
  language: "auto",
  translation: "none",
  retainAudio: true,
  liveCaptions: false,
  asrProvider: "minimax",
  asrProfiles: {},
  localWhisperModel: "small",
  whisperHotwords: "",
  autoCleanupDays: 0,
  storageQuotaMb: 0,
};

function applySettings(next: AppSettings): void {
  settings = mergeSettings(settings, next);
  const snapshot = viewSettings(settings);
  view.language = snapshot.language;
  view.translation = snapshot.translation;
  view.retainAudio = snapshot.retainAudio;
  liveDesired = snapshot.liveCaptions;
  const selects = app.querySelectorAll<HTMLSelectElement>("select[data-option]");
  selects.forEach((select) => {
    if (select.dataset.option === "language") select.value = settings.language;
    if (select.dataset.option === "translation") select.value = settings.translation;
    if (select.dataset.option === "audio") select.value = settings.retainAudio ? "retain" : "discard";
  });
  const autoCleanup = app.querySelector<HTMLSelectElement>(".auto-cleanup-days");
  if (autoCleanup) autoCleanup.value = String(settings.autoCleanupDays);
  const quota = app.querySelector<HTMLSelectElement>(".storage-quota");
  if (quota) quota.value = String(settings.storageQuotaMb);
  syncAsrProviderSelect();
  setLiveIndicator(false);
}

function syncAsrProviderSelect(): void {
  const select = app.querySelector<HTMLSelectElement>(".asr-provider");
  if (!select) return;
  if (Array.from(select.options).some((option) => option.value === settings.asrProvider)) {
    select.value = settings.asrProvider;
  }
}

async function persistSettings(): Promise<void> {
  const microphone = app.querySelector<HTMLSelectElement>(".microphone-select");
  const system = app.querySelector<HTMLSelectElement>(".system-output-select");
  settings = settingsFromUi(settings, {
    microphoneId: microphone?.value || null,
    systemOutputId: system?.value || null,
    language: view.language,
    translation: view.translation,
    retainAudio: view.retainAudio,
    liveCaptions: liveDesired,
    autoCleanupDays: Number(app.querySelector<HTMLSelectElement>(".auto-cleanup-days")?.value ?? 0),
    storageQuotaMb: Number(app.querySelector<HTMLSelectElement>(".storage-quota")?.value ?? 0),
  });
  try {
    const saved = await invoke<AppSettings>("save_settings", { settings });
    settings = mergeSettings(settings, saved);
  } catch (error) {
    view.errorMessage = `无法保存设置：${error instanceof Error ? error.message : String(error)}`;
    applyView(app, view);
  }
}

function applySavedDevices(): void {
  const microphone = app.querySelector<HTMLSelectElement>(".microphone-select");
  const system = app.querySelector<HTMLSelectElement>(".system-output-select");
  if (microphone && settings.microphoneId) {
    const saved = Array.from(microphone.options).find((option) => option.value === settings.microphoneId);
    if (saved) {
      microphone.value = saved.value;
      microphone.dispatchEvent(new Event("change"));
    }
  }
  if (system && settings.systemOutputId) {
    const saved = Array.from(system.options).find((option) => option.value === settings.systemOutputId);
    if (saved) {
      system.value = saved.value;
      system.dispatchEvent(new Event("change"));
    }
  }
}

const liveButton = app.querySelector<HTMLButtonElement>(".live-caption")!;
function setLiveIndicator(active: boolean): void {
  liveButton.classList.toggle("active", active);
  const translated = settings.translation === "none" ? "" : ` · ${translationLabel(settings.translation)}`;
  liveButton.querySelector<HTMLElement>(".live-state")!.textContent = active
    ? `开${translated}`
    : (liveDesired ? `下次开启${translated}` : "关");
}

function renderLive(): void {
  const stream = app.querySelector<HTMLElement>(".transcript-panel .transcript-stream");
  if (!stream) return;
  if (view.status !== "recording" && view.status !== "paused") return;
  const recognition = app.querySelector<HTMLElement>(".recognition-status");
  renderLiveTranslationSummary();
  if (liveSegments.length === 0 && !livePartialText) {
    if (liveError) {
      stream.innerHTML = `<p class="live-error">实时字幕暂不可用：${escapeForHtml(liveError)}</p>`;
    } else {
      const placeholder = stream.querySelector<HTMLElement>(".transcript-placeholder");
      if (placeholder) {
        placeholder.querySelector("h3")!.textContent = livePendingChunks > 0
          ? `正在识别 ${livePendingChunks} 个音频分片`
          : (view.status === "paused" ? "转录已暂停" : "正在聆听…");
        placeholder.querySelector("p")!.textContent = livePendingChunks > 0 && liveProcessedChunks > 0
          ? `已完成 ${liveProcessedChunks} 个分片，正在等待最新结果。`
          : "实时字幕将在识别到语音后显示；开启 AI 同传可同步生成译文。";
      }
    }
    if (recognition) recognition.hidden = true;
    lastLiveSegmentCount = 0;
    liveUnread = 0;
    updateFollowUi();
    updateInsights();
    syncTranscriptActions(app);
    return;
  }
  const segments = livePartialText
    ? [...liveSegments, { startMs: liveSegments.at(-1)?.endMs ?? 0, endMs: liveSegments.at(-1)?.endMs ?? 0, text: livePartialText }]
    : liveSegments;
  if (segments.length > lastLiveSegmentCount && !liveAutoFollow) {
    liveUnread += segments.length - lastLiveSegmentCount;
  }
  lastLiveSegmentCount = segments.length;
  if (liveAutoFollow) liveUnread = 0;
  renderTranscript(stream, {
    provider: "live",
    model: "",
    language: view.language,
    durationMs: 0,
    text: "",
    segments,
    createdAtEpochMs: 0,
  }, undefined, { partial: Boolean(livePartialText) });
  stream.querySelectorAll<HTMLElement>(".transcript-line").forEach((line, index) => {
    const translation = liveTranslations[index];
    if (!translation) return;
    const translated = document.createElement("span");
    translated.className = "live-translation-text";
    translated.lang = settings.translation === "zh-CN" ? "zh-CN" : "en";
    translated.textContent = translation;
    line.append(translated);
  });
  if (settings.translation !== "none") {
    const status = document.createElement("p");
    status.className = liveTranslationError ? "live-error live-translation-status" : "live-translation-status";
    status.textContent = liveTranslationError
      ? `AI 同传将在稍后重试：${liveTranslationError}`
      : liveTranslationBusy
        ? `AI 正在翻译为${translationLabel(settings.translation)} · ${liveTranslatedCount}/${liveSegments.length} 段`
        : `AI 同传 ${translationLabel(settings.translation)} · 已翻译 ${liveTranslatedCount}/${liveSegments.length} 段`;
    stream.append(status);
  }
  if (liveError) {
    const failure = document.createElement("p");
    failure.className = "live-error";
    failure.textContent = `实时字幕错误：${liveError}`;
    stream.append(failure);
  }
  if (recognition) recognition.hidden = false;
  updateInsights();
  syncTranscriptActions(app);
  updateFollowUi();
  if (liveAutoFollow) {
    const scroller = app.querySelector<HTMLElement>(".transcript-scroll");
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
  }
}

function updateFollowUi(): void {
  const button = app.querySelector<HTMLButtonElement>(".follow-latest");
  if (!button) return;
  button.hidden = liveAutoFollow;
  const count = button.querySelector<HTMLElement>(".follow-count");
  if (count) {
    count.hidden = liveUnread === 0;
    count.textContent = String(liveUnread);
  }
}

function renderLiveTranslationSummary(): void {
  const list = app.querySelector<HTMLElement>(".live-translation-list");
  if (!list) return;
  if (liveSegments.length === 0) return;
  list.innerHTML = liveSegments.map((segment, index) => `
    <article class="live-translation-item">
      <header><span>${escapeForHtml(speakerName(segment.speaker, index))}</span><time>${formatElapsed(segment.startMs)}</time></header>
      <p>${escapeForHtml(segment.text)}</p>
      <div class="translation-text">${escapeForHtml(liveTranslations[index] ?? "等待 AI 译文…")}</div>
    </article>`).join("");
}

function escapeForHtml(value: string): string {
  const replacements: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
  return value.replace(/[&<>"']/g, (character) => replacements[character]);
}

function speakerName(speaker: string | undefined, index: number): string {
  if (!speaker) return `发言人 ${String.fromCharCode(65 + (index % 4))}`;
  const match = /^S(\d+)$/i.exec(speaker);
  return match ? `发言人 ${String.fromCharCode(64 + Number(match[1]))}` : speaker;
}

function translationLabel(value: string): string {
  if (value === "zh-CN" || value === "zh") return "中文";
  if (value === "en-US" || value === "en") return "English";
  return value;
}

async function translateNewLiveSegments(): Promise<void> {
  if (settings.translation === "none" || liveTranslationBusy || Date.now() < liveTranslationRetryAfter) return;
  if (liveSegments.length <= liveTranslatedCount) return;
  const start = liveTranslatedCount;
  const pendingSegments = liveSegments.slice(start, start + 32);
  const generation = liveTranslationGeneration;
  const target = settings.translation;
  liveTranslationBusy = true;
  liveTranslationError = undefined;
  renderLive();
  try {
    const configurations = await invoke<AiConfiguration[]>("get_ai_configurations");
    const configured = configurations.find((item) => item.configured);
    if (!configured) throw new Error("请先在“AI 助手”中配置 MiniMax 或 DeepSeek API Key");
    liveAiProvider = configured.provider;
    const translated = await invoke<string[]>("translate_live_segments", {
      segments: pendingSegments.map((segment) => segment.text),
      targetLanguage: translationLabel(target),
      provider: liveAiProvider,
    });
    if (generation !== liveTranslationGeneration || target !== settings.translation) return;
    liveTranslations.splice(start, translated.length, ...translated);
    liveTranslatedCount = start + translated.length;
    liveTranslationRetryAfter = 0;
  } catch (error) {
    liveTranslationError = error instanceof Error ? error.message : String(error);
    liveTranslationRetryAfter = Date.now() + 15_000;
  } finally {
    liveTranslationBusy = false;
    renderLive();
    if (liveSegments.length > liveTranslatedCount && !liveTranslationError) void translateNewLiveSegments();
  }
}

async function pollLive(): Promise<void> {
  if (livePollBusy) return;
  livePollBusy = true;
  try {
    const live = await invoke<LiveTranscript>("poll_live_transcription");
    liveSegments = live.segments;
    livePendingChunks = live.pendingChunks;
    liveProcessedChunks = live.processedChunks;
    livePartialText = live.partialText;
    liveError = live.error;
    renderLive();
    void translateNewLiveSegments();
  } catch {
    // A later poll retries; recording must not be interrupted by ASR errors.
  } finally {
    livePollBusy = false;
  }
}

function startLivePolling(): void {
  if (livePollHandle !== undefined) return;
  void pollLive();
  livePollHandle = window.setInterval(() => void pollLive(), 250);
}

function stopLivePolling(): void {
  window.clearInterval(livePollHandle);
  livePollHandle = undefined;
}

async function beginLive(): Promise<void> {
  try {
    const [configurations, aiConfigurations] = await Promise.all([
      invoke<AsrConfiguration[]>("get_asr_configurations"),
      invoke<AiConfiguration[]>("get_ai_configurations"),
    ]);
    const provider = resolveLiveProvider(settings, configurations);
    liveAiProvider = aiConfigurations.find((item) => item.configured)?.provider ?? "minimax";
    liveTranslations = [];
    liveTranslatedCount = 0;
    liveTranslationError = undefined;
    liveTranslationRetryAfter = 0;
    liveTranslationGeneration += 1;
    const live = await invoke<LiveTranscript>("start_live_transcription", {
      provider,
      language: view.language,
    });
    liveSegments = live.segments;
    livePendingChunks = live.pendingChunks;
    liveProcessedChunks = live.processedChunks;
    livePartialText = live.partialText;
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
      void invoke("stop_live_transcription");
      liveDesired = false;
      setLiveIndicator(false);
    } else {
      liveDesired = true;
      void beginLive();
    }
    void persistSettings();
    return;
  }
  liveDesired = !liveDesired;
  setLiveIndicator(false);
  void persistSettings();
});
let view: RecorderViewState = { title: "新录音", status: "idle", elapsedMs: 0, language: "auto", translation: "none", retainAudio: true, volumeLevel: 0, sessionId: null, microphoneChunks: [], systemChunks: [] };

function syncBackend(next: SessionState): void {
  const sessionChanged = backendState?.sessionId !== next.sessionId;
  backendState = next;
  view = { ...view, title: next.title, status: next.status, elapsedMs: next.elapsedMs, sessionId: next.sessionId, microphonePath: next.microphonePath, systemPath: next.systemPath, microphoneChunks: next.microphoneChunks, systemChunks: next.systemChunks, droppedBlocks: next.droppedBlocks, recovered: next.recovered, errorMessage: undefined };
  restartClock();
  applyView(app, view);
  syncAsrProviderSelect();
  if (sessionChanged || next.status !== "finished") {
    transcription = idleTranscription;
  }
  renderTranscriptionState();
  if (next.status === "finished") {
    stopLivePolling();
    stopHealthPolling();
    liveSegments = [];
    liveError = undefined;
    liveTranslations = [];
    liveTranslatedCount = 0;
    liveTranslationError = undefined;
    liveTranslationGeneration += 1;
    liveDesired = false;
    setLiveIndicator(false);
    currentTranscript = null;
    activeTranscriptLine = null;
    const revealPath = next.microphonePath ?? next.systemPath ?? "";
    app.querySelectorAll<HTMLButtonElement>(".reveal-recording").forEach((button) => {
      button.dataset.path = revealPath;
      button.hidden = revealPath.length === 0;
    });
    void ensurePlayback();
    void loadSessionNotes();
    void loadTranscript();
  } else {
    playbackRequest += 1;
    playbackPlayer?.dispose();
    playbackPlayer = null;
    activeTranscriptLine = null;
    resetPlaybackUi();
  }
  updateInsights();
  renderChatThreads();
  renderLive();
}

function resetPlaybackUi(): void {
  const toggle = app.querySelector<HTMLButtonElement>(".playback-toggle");
  const seek = app.querySelector<HTMLInputElement>(".playback-seek");
  const rate = app.querySelector<HTMLSelectElement>(".playback-rate");
  const current = app.querySelector<HTMLElement>(".playback-current");
  const total = app.querySelector<HTMLElement>(".playback-total");
  if (toggle) {
    toggle.disabled = true;
    toggle.classList.remove("playing");
    toggle.setAttribute("aria-label", "播放录音");
  }
  if (seek) {
    seek.disabled = true;
    seek.value = "0";
  }
  if (rate) rate.disabled = true;
  if (current) current.textContent = "00:00";
  if (total) {
    total.textContent = view.status === "recording" || view.status === "paused" || view.status === "stopping"
      ? formatElapsed(view.elapsedMs)
      : "00:00";
  }
}

/// Loads (or reloads) the unified playback timeline for the finished session.
async function ensurePlayback(): Promise<void> {
  const request = ++playbackRequest;
  const container = app.querySelector<HTMLElement>(".playback");
  if (view.status !== "finished" || !container) {
    playbackPlayer?.dispose();
    playbackPlayer = null;
    return;
  }
  if (playbackPlayer && playbackPlayer.root === container) return;
  playbackPlayer?.dispose();
  playbackPlayer = null;
  try {
    const tracks = await invoke<PlaybackTracks>("get_session_playback");
    if (request !== playbackRequest) return;
    if (view.status !== "finished") return;
    if (app.querySelector<HTMLElement>(".playback") !== container) return;
    playbackPlayer = new TimelinePlayer(container, tracks, {
      onPosition: (positionMs) => highlightTranscript(positionMs),
    });
  } catch (error) {
    if (request !== playbackRequest) return;
    const hint = container.querySelector<HTMLElement>(".playback-hint");
    if (hint) hint.textContent = `无法加载回放时间轴：${error instanceof Error ? error.message : String(error)}`;
  }
}

/// Highlights the subtitle line that contains `positionMs` and keeps it in
/// view without fighting the user's manual scrolling.
function highlightTranscript(positionMs: number): void {
  const body = app.querySelector<HTMLElement>(".transcript-body");
  if (!body) return;
  let active: HTMLElement | null = null;
  for (const line of body.querySelectorAll<HTMLElement>(".transcript-line[data-start-ms]")) {
    const start = Number(line.dataset.startMs ?? 0);
    if (start <= positionMs + 400) active = line;
  }
  if (active === activeTranscriptLine) return;
  activeTranscriptLine?.classList.remove("active");
  active?.classList.add("active");
  activeTranscriptLine = active;
  active?.scrollIntoView({ block: "nearest" });
}

function seekFromTranscript(target: HTMLElement): void {
  const line = target.closest<HTMLElement>(".transcript-line[data-start-ms]");
  if (!line || !playbackPlayer) return;
  const start = Number(line.dataset.startMs);
  if (!Number.isFinite(start)) return;
  playbackPlayer.seek(start);
  highlightTranscript(start);
}

async function loadSessionNotes(): Promise<void> {
  try {
    sessionNotes = await invoke<string>("get_session_notes");
    const editor = app.querySelector<HTMLTextAreaElement>(".meeting-notes");
    if (editor) editor.value = sessionNotes;
    updateInsights();
    applyTasks(app, sessionNotes);
    renderTranslationPanel();
  } catch (error) {
    const status = app.querySelector<HTMLElement>(".notes-state");
    if (status) status.textContent = `读取失败：${error instanceof Error ? error.message : String(error)}`;
  }
}

function updateInsights(): void {
  const segments = currentTranscript?.segments ?? liveSegments;
  applyInsights(app, {
    title: view.title,
    status: view.status,
    segments,
    notes: sessionNotes,
  });
  applyTasks(app, sessionNotes);
  const topicStatus = app.querySelector<HTMLElement>(".insight-panel");
  if (topicStatus) topicStatus.classList.toggle("live", view.status === "recording");
}

function renderTranslationPanel(): void {
  const result = app.querySelector<HTMLElement>(".translation-content");
  if (!result) return;
  if (!currentTranslation || !currentTranslation.content.trim()) return;
  result.innerHTML = `<p>${escapeForHtml(currentTranslation.content)}</p>`;
  const original = app.querySelector<HTMLElement>(".translation-original");
  if (!original || !currentTranscript) return;
  original.replaceChildren();
  for (const segment of currentTranscript.segments) {
    const line = document.createElement("p");
    line.className = "translation-original-line";
    line.innerHTML = `<time>${formatElapsed(segment.startMs)}</time><span>${escapeForHtml(segment.text)}</span>`;
    original.append(line);
  }
  if (currentTranscript.segments.length === 0) {
    original.textContent = currentTranscript.text;
  }
}

function selectedTranslationCode(): string {
  return app.querySelector<HTMLSelectElement>(".translate-target")?.value ?? "zh";
}

function renderTranscriptBody(): void {
  const body = app.querySelector<HTMLElement>(".transcript-body");
  if (!body || !currentTranscript) return;
  const code = selectedTranslationCode();
  const translation =
    currentTranslation && currentTranslation.code === code
      ? { label: translationLabel(code), content: currentTranslation.content }
      : undefined;
  renderTranscript(body, currentTranscript, translation);
}

function setTranscriptStatus(suffix?: string): void {
  const status = app.querySelector<HTMLElement>(".transcript-state");
  if (!status || !currentTranscript) return;
  const base = `${currentTranscript.model} · ${currentTranscript.segments.length} 段`;
  status.textContent = suffix ? `${base} · ${suffix}` : base;
}

/// Loads the saved translation for the currently selected target language.
async function loadTranslation(): Promise<void> {
  if (!currentTranscript) {
    currentTranslation = null;
    return;
  }
  const code = selectedTranslationCode();
  try {
    const content = await invoke<string | null>("get_translation", { targetLanguage: code });
    currentTranslation = content && content.trim() ? { code, content } : null;
  } catch {
    currentTranslation = null;
  }
  renderTranscriptBody();
  setTranscriptStatus(currentTranslation ? "已翻译" : undefined);
  renderTranslationPanel();
  updateInsights();
}

async function loadTranscript(): Promise<void> {
  const status = app.querySelector<HTMLElement>(".transcript-state");
  if (!status) return;
  currentTranscript = null;
  currentTranslation = null;
  try {
    const transcript = await invoke<Transcript | null>("get_transcript");
    if (!transcript) return;
    currentTranscript = transcript;
    renderTranscriptBody();
    setTranscriptStatus();
    syncTranscriptActions(app);
    updateInsights();
    await loadTranslation();
  } catch {
    // No transcript has been generated for this session yet.
  }
}

/// Applies backend job progress to the matching status line.
function applyJobProgress(progress: JobProgress): void {
  if (progress.stage === "transcribe") {
    transcription = applyTranscriptionProgress(transcription, progress);
    renderTranscriptionState();
    return;
  }
  const selector = progress.stage === "summarize" ? ".notes-state" : ".transcript-state";
  const status = app.querySelector<HTMLElement>(selector);
  if (!status) return;
  const label = progress.stage === "translate" ? "AI 正在翻译" : "AI 正在整理";
  const counter = progress.total > 0 ? ` ${progress.done}/${progress.total}` : "";
  status.textContent = `${label}${counter} 段`;
}

/// Reflects the transcription state machine in the workbench controls.
function renderTranscriptionState(): void {
  const button = app.querySelector<HTMLButtonElement>(".transcribe");
  const cancel = app.querySelector<HTMLButtonElement>(".cancel-transcribe");
  const status = app.querySelector<HTMLElement>(".transcript-state");
  if (button) button.disabled = isTranscriptionBusy(transcription);
  if (cancel) {
    cancel.hidden = !canCancelTranscription(transcription);
    cancel.disabled = transcription.phase === "cancelling";
  }
  const text = transcriptionStatus(transcription);
  if (status && text !== null) status.textContent = text;
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
      stopHealthPolling();
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
    activeTab = "transcript";
    liveAutoFollow = true;
    liveUnread = 0;
    lastLiveSegmentCount = 0;
    applyTab(app, "transcript");
    syncBackend(await invoke<SessionState>("start_session", { microphoneId, systemOutputId, title: view.title }));
    startLevelPolling();
    startHealthPolling();
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
      updateLevelMeter(".microphone-level", levels.microphone);
      updateLevelMeter(".system-level", levels.system);
      app.querySelector<HTMLElement>(".waveform")!.title = `麦克风 ${Math.round(levels.microphone * 100)}% · 系统声音 ${Math.round(levels.system * 100)}%`;
    } catch {
      app.style.setProperty("--audio-level", "0");
      updateLevelMeter(".microphone-level", 0);
      updateLevelMeter(".system-level", 0);
    }
  }, 100);
}

function updateLevelMeter(selector: string, value: number): void {
  const channel = app.querySelector<HTMLElement>(selector);
  if (!channel) return;
  const normalized = Math.min(1, Math.max(0, value));
  const decibels = normalized > 0 ? Math.max(-60, 20 * Math.log10(normalized)) : -60;
  channel.style.setProperty("--level", normalized.toFixed(3));
  channel.classList.toggle("clipping", normalized >= .98);
  const meter = channel.querySelector<HTMLElement>(".level-track");
  meter?.setAttribute("aria-valuenow", decibels.toFixed(1));
  const readout = channel.querySelector<HTMLOutputElement>(".level-readout");
  if (readout) readout.value = `${decibels <= -59.5 ? "−60" : decibels.toFixed(1)} dB`;
}

function stopLevelPolling(): void {
  window.clearInterval(levelHandle);
  app.style.setProperty("--audio-level", "0");
  updateLevelMeter(".microphone-level", 0);
  updateLevelMeter(".system-level", 0);
}

function applyHealth(health: AudioHealth): void {
  const warning = app.querySelector<HTMLElement>(".device-warning");
  if (!warning) return;
  const message = health.microphoneError
    ? "麦克风连接中断，正在自动重连…"
    : health.systemError
      ? "系统声音连接中断，正在自动重连…"
      : health.stalled
        ? "麦克风音频流已停止，正在自动重连…"
        : "";
  warning.hidden = message.length === 0;
  warning.textContent = message;
}

function startHealthPolling(): void {
  stopHealthPolling();
  const poll = async (): Promise<void> => {
    if (view.status !== "recording" && view.status !== "paused") return;
    try {
      applyHealth(await invoke<AudioHealth>("get_audio_health"));
    } catch {
      // The health endpoint is best-effort; recording continues regardless.
    }
  };
  void poll();
  healthHandle = window.setInterval(() => void poll(), 3_000);
}

function stopHealthPolling(): void {
  window.clearInterval(healthHandle);
  healthHandle = undefined;
  const warning = app.querySelector<HTMLElement>(".device-warning");
  if (warning) {
    warning.hidden = true;
    warning.textContent = "";
  }
}

async function refreshAudioDevices(): Promise<void> {
  const button = app.querySelector<HTMLElement & { disabled: boolean }>(".refresh-devices")!;
  button.disabled = true;
  button.classList.add("is-busy");
  button.title = "正在检测音频设备…";
  button.setAttribute("aria-label", "正在检测音频设备");
  try {
    applyAudioDevices(app, await invoke<AudioDeviceSnapshot>("list_audio_devices"));
  } catch (error) {
    view.errorMessage = `无法检测音频设备：${error instanceof Error ? error.message : String(error)}`;
    applyView(app, view);
  } finally {
    button.disabled = false;
    button.classList.remove("is-busy");
    button.title = "重新检测音频设备";
    button.setAttribute("aria-label", "重新检测音频设备");
  }
}

app.querySelector<HTMLElement>(".start-button")!.addEventListener("click", () => void startRecording());
app.querySelector<HTMLElement>(".pause-button")!.addEventListener("click", () => void command(view.status === "paused" ? "resume_session" : "pause_session"));
app.querySelector<HTMLElement>(".stop-button")!.addEventListener("click", () => void command("stop_session"));
app.querySelector<HTMLElement>(".refresh-devices")!.addEventListener("click", () => void refreshAudioDevices());

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
      const status = recording.status === "completed" ? "已完成" : recording.status === "recovered" ? "已恢复" : recording.status === "transcript_only" ? "仅转写" : "意外中断";
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
const libraryStats = app.querySelector<HTMLElement>(".library-stats")!;
async function refreshLibraryStats(): Promise<void> {
  try {
    const stats = await invoke<LibraryStats>("get_library_stats");
    const transcriptOnly = stats.transcriptOnlyCount > 0 ? ` · 仅转写 ${stats.transcriptOnlyCount} 条` : "";
    libraryStats.textContent = `${stats.sessionCount} 条录音 · 音频 ${formatBytes(stats.audioBytes)} · 总计 ${formatBytes(stats.totalBytes)}${transcriptOnly}`;
  } catch (error) {
    libraryStats.textContent = `无法统计占用：${error instanceof Error ? error.message : String(error)}`;
  }
}
app.querySelector<HTMLElement>(".history-button")!.addEventListener("click", () => {
  historySearch.value = "";
  historyDialog.showModal();
  void refreshHistory();
  void refreshLibraryStats();
});
app.querySelector<HTMLButtonElement>(".cleanup-audio")!.addEventListener("click", async () => {
  const days = Number(app.querySelector<HTMLSelectElement>(".cleanup-days")!.value);
  if (!window.confirm(`将删除 ${days} 天前、且已有转写的录音音频，转写与笔记会保留。确定继续吗？`)) return;
  const button = app.querySelector<HTMLButtonElement>(".cleanup-audio")!;
  button.disabled = true;
  libraryStats.textContent = "正在清理…";
  try {
    const result = await invoke<CleanupResult>("cleanup_audio_older_than", { days });
    libraryStats.textContent = `已清理 ${result.sessions} 条录音的音频，释放 ${formatBytes(result.bytesFreed)}`;
    await refreshHistory();
    syncBackend(await invoke<SessionState>("get_session_state"));
    await refreshLibraryStats();
  } catch (error) {
    libraryStats.textContent = `清理失败：${error instanceof Error ? error.message : String(error)}`;
  } finally {
    button.disabled = false;
  }
});
app.querySelector<HTMLButtonElement>(".open-logs")!.addEventListener("click", async () => {
  const state = app.querySelector<HTMLElement>(".diagnostics-state")!;
  try {
    await invoke("open_log_directory");
    state.textContent = "已打开日志目录";
  } catch (error) {
    state.textContent = `无法打开日志目录：${error instanceof Error ? error.message : String(error)}`;
  }
});
app.querySelector<HTMLButtonElement>(".export-diagnostics")!.addEventListener("click", async () => {
  const button = app.querySelector<HTMLButtonElement>(".export-diagnostics")!;
  const state = app.querySelector<HTMLElement>(".diagnostics-state")!;
  button.disabled = true;
  state.textContent = "正在生成诊断包…";
  try {
    const path = await invoke<string>("export_diagnostics");
    state.textContent = `诊断包已导出：${path}`;
  } catch (error) {
    state.textContent = `导出失败：${error instanceof Error ? error.message : String(error)}`;
  } finally {
    button.disabled = false;
  }
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
async function refreshAiStatus(): Promise<void> {
  try {
    const configurations = await invoke<AiConfiguration[]>("get_ai_configurations");
    const configured = configurations.find((configuration) => configuration.configured);
    const dot = app.querySelector<HTMLElement>(".config-status-dot");
    const summary = app.querySelector<HTMLElement>(".ai-model-summary");
    dot?.classList.toggle("configured", Boolean(configured));
    if (dot) dot.title = configured ? `${configured.provider} 已配置` : "尚未配置 AI 服务";
    if (summary) {
      summary.textContent = configured
        ? `${configured.provider === "minimax" ? "MiniMax" : "DeepSeek"} · ${configured.model}`
        : "尚未配置 AI 服务";
    }
  } catch {
    // Status is cosmetic; configuration failures are surfaced in the dialog.
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
    await refreshAiStatus();
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
  openai_realtime: { endpoint: "wss://api.openai.com/v1/realtime?intent=transcription", model: "gpt-live-transcribe" },
  minimax: { endpoint: "https://api.minimax.cn/v1/speech_to_text", model: "asr-1.0" },
  silicon_flow: { endpoint: "https://api.siliconflow.cn/v1/audio/transcriptions", model: "FunAudioLLM/SenseVoiceSmall" },
};
function applyAsrDefaults(): void {
  const defaults = asrDefaults[asrDialogProvider.value];
  if (!defaults) return;
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
  syncAsrMode();
  applyAsrDefaults();
  void refreshAsrConfiguration();
});
function openAsrSettings(): void {
  asrResult.textContent = "";
  asrDialogProvider.value = settings.asrProvider;
  syncAsrMode();
  applyAsrDefaults();
  asrDialog.showModal();
  void refreshAsrConfiguration();
}

app.addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".asr-settings");
  if (!button) return;
  openAsrSettings();
});
app.querySelector<HTMLButtonElement>(".asr-save")!.addEventListener("click", async () => {
  if (asrDialogProvider.value === "local_whisper") {
    asrResult.className = "asr-result pending";
    asrResult.textContent = "正在保存…";
    try {
      settings = withLocalWhisper(
        settings,
        localModelSelect.value,
        asrHotwords.value,
      );
      await persistSettings();
      asrResult.className = "asr-result success";
      asrResult.textContent = "本地 Whisper 配置已保存";
      await refreshLocalWhisper();
    } catch (error) {
      asrResult.className = "asr-result failure";
      asrResult.textContent = error instanceof Error ? error.message : String(error);
    }
    return;
  }
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
    settings = withCloudProfile(settings, configuration);
    // The hotwords travel with the cloud request as well.
    settings = mergeSettings(settings, { whisperHotwords: asrHotwords.value });
    await persistSettings();
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
const asrRemote = app.querySelector<HTMLElement>(".asr-remote")!;
const localPanel = app.querySelector<HTMLElement>(".local-whisper")!;
const localModelSelect = app.querySelector<HTMLSelectElement>(".local-model")!;
const asrHotwords = app.querySelector<HTMLTextAreaElement>(".asr-hotwords")!;
const localStatus = app.querySelector<HTMLElement>(".local-whisper-status")!;
const localDownloadEngine = app.querySelector<HTMLButtonElement>(".local-download-engine")!;
const localDownloadModel = app.querySelector<HTMLButtonElement>(".local-download-model")!;
const localDeleteModel = app.querySelector<HTMLButtonElement>(".local-delete-model")!;
const localDownloadCuda = app.querySelector<HTMLButtonElement>(".local-download-engine-cuda")!;
let localWhisperStatus: LocalWhisperStatus | null = null;
let localWhisperBusy = false;

function syncAsrMode(): void {
  const local = asrDialogProvider.value === "local_whisper";
  asrRemote.hidden = local;
  localPanel.hidden = !local;
  // The hotwords apply to every provider, so the field is always shown.
  asrHotwords.value = settings.whisperHotwords;
  if (local) {
    void refreshLocalWhisper();
  }
}

function renderLocalWhisper(): void {
  const status = localWhisperStatus;
  if (!status) {
    localStatus.textContent = "正在读取本地引擎状态…";
    return;
  }
  if (localModelSelect.options.length !== status.models.length) {
    localModelSelect.replaceChildren(
      ...status.models.map(
        (model) => new Option(`${model.label} · ${formatBytes(model.sizeBytes)}`, model.id),
      ),
    );
  }
  localModelSelect.value = status.model;
  const current = status.models.find((model) => model.id === status.model);
  const cpu = status.engines.find((engine) => engine.id === "cpu");
  const cuda = status.engines.find((engine) => engine.id === "cuda");
  const engineLabel = status.engine === "cuda" ? "CUDA" : status.engine === "cpu" ? "CPU" : "未安装";
  localStatus.textContent = [
    status.gpuName ? `GPU：${status.gpuName}` : "未检测到 NVIDIA GPU",
    status.engineInstalled ? `引擎：${engineLabel}（${formatBytes(status.engineBytes)}）` : "引擎未安装",
    current ? `${current.label}：${current.installed ? "已下载" : "未下载"}` : "",
    `本地占用 ${formatBytes(status.totalBytes)}`,
  ]
    .filter(Boolean)
    .join(" · ");
  localDownloadEngine.hidden = !cpu || cpu.installed;
  localDownloadEngine.disabled = localWhisperBusy;
  localDownloadCuda.hidden = !cuda || cuda.installed || !status.gpuName;
  localDownloadCuda.disabled = localWhisperBusy;
  localDownloadModel.hidden = !current || current.installed;
  localDownloadModel.disabled = localWhisperBusy || !status.engineInstalled;
  localDownloadModel.textContent = status.engineInstalled ? "下载模型" : "请先下载引擎";
  localDeleteModel.hidden = !current?.installed;
  localDeleteModel.disabled = localWhisperBusy;
  localModelSelect.disabled = localWhisperBusy;
}

async function refreshLocalWhisper(): Promise<void> {
  try {
    localWhisperStatus = await invoke<LocalWhisperStatus>("get_local_whisper_status");
    renderLocalWhisper();
  } catch (error) {
    localStatus.textContent = `无法读取本地引擎状态：${error instanceof Error ? error.message : String(error)}`;
  }
}

localModelSelect.addEventListener("change", () => {
  settings.localWhisperModel = localModelSelect.value;
  void persistSettings().then(() => refreshLocalWhisper());
});

async function downloadLocalEngine(variant: "cpu" | "cuda"): Promise<void> {
  localWhisperBusy = true;
  localStatus.textContent = variant === "cuda" ? "正在下载 CUDA 引擎…" : "正在下载 CPU 引擎…";
  renderLocalWhisper();
  try {
    localWhisperStatus = await invoke<LocalWhisperStatus>("download_local_whisper_engine", { variant });
    asrResult.className = "asr-result success";
    asrResult.textContent = variant === "cuda" ? "CUDA 引擎已安装" : "CPU 引擎已安装";
  } catch (error) {
    asrResult.className = "asr-result failure";
    asrResult.textContent = `引擎下载失败：${error instanceof Error ? error.message : String(error)}`;
  } finally {
    localWhisperBusy = false;
    if (localWhisperStatus) renderLocalWhisper();
    else void refreshLocalWhisper();
  }
}

localDownloadEngine.addEventListener("click", () => void downloadLocalEngine("cpu"));
localDownloadCuda.addEventListener("click", () => void downloadLocalEngine("cuda"));

localDownloadModel.addEventListener("click", async () => {
  const model = localModelSelect.value;
  localWhisperBusy = true;
  localStatus.textContent = "正在下载本地模型…";
  renderLocalWhisper();
  try {
    localWhisperStatus = await invoke<LocalWhisperStatus>("download_local_whisper_model", { model });
    asrResult.className = "asr-result success";
    asrResult.textContent = "本地模型已下载";
  } catch (error) {
    asrResult.className = "asr-result failure";
    asrResult.textContent = `模型下载失败：${error instanceof Error ? error.message : String(error)}`;
  } finally {
    localWhisperBusy = false;
    if (localWhisperStatus) renderLocalWhisper();
    else void refreshLocalWhisper();
  }
});

localDeleteModel.addEventListener("click", async () => {
  if (!window.confirm("删除本地模型文件？已生成的转写不受影响，下次转写需要重新下载。")) return;
  localWhisperBusy = true;
  renderLocalWhisper();
  try {
    localWhisperStatus = await invoke<LocalWhisperStatus>("delete_local_whisper_model", {
      model: localModelSelect.value,
    });
    asrResult.className = "asr-result success";
    asrResult.textContent = "本地模型已删除";
  } catch (error) {
    asrResult.className = "asr-result failure";
    asrResult.textContent = `删除失败：${error instanceof Error ? error.message : String(error)}`;
  } finally {
    localWhisperBusy = false;
    if (localWhisperStatus) renderLocalWhisper();
    else void refreshLocalWhisper();
  }
});
app.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".transcribe");
  if (!button) return;
  const status = app.querySelector<HTMLElement>(".transcript-state");
  const provider = app.querySelector<HTMLSelectElement>(".asr-provider")?.value ?? "minimax";
  if (!status) return;
  transcription = beginTranscription();
  renderTranscriptionState();
  try {
    const transcript = await invoke<Transcript>("transcribe_session", { provider, language: null });
    transcription = transcriptionSucceeded(transcription);
    currentTranscript = transcript;
    currentTranslation = null;
    renderTranscriptBody();
    setTranscriptStatus();
    renderTranscriptionState();
    syncTranscriptActions(app);
    updateInsights();
    renderTranslationPanel();
    // The retention policy may have deleted the audio after transcription.
    syncBackend(await invoke<SessionState>("get_session_state"));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    transcription = transcriptionFailed(transcription, message);
    const errorMessage = transcriptionError(transcription);
    if (errorMessage) {
      view.errorMessage = errorMessage;
      applyView(app, view);
    }
    renderTranscriptionState();
  }
});
app.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".cancel-transcribe");
  if (!button) return;
  transcription = requestTranscriptionCancel(transcription);
  renderTranscriptionState();
  try {
    await invoke<boolean>("cancel_transcription");
  } catch (error) {
    const status = app.querySelector<HTMLElement>(".transcript-state");
    if (status) {
      status.textContent = `取消失败：${error instanceof Error ? error.message : String(error)}`;
    }
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
  const targetLanguage = translationLabel(selectedTranslationCode());
  const targetCode = selectedTranslationCode();
  const provider = app.querySelector<HTMLSelectElement>(".notes-provider")?.value ?? "minimax";
  const status = app.querySelector<HTMLElement>(".transcript-state");
  button.disabled = true;
  if (status) status.textContent = "AI 正在翻译…";
  try {
    const translation = await invoke<string>("translate_transcript", { targetLanguage, provider });
    currentTranslation = { code: targetCode, content: translation };
    renderTranscriptBody();
    setTranscriptStatus("已翻译");
  } catch (error) {
    if (status) status.textContent = "翻译失败";
    view.errorMessage = `翻译失败：${error instanceof Error ? error.message : String(error)}`;
    applyView(app, view);
  } finally {
    button.disabled = false;
  }
});
app.addEventListener("change", async (event) => {
  const select = (event.target as HTMLElement).closest<HTMLSelectElement>(".translate-target");
  if (!select || !currentTranscript) return;
  await loadTranslation();
});
app.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".export-session");
  if (!button) return;
  const status = app.querySelector<HTMLElement>(".export-state");
  button.disabled = true;
  if (status) status.textContent = "正在导出…";
  try {
    const result = await invoke<ExportResult>("export_session");
    if (status) status.textContent = `已导出 ${result.files.length} 个文件到 ${result.directory}`;
    try {
      await invoke("reveal_export", { path: result.directory });
    } catch {
      // The export itself succeeded; opening Explorer is best effort.
    }
  } catch (error) {
    if (status) status.textContent = `导出失败：${error instanceof Error ? error.message : String(error)}`;
  } finally {
    button.disabled = false;
  }
});
app.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".discard-audio");
  if (!button) return;
  if (!window.confirm("删除后音频不可恢复，转写和会议笔记会保留。确定删除音频吗？")) return;
  button.disabled = true;
  try {
    syncBackend(await invoke<SessionState>("discard_session_audio"));
  } catch (error) {
    view.errorMessage = `无法删除音频：${error instanceof Error ? error.message : String(error)}`;
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
    sessionNotes = editor.value;
    updateInsights();
    applyTasks(app, sessionNotes);
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
  try {
    const content = await invoke<string>("summarize_session", {
      provider,
      source: editor.value.trim() || null,
    });
    editor.value = content;
    sessionNotes = content;
    await invoke("save_session_notes", { content });
    updateInsights();
    applyTasks(app, sessionNotes);
    status.textContent = "已生成并保存纪要";
  } catch (error) {
    status.textContent = `生成失败：${error instanceof Error ? error.message : String(error)}`;
  } finally {
    button.disabled = false;
  }
});

app.addEventListener("click", (event) => {
  seekFromTranscript(event.target as HTMLElement);
});
app.addEventListener("keydown", (event) => {
  if (event.key !== "Enter" && event.key !== " ") return;
  const line = (event.target as HTMLElement).closest<HTMLElement>(".transcript-line[data-start-ms]");
  if (!line) return;
  event.preventDefault();
  seekFromTranscript(line);
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
  if (select.dataset.option === "translation") {
    view.translation = select.value;
    liveTranslations = [];
    liveTranslatedCount = 0;
    liveTranslationError = undefined;
    liveTranslationRetryAfter = 0;
    liveTranslationGeneration += 1;
    setLiveIndicator(livePollHandle !== undefined);
    renderLive();
    void translateNewLiveSegments();
  }
  if (select.dataset.option === "audio") view.retainAudio = select.value === "retain";
  void persistSettings();
}));

for (const selector of [".microphone-select", ".system-output-select"]) {
  app.querySelector<HTMLSelectElement>(selector)!.addEventListener("change", () => void persistSettings());
}
for (const selector of [".auto-cleanup-days", ".storage-quota"]) {
  app.querySelector<HTMLSelectElement>(selector)!.addEventListener("change", () => void persistSettings());
}

// ----- Workspace tabs -------------------------------------------------------

function selectTab(tab: WorkspaceTab): void {
  activeTab = tab;
  applyTab(app, tab);
  onWorkspaceTabChanged(tab);
}

function onWorkspaceTabChanged(tab: WorkspaceTab): void {
  if (tab === "transcript") {
    if (view.status === "recording" || view.status === "paused") renderLive();
    syncTranscriptActions(app);
  }
  if (tab === "translation") renderTranslationPanel();
  if (tab === "analysis") {
    const editor = app.querySelector<HTMLTextAreaElement>(".meeting-notes");
    if (editor && sessionNotes) editor.value = sessionNotes;
    updateInsights();
  }
  if (tab === "tasks") applyTasks(app, sessionNotes);
  if (tab === "chat") renderChatThreads();
}

app.querySelectorAll<HTMLButtonElement>(".workspace-tab").forEach((button) => {
  button.addEventListener("click", () => selectTab(button.dataset.tab as WorkspaceTab));
  button.addEventListener("keydown", (event) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const tabs = Array.from(app.querySelectorAll<HTMLButtonElement>(".workspace-tab"));
    const index = tabs.indexOf(button);
    const next = event.key === "Home"
      ? 0
      : event.key === "End"
        ? tabs.length - 1
        : (index + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
    tabs[next]?.focus();
    selectTab(tabs[next].dataset.tab as WorkspaceTab);
  });
});

// ----- Scene presets --------------------------------------------------------

const sceneCards = app.querySelectorAll<HTMLButtonElement>(".scene-card");
const savedScene = localStorage.getItem("magnetic-tape.scene");
function selectScene(scene: string): void {
  sceneCards.forEach((button) => {
    const selected = button.dataset.scene === scene;
    button.classList.toggle("selected", selected);
    button.setAttribute("aria-pressed", String(selected));
    if (selected) {
      const name = app.querySelector<HTMLElement>(".scene-compact-name");
      const description = app.querySelector<HTMLElement>(".scene-compact-desc");
      if (name) name.textContent = button.querySelector("span")?.textContent ?? "";
      if (description) description.textContent = button.dataset.description ?? "";
    }
  });
  localStorage.setItem("magnetic-tape.scene", scene);
}
if (savedScene && Array.from(sceneCards).some((button) => button.dataset.scene === savedScene)) {
  selectScene(savedScene);
}
sceneCards.forEach((button) => button.addEventListener("click", () => {
  if (button.disabled) return;
  selectScene(button.dataset.scene ?? "meeting");
}));

// ----- Scroll-follow --------------------------------------------------------

app.addEventListener("scroll", (event) => {
  const target = event.target as HTMLElement;
  if (!target.classList.contains("transcript-scroll")) return;
  liveAutoFollow = target.scrollHeight - target.scrollTop - target.clientHeight < 28;
  if (liveAutoFollow) liveUnread = 0;
  updateFollowUi();
}, true);

app.addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".follow-latest");
  if (!button) return;
  liveAutoFollow = true;
  liveUnread = 0;
  lastLiveSegmentCount = liveSegments.length;
  const scroller = app.querySelector<HTMLElement>(".transcript-scroll");
  if (scroller) scroller.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" });
  updateFollowUi();
});

// ----- Content toolbar ------------------------------------------------------

const moreButton = app.querySelector<HTMLButtonElement>(".more-button")!;
const moreMenu = app.querySelector<HTMLElement>(".more-menu")!;
moreButton.addEventListener("click", (event) => {
  event.stopPropagation();
  const open = moreMenu.hidden;
  moreMenu.hidden = !open;
  moreButton.setAttribute("aria-expanded", String(open));
});
document.addEventListener("click", (event) => {
  if (!(event.target as HTMLElement).closest(".more-wrap")) {
    moreMenu.hidden = true;
    moreButton.setAttribute("aria-expanded", "false");
  }
});

function visibleTranscriptLines(): HTMLElement[] {
  return Array.from(app.querySelectorAll<HTMLElement>(".transcript-panel .transcript-line"));
}

app.querySelector<HTMLButtonElement>(".copy-transcript")!.addEventListener("click", async (event) => {
  const button = event.currentTarget as HTMLButtonElement;
  const lines = visibleTranscriptLines();
  const text = lines.map((line) => line.querySelector<HTMLElement>(".transcript-content")?.textContent ?? "").join("\n");
  if (!text) return;
  try {
    await navigator.clipboard.writeText(text);
    button.querySelector("span")!.textContent = "已复制";
    window.setTimeout(() => { button.querySelector("span")!.textContent = "复制"; }, 1600);
  } catch {
    view.errorMessage = "无法写入剪贴板，请检查系统权限。";
    applyView(app, view);
  }
});

app.querySelector<HTMLButtonElement>(".mark-transcript")!.addEventListener("click", () => {
  const lines = visibleTranscriptLines();
  const target = lines.at(-1);
  if (!target) return;
  target.classList.toggle("marked");
  const button = app.querySelector<HTMLButtonElement>(".mark-transcript")!;
  button.querySelector("span")!.textContent = target.classList.contains("marked") ? "已标记" : "标记重点";
});

app.querySelector<HTMLButtonElement>(".seek-transcript")!.addEventListener("click", () => {
  const line = visibleTranscriptLines()[0];
  if (!line) return;
  seekFromTranscript(line);
});

app.querySelector<HTMLButtonElement>(".edit-transcript")!.addEventListener("click", () => {
  const line = visibleTranscriptLines().at(-1);
  const content = line?.querySelector<HTMLElement>(".transcript-content");
  if (!line || !content) return;
  content.contentEditable = "plaintext-only";
  content.focus();
  const range = document.createRange();
  range.selectNodeContents(content);
  const selection = window.getSelection();
  selection?.removeAllRanges();
  selection?.addRange(range);
  const finish = (): void => {
    content.contentEditable = "false";
    content.removeEventListener("blur", finish);
    const pill = line.querySelector<HTMLElement>(".recognized-pill");
    if (pill) pill.textContent = "已编辑";
    if (currentTranscript) {
      const segment = currentTranscript.segments.find((item) => item.startMs === Number(line.dataset.startMs));
      if (segment) segment.text = content.textContent ?? segment.text;
    }
    updateInsights();
  };
  content.addEventListener("blur", finish, { once: true });
  content.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      content.blur();
    }
    if (event.key === "Escape") {
      event.preventDefault();
      content.textContent = content.dataset.original ?? content.textContent;
      content.blur();
    }
  }, { once: true });
});

// ----- AI chat --------------------------------------------------------------

function renderChatThreads(): void {
  app.querySelectorAll<HTMLElement>(".chat-thread").forEach((thread) => {
    thread.replaceChildren();
    if (chatMessages.length === 0) {
      const suggestion = document.createElement("button");
      suggestion.className = "chat-suggestion";
      suggestion.type = "button";
      suggestion.textContent = "提问：刚才讨论的三个技术问题是什么？";
      thread.append(suggestion);
      return;
    }
    for (const message of chatMessages) {
      const wrapper = document.createElement("div");
      wrapper.className = `chat-message ${message.role}${message.pending ? " pending" : ""}`;
      const role = document.createElement("span");
      role.className = "chat-role";
      role.textContent = message.role === "user" ? "你" : "AI · 基于当前录音";
      const bubble = document.createElement("div");
      bubble.className = "chat-bubble";
      bubble.textContent = message.pending ? "正在整理当前录音…" : message.text;
      wrapper.append(role, bubble);
      thread.append(wrapper);
    }
    thread.scrollTop = thread.scrollHeight;
  });
}

async function sendChat(question: string): Promise<void> {
  const prompt = question.trim();
  if (!prompt || chatBusy) return;
  chatBusy = true;
  chatMessages.push({ role: "user", text: prompt }, { role: "assistant", text: "", pending: true });
  renderChatThreads();
  app.querySelectorAll<HTMLButtonElement>(".chat-send").forEach((button) => { button.disabled = true; });
  try {
    const configurations = await invoke<AiConfiguration[]>("get_ai_configurations");
    const configured = configurations.find((configuration) => configuration.configured);
    if (!configured) throw new Error("请先在“设置 → AI 服务”中配置 MiniMax 或 DeepSeek API Key");
    const segments = currentTranscript?.segments ?? liveSegments;
    const context = segments.slice(-20).map((segment) => `[${formatElapsed(segment.startMs)}] ${segment.text}`).join("\n");
    const response = await invoke<AiResponse>("generate_ai", {
      provider: configured.provider,
      prompt: [
        "你是 MagneticTape 的录音工作台助手。请只基于下面的录音上下文回答；如果上下文不足，直接说明缺少信息，不要编造。",
        `录音标题：${view.title}`,
        context ? `录音内容：\n${context}` : "当前还没有转写内容。",
        `用户问题：${prompt}`,
        "回答使用中文，必要时引用时间戳。",
      ].join("\n\n"),
    });
    chatMessages[chatMessages.length - 1] = { role: "assistant", text: response.content };
    liveAiProvider = response.provider;
  } catch (error) {
    chatMessages[chatMessages.length - 1] = {
      role: "assistant",
      text: `无法回答：${error instanceof Error ? error.message : String(error)}`,
    };
  } finally {
    chatBusy = false;
    app.querySelectorAll<HTMLButtonElement>(".chat-send").forEach((button) => { button.disabled = false; });
    renderChatThreads();
  }
}

app.addEventListener("click", (event) => {
  const suggestion = (event.target as HTMLElement).closest<HTMLButtonElement>(".chat-suggestion");
  if (!suggestion) return;
  const question = suggestion.textContent?.replace(/^提问：/, "") ?? "";
  void sendChat(question);
});
app.addEventListener("submit", (event) => {
  const form = event.target as HTMLFormElement;
  if (!form.classList.contains("chat-form")) return;
  event.preventDefault();
  const input = form.querySelector<HTMLInputElement>(".chat-input");
  if (!input) return;
  const value = input.value;
  input.value = "";
  void sendChat(value);
});
app.querySelectorAll<HTMLButtonElement>(".chat-clear").forEach((button) => button.addEventListener("click", () => {
  chatMessages = [];
  renderChatThreads();
}));

// ----- Dialogs and global navigation ---------------------------------------

const settingsDialog = app.querySelector<HTMLDialogElement>(".settings-dialog")!;
const knowledgeDialog = app.querySelector<HTMLDialogElement>(".knowledge-dialog")!;
app.querySelector<HTMLButtonElement>(".settings-button")!.addEventListener("click", () => settingsDialog.showModal());
app.querySelector<HTMLButtonElement>(".settings-close")!.addEventListener("click", () => settingsDialog.close());
app.querySelector<HTMLButtonElement>(".knowledge-button")!.addEventListener("click", () => knowledgeDialog.showModal());
app.querySelector<HTMLButtonElement>(".knowledge-close")!.addEventListener("click", () => knowledgeDialog.close());
app.querySelector<HTMLButtonElement>(".knowledge-open-history")!.addEventListener("click", () => {
  knowledgeDialog.close();
  historySearch.value = "";
  historyDialog.showModal();
  void refreshHistory();
  void refreshLibraryStats();
});
app.querySelector<HTMLButtonElement>(".settings-open-ai")!.addEventListener("click", () => {
  settingsDialog.close();
  aiDialog.showModal();
  void refreshAiConfiguration();
});
app.querySelector<HTMLButtonElement>(".settings-open-asr")!.addEventListener("click", () => {
  settingsDialog.close();
  openAsrSettings();
});
app.querySelector<HTMLButtonElement>(".settings-open-library")!.addEventListener("click", () => {
  settingsDialog.close();
  historySearch.value = "";
  historyDialog.showModal();
  void refreshHistory();
  void refreshLibraryStats();
});

// ----- Focus and player chrome ---------------------------------------------

const appShell = app.querySelector<HTMLElement>(".app-shell")!;
app.querySelector<HTMLButtonElement>(".focus-toggle")!.addEventListener("click", (event) => {
  const button = event.currentTarget as HTMLButtonElement;
  const active = appShell.classList.toggle("theater");
  button.title = active ? "退出专注阅读" : "专注阅读";
  button.setAttribute("aria-label", button.title);
});
const workspaceGrid = app.querySelector<HTMLElement>(".workspace-grid")!;
const insightColumn = app.querySelector<HTMLElement>(".insight-column")!;
const narrowInsight = window.matchMedia("(max-width: 1099px)");

function setInsightOpen(open: boolean): void {
  if (narrowInsight.matches) {
    insightColumn.classList.toggle("open", open);
    if (open) insightColumn.removeAttribute("aria-hidden");
    else insightColumn.setAttribute("aria-hidden", "true");
    return;
  }
  insightColumn.classList.remove("open");
  insightColumn.removeAttribute("aria-hidden");
  insightColumn.classList.toggle("collapsed", !open);
  workspaceGrid.classList.toggle("insight-collapsed", !open);
  localStorage.setItem("magnetic-tape.insight", open ? "open" : "collapsed");
}

function insightIsOpen(): boolean {
  return narrowInsight.matches
    ? insightColumn.classList.contains("open")
    : !insightColumn.classList.contains("collapsed");
}

function syncInsightMode(): void {
  if (narrowInsight.matches) {
    insightColumn.classList.remove("collapsed");
    workspaceGrid.classList.remove("insight-collapsed");
    setInsightOpen(insightColumn.classList.contains("open"));
  } else {
    insightColumn.classList.remove("open");
    setInsightOpen(localStorage.getItem("magnetic-tape.insight") !== "collapsed");
  }
}
syncInsightMode();
narrowInsight.addEventListener("change", syncInsightMode);

app.addEventListener("click", (event) => {
  const target = event.target as HTMLElement;
  if (target.closest(".insight-toggle")) {
    setInsightOpen(!insightIsOpen());
    return;
  }
  if (target.closest(".insight-drawer-toggle")) {
    setInsightOpen(true);
    return;
  }
  if (target.closest(".insight-collapse")) {
    setInsightOpen(false);
    return;
  }
  const confirm = target.closest<HTMLButtonElement>(".insight-confirm");
  if (confirm) {
    confirm.classList.add("confirmed");
    confirm.textContent = "已确认";
    confirm.disabled = true;
    const badge = confirm.closest(".insight-card")?.querySelector<HTMLElement>(".insight-badge.pending");
    if (badge) {
      badge.textContent = "已确认";
      badge.classList.remove("pending");
      badge.classList.add("summary");
    }
    return;
  }
  const source = target.closest<HTMLButtonElement>(".insight-source");
  if (source && playbackPlayer) {
    const start = Number(source.dataset.startMs);
    if (Number.isFinite(start)) {
      playbackPlayer.seek(start);
      highlightTranscript(start);
    }
  }
});

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && narrowInsight.matches && insightColumn.classList.contains("open")) {
    setInsightOpen(false);
  }
});
app.querySelector<HTMLButtonElement>(".player-expand")!.addEventListener("click", async () => {
  try {
    if (document.fullscreenElement) await document.exitFullscreen();
    else await document.documentElement.requestFullscreen();
  } catch {
    // Fullscreen is optional; some WebView2/OS combinations reject it.
  }
});

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

for (const eventName of ["transcription-progress", "translation-progress", "summarize-progress"] as const) {
  await listen<JobProgress>(eventName, (event) => applyJobProgress(event.payload));
}
await listen<WhisperDownloadProgress>("local-whisper-download", (event) => {
  const { kind, downloaded, total } = event.payload;
  if (localPanel.hidden) return;
  const percent = total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : 0;
  const label = kind === "engine" ? "引擎" : "模型";
  localStatus.textContent = `正在下载${label} ${percent}%（${formatBytes(downloaded)} / ${formatBytes(total)}）`;
});

const recovered = await invoke<SessionState | null>("recover_latest_session");
syncBackend(recovered ?? await invoke<SessionState>("get_session_state"));
applySettings(await invoke<AppSettings>("get_settings"));
applyTab(app, "transcript");
renderChatThreads();
void refreshAiStatus();
await refreshAudioDevices();
applySavedDevices();

