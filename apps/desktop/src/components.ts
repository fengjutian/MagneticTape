import type { AudioDeviceInfo, AudioDeviceSnapshot, RecorderStatus, RecorderViewState } from "./types";

function icon(name: "pause" | "play" | "stop" | "sparkles"): string {
  const paths = {
    pause: '<path d="M8 6v12M16 6v12"/>',
    play: '<path d="m9 6 9 6-9 6Z"/>',
    stop: '<rect x="7" y="7" width="10" height="10" rx="2"/>',
    sparkles: '<path d="m12 3 1.2 4.1L17 8.5l-3.8 1.4L12 14l-1.2-4.1L7 8.5l3.8-1.4ZM5 15l.7 2.3L8 18l-2.3.7L5 21l-.7-2.3L2 18l2.3-.7Z"/>',
  };
  return `<svg viewBox="0 0 24 24" aria-hidden="true">${paths[name]}</svg>`;
}

export function renderApp(): string {
  return `
    <main class="workspace">
      <section class="recorder-pane" aria-labelledby="recording-title">
        <header class="session-header">
          <div class="title-row">
            <button class="title-display" id="recording-title" type="button" aria-label="编辑录音标题">新录音</button>
            <input class="title-input" type="text" maxlength="80" aria-label="录音标题" hidden />
            <span class="local-badge">本地</span>
          </div>
          <div class="options" aria-label="录音选项">
            <label class="option"><span>转写语言</span><select data-option="language" aria-label="转写语言"><option value="auto">自动识别</option><option value="zh-CN">中文</option><option value="en-US">English</option></select></label>
            <label class="option"><span>翻译</span><select data-option="translation" aria-label="翻译语言"><option value="none">不翻译</option><option value="zh-CN">翻译为中文</option><option value="en-US">Translate to English</option></select></label>
            <label class="option"><span>音频</span><select data-option="audio" aria-label="音频保留策略"><option value="retain">保留音频</option><option value="discard" disabled>完成后删除（即将支持）</option></select></label>
            <button class="option coming-soon" type="button" disabled title="AI 同传尚未接入">${icon("sparkles")}<span>AI 同传</span><small>即将支持</small></button>
          </div>
          <div class="device-strip" aria-label="音频设备">
            <label><span class="device-kind">麦克风</span><select class="microphone-select" aria-label="麦克风设备"><option>正在检测…</option></select><small class="microphone-detail"></small></label>
            <label><span class="device-kind">系统声音</span><select class="system-output-select" aria-label="系统输出设备"><option>正在检测…</option></select><small class="system-output-detail"></small></label>
            <button class="refresh-devices" type="button" title="重新检测音频设备">重新检测</button>
          </div>
        </header>

        <section class="status-area" aria-live="polite">
          <div class="status-mark"><span></span></div>
          <h2 class="status-title">等待开始录音</h2>
          <p class="status-copy">麦克风与系统声音将分别保存在本地。</p>
          <button class="start-button" type="button">开始录音</button>
        </section>

        <section class="controls" aria-label="录音控制">
          <div class="waveform" aria-hidden="true">${Array.from({ length: 48 }, (_, index) => `<i style="--bar:${index}"></i>`).join("")}</div>
          <div class="control-row">
            <time class="elapsed" datetime="PT0S">00:00</time>
            <button class="pause-button" type="button" disabled title="暂停录音">${icon("pause")}<span>暂停</span></button>
            <button class="stop-button" type="button" disabled title="停止并保存">${icon("stop")}<span>停止</span></button>
          </div>
          <p class="shortcut-hint">Space 暂停/继续 · Ctrl + Enter 停止</p>
        </section>
      </section>

      <aside class="content-pane" aria-labelledby="content-title">
        <header class="content-header">
          <div><p class="section-kicker">MEETING NOTES</p><h2 id="content-title">实时内容</h2></div>
          <span class="content-status">待机</span>
        </header>
        <div class="content-body"></div>
        <div class="error-banner" role="alert" hidden></div>
      </aside>
    </main>
  `;
}

export function applyAudioDevices(root: HTMLElement, snapshot: AudioDeviceSnapshot): void {
  updateDeviceSelect(root, ".microphone-select", ".microphone-detail", snapshot.microphones, "未检测到麦克风");
  updateDeviceSelect(root, ".system-output-select", ".system-output-detail", snapshot.systemOutputs, "未检测到系统输出");
}

function updateDeviceSelect(
  root: HTMLElement,
  selectQuery: string,
  detailQuery: string,
  devices: AudioDeviceInfo[],
  emptyText: string,
): void {
  const select = root.querySelector<HTMLSelectElement>(selectQuery)!;
  select.replaceChildren();
  if (devices.length === 0) {
    select.add(new Option(emptyText, ""));
    select.disabled = true;
    root.querySelector<HTMLElement>(detailQuery)!.textContent = "请检查 Windows 声音设置";
    return;
  }
  select.disabled = false;
  for (const device of devices) {
    select.add(new Option(`${device.name}${device.isDefault ? "（默认）" : ""}`, device.id, device.isDefault, device.isDefault));
  }
  const selected = devices.find((device) => device.id === select.value) ?? devices[0];
  root.querySelector<HTMLElement>(detailQuery)!.textContent = `${selected.sampleRate / 1000} kHz · ${selected.channels} 声道 · ${selected.sampleFormat}`;
  select.onchange = () => {
    const device = devices.find((item) => item.id === select.value);
    if (device) root.querySelector<HTMLElement>(detailQuery)!.textContent = `${device.sampleRate / 1000} kHz · ${device.channels} 声道 · ${device.sampleFormat}`;
  };
}

export function formatElapsed(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1000));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60).toString().padStart(2, "0");
  const remainder = (seconds % 60).toString().padStart(2, "0");
  return hours > 0 ? `${hours.toString().padStart(2, "0")}:${minutes}:${remainder}` : `${minutes}:${remainder}`;
}

function statusText(status: RecorderStatus): { title: string; copy: string; badge: string } {
  switch (status) {
    case "recording": return { title: "正在录音", copy: "麦克风与系统声音正在分别写入本地 WAV 文件。", badge: "录音中" };
    case "paused": return { title: "录音已暂停", copy: "计时与模拟波形已暂停，点击继续恢复会话。", badge: "已暂停" };
    case "stopping": return { title: "正在结束", copy: "正在安全结束会话，请稍候。", badge: "保存中" };
    case "finished": return { title: "录音已保存", copy: "两路音频文件已安全封口，可在右侧查看保存位置。", badge: "已完成" };
    case "error": return { title: "操作失败", copy: "请查看错误信息后重试。", badge: "错误" };
    default: return { title: "等待开始录音", copy: "选择设备后，麦克风与系统声音将分别保存在本地。", badge: "待机" };
  }
}

export function applyView(root: HTMLElement, state: RecorderViewState): void {
  const text = statusText(state.status);
  const statusChanged = root.dataset.status !== state.status;
  root.dataset.status = state.status;
  root.querySelector<HTMLElement>(".status-title")!.textContent = text.title;
  root.querySelector<HTMLElement>(".status-copy")!.textContent = text.copy;
  root.querySelector<HTMLElement>(".content-status")!.textContent = text.badge;
  root.querySelector<HTMLElement>(".title-display")!.textContent = state.title;
  const elapsed = root.querySelector<HTMLTimeElement>(".elapsed")!;
  elapsed.textContent = formatElapsed(state.elapsedMs);
  elapsed.dateTime = `PT${Math.floor(state.elapsedMs / 1000)}S`;

  const active = state.status === "recording" || state.status === "paused";
  const busy = state.status === "stopping";
  const pause = root.querySelector<HTMLButtonElement>(".pause-button")!;
  pause.disabled = !active || busy;
  pause.innerHTML = `${icon(state.status === "paused" ? "play" : "pause")}<span>${state.status === "paused" ? "继续" : "暂停"}</span>`;
  pause.title = state.status === "paused" ? "继续录音" : "暂停录音";
  root.querySelector<HTMLButtonElement>(".stop-button")!.disabled = !active || busy;
  const start = root.querySelector<HTMLButtonElement>(".start-button")!;
  start.hidden = active || busy;
  start.textContent = state.status === "finished" ? "开始新录音" : "开始录音";

  const error = root.querySelector<HTMLElement>(".error-banner")!;
  error.hidden = !state.errorMessage;
  error.textContent = state.errorMessage ?? "";
  if (statusChanged) {
    root.querySelector<HTMLElement>(".content-body")!.innerHTML = contentForStatus(state);
  }
}

function contentForStatus(state: RecorderViewState): string {
  const { status, sessionId } = state;
  if (status === "recording" || status === "paused") {
    return `<div class="transcript-placeholder"><span class="pulse-dot"></span><h3>${status === "paused" ? "字幕已暂停" : "等待实时字幕"}</h3><p>ASR 尚未接入，因此不会生成虚构的转写内容。</p><div class="skeleton"><i></i><i></i><i></i></div></div>`;
  }
  if (status === "stopping") return `<div class="saving"><span class="spinner" aria-hidden="true"></span><h3>正在安全结束会话</h3><p>请勿重复提交停止操作。</p></div>`;
  if (status === "finished") {
    const revealPath = state.microphonePath ?? state.systemPath;
    return `<div class="finished-card"><span class="check">✓</span><h3>录音已保存</h3><p>会话编号：${sessionId?.slice(0, 12) ?? "—"}</p><dl><div><dt>麦克风</dt><dd title="${escapeHtml(state.microphonePath ?? "")}">${escapeHtml(state.microphonePath ?? "未生成")}</dd></div><div><dt>系统声音</dt><dd title="${escapeHtml(state.systemPath ?? "")}">${escapeHtml(state.systemPath ?? "未生成")}</dd></div><div><dt>丢弃缓冲块</dt><dd>${state.droppedBlocks ?? 0}</dd></div></dl>${revealPath ? `<button class="reveal-recording" type="button" data-path="${escapeHtml(revealPath)}">在资源管理器中查看</button>` : ""}</div>`;
  }
  return `<div class="empty-content"><div class="cassette-mini" aria-hidden="true"><i></i><i></i></div><h3>内容将在这里出现</h3><p>开始录音后，这里将承载实时字幕。停止后可继续生成摘要、章节和待办。</p></div>`;
}

function escapeHtml(value: string): string {
  const replacements: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
  return value.replace(/[&<>"']/g, (character) => replacements[character]);
}

