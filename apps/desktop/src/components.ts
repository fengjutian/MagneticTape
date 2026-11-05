import type { RecorderStatus, RecorderViewState } from "./types";

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
            <label class="option"><span>音频</span><select data-option="audio" aria-label="音频保留策略"><option value="retain">保留音频</option><option value="discard">完成后删除</option></select></label>
            <button class="option coming-soon" type="button" disabled title="AI 同传尚未接入">${icon("sparkles")}<span>AI 同传</span><small>即将支持</small></button>
          </div>
        </header>

        <section class="status-area" aria-live="polite">
          <div class="status-mark"><span></span></div>
          <h2 class="status-title">等待开始录音</h2>
          <p class="status-copy">实时转写功能尚未接入，录音功能仍在开发中。</p>
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

export function formatElapsed(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1000));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60).toString().padStart(2, "0");
  const remainder = (seconds % 60).toString().padStart(2, "0");
  return hours > 0 ? `${hours.toString().padStart(2, "0")}:${minutes}:${remainder}` : `${minutes}:${remainder}`;
}

function statusText(status: RecorderStatus): { title: string; copy: string; badge: string } {
  switch (status) {
    case "recording": return { title: "正在录音", copy: "正在建立本地会话；音频采集尚未接入。", badge: "录音中" };
    case "paused": return { title: "录音已暂停", copy: "计时与模拟波形已暂停，点击继续恢复会话。", badge: "已暂停" };
    case "stopping": return { title: "正在结束", copy: "正在安全结束会话，请稍候。", badge: "保存中" };
    case "finished": return { title: "会话已结束", copy: "会话状态已保存；当前版本尚未产生音频文件。", badge: "已完成" };
    case "error": return { title: "操作失败", copy: "请查看错误信息后重试。", badge: "错误" };
    default: return { title: "等待开始录音", copy: "实时转写功能尚未接入，录音功能仍在开发中。", badge: "待机" };
  }
}

export function applyView(root: HTMLElement, state: RecorderViewState): void {
  const text = statusText(state.status);
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
  root.querySelector<HTMLElement>(".content-body")!.innerHTML = contentForStatus(state.status, state.sessionId);
}

function contentForStatus(status: RecorderStatus, sessionId: string | null): string {
  if (status === "recording" || status === "paused") {
    return `<div class="transcript-placeholder"><span class="pulse-dot"></span><h3>${status === "paused" ? "字幕已暂停" : "等待实时字幕"}</h3><p>ASR 尚未接入，因此不会生成虚构的转写内容。</p><div class="skeleton"><i></i><i></i><i></i></div></div>`;
  }
  if (status === "stopping") return `<div class="saving"><span class="spinner" aria-hidden="true"></span><h3>正在安全结束会话</h3><p>请勿重复提交停止操作。</p></div>`;
  if (status === "finished") {
    return `<div class="finished-card"><span class="check">✓</span><h3>会话状态已保存</h3><p>会话编号：${sessionId?.slice(0, 12) ?? "—"}</p><dl><div><dt>音频文件</dt><dd>尚未接入</dd></div><div><dt>转写内容</dt><dd>尚未接入</dd></div><div><dt>摘要</dt><dd>等待后续版本</dd></div></dl></div>`;
  }
  return `<div class="empty-content"><div class="cassette-mini" aria-hidden="true"><i></i><i></i></div><h3>内容将在这里出现</h3><p>开始录音后，这里将承载实时字幕。停止后可继续生成摘要、章节和待办。</p></div>`;
}

