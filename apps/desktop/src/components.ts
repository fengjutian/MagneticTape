import type { AudioDeviceInfo, AudioDeviceSnapshot, RecorderStatus, RecorderViewState, Transcript } from "./types";
import {
  Captions,
  CassetteTape,
  Check,
  Clock3,
  Download,
  FileText,
  FolderOpen,
  HardDrive,
  History,
  Languages,
  Mic,
  MessageSquareText,
  MonitorUp,
  Pause,
  Play,
  Radio,
  ShieldCheck,
  RefreshCw,
  Sparkles,
  Square,
  Trash2,
  WandSparkles,
  type IconNode,
} from "lucide";

const icons = { pause: Pause, play: Play, stop: Square, sparkles: Sparkles } as const;

function iconNode(node: IconNode, className = "ui-icon"): string {
  const [, svgAttributes, children = []] = node;
  const attributes = Object.entries(svgAttributes)
    .map(([key, value]) => `${key === "class" ? "class" : key}="${String(value)}"`)
    .join(" ");
  const body = children.map(([tag, childAttributes]) => {
    const child = Object.entries(childAttributes).map(([key, value]) => `${key}="${String(value)}"`).join(" ");
    return `<${tag} ${child}/>`;
  }).join("");
  return `<svg ${attributes} class="${className}" aria-hidden="true">${body}</svg>`;
}

function icon(name: keyof typeof icons, className = "ui-icon"): string {
  return iconNode(icons[name], className);
}

export function renderApp(): string {
  return `
    <main class="workspace">
      <section class="recorder-pane" aria-labelledby="recording-title">
        <header class="session-header">
          <div class="title-row">
            <span class="brand-mark">${iconNode(CassetteTape)}</span>
            <button class="title-display" id="recording-title" type="button" aria-label="编辑录音标题">新录音</button>
            <input class="title-input" type="text" maxlength="80" aria-label="录音标题" hidden />
            <span class="local-badge">本地</span>
            <wa-button class="history-button" type="button" size="small">${iconNode(History)}<span>历史录音</span></wa-button>
          </div>
          <div class="options" aria-label="录音选项">
            <label class="option"><span>${iconNode(Languages)}转写语言</span><select data-option="language" aria-label="转写语言"><option value="auto">自动识别</option><option value="zh-CN">中文</option><option value="en-US">English</option></select></label>
            <label class="option"><span>${iconNode(Radio)}AI 同传</span><select data-option="translation" aria-label="实时翻译语言"><option value="none">不翻译</option><option value="zh-CN">实时翻译为中文</option><option value="en-US">Live English translation</option></select></label>
            <label class="option"><span>${iconNode(HardDrive)}音频</span><select data-option="audio" aria-label="音频保留策略"><option value="retain">保留音频</option><option value="discard">转写后删除音频</option></select></label>
            <button class="option ai-settings" type="button" title="配置 AI Provider">${icon("sparkles")}<span>AI 助手</span><small>MiniMax / DeepSeek</small></button>
            <button class="option live-caption" type="button" title="录音时约每 5 秒生成实时字幕和增量译文">${iconNode(Captions)}<span>实时字幕</span><small class="live-state">关</small></button>
          </div>
          <div class="device-strip" aria-label="音频设备">
            <label><span class="device-icon">${iconNode(Mic)}</span><span class="device-kind">麦克风</span><select class="microphone-select" aria-label="麦克风设备"><option>正在检测…</option></select><small class="microphone-detail"></small></label>
            <label><span class="device-icon">${iconNode(MonitorUp)}</span><span class="device-kind">系统声音</span><select class="system-output-select" aria-label="系统输出设备"><option>正在检测…</option></select><small class="system-output-detail"></small></label>
            <wa-button class="refresh-devices" type="button" size="small" title="重新检测音频设备">${iconNode(RefreshCw)}<span>重新检测</span></wa-button>
          </div>
        </header>

        <section class="status-area" aria-live="polite">
          <div class="status-orbit"><span class="status-mark">${iconNode(Mic)}</span><i></i><i></i><i></i></div>
          <span class="status-eyebrow">${iconNode(ShieldCheck)}本地安全录制</span>
          <h2 class="status-title">等待开始录音</h2>
          <p class="status-copy">麦克风与系统声音将分别保存在本地。</p>
          <p class="device-warning" role="status" hidden></p>
          <wa-button class="start-button" type="button" variant="brand">开始录音</wa-button>
        </section>

        <section class="controls" aria-label="录音控制">
          <div class="waveform" aria-hidden="true">${Array.from({ length: 48 }, (_, index) => `<i style="--bar:${index}"></i>`).join("")}</div>
          <div class="level-meters" aria-label="实时输入电平">
            <div class="level-channel microphone-level"><span class="level-name">麦克风</span><div class="level-track" role="meter" aria-label="麦克风电平" aria-valuemin="-60" aria-valuemax="0" aria-valuenow="-60"><i></i><b class="clip-flag">CLIP</b></div><output class="level-readout">−60 dB</output></div>
            <div class="level-channel system-level"><span class="level-name">系统声音</span><div class="level-track" role="meter" aria-label="系统声音电平" aria-valuemin="-60" aria-valuemax="0" aria-valuenow="-60"><i></i><b class="clip-flag">CLIP</b></div><output class="level-readout">−60 dB</output></div>
          </div>
          <div class="control-row">
            <span class="elapsed-wrap">${iconNode(Clock3)}<time class="elapsed" datetime="PT0S">00:00</time></span>
            <wa-button class="pause-button" type="button" disabled title="暂停录音">${icon("pause")}<span>暂停</span></wa-button>
            <wa-button class="stop-button" type="button" variant="brand" disabled title="停止并保存">${icon("stop")}<span>停止</span></wa-button>
          </div>
          <p class="shortcut-hint">Space 暂停/继续 · Ctrl + Enter 停止</p>
        </section>
      </section>

      <aside class="content-pane" aria-labelledby="content-title">
        <header class="content-header">
          <div class="content-heading"><span class="content-heading-icon">${iconNode(MessageSquareText)}</span><div><p class="section-kicker">LIVE TRANSCRIPT</p><h2 id="content-title">实时内容</h2><p class="content-subtitle">实时捕捉对话，重要内容不错过</p></div></div>
          <span class="content-status">待机</span>
        </header>
        <div class="content-body"></div>
        <div class="error-banner" role="alert" hidden></div>
      </aside>
    </main>
    <dialog class="ai-dialog" aria-labelledby="ai-dialog-title">
      <form method="dialog" class="ai-form">
        <header><div><p class="section-kicker">AI PROVIDER</p><h2 id="ai-dialog-title">配置 AI 助手</h2></div><button class="dialog-close" value="cancel" aria-label="关闭">×</button></header>
        <label><span>服务商</span><select class="ai-provider"><option value="minimax">MiniMax</option><option value="deep_seek">DeepSeek</option></select></label>
        <label><span>模型</span><input class="ai-model" value="MiniMax-M3.1-Flash" autocomplete="off" /></label>
        <label><span>API Key</span><input class="ai-key" type="password" placeholder="保存在 Windows 凭据管理器" autocomplete="off" /></label>
        <p class="ai-privacy">密钥由 Windows 凭据管理器加密保存，只发送到所选服务商。</p>
        <div class="ai-credential-row"><span class="ai-config-status">正在读取配置…</span><button class="ai-clear" type="button">清除密钥</button></div>
        <label><span>向 AI 提问</span><textarea class="ai-prompt" rows="3" placeholder="例如：给我一个中文会议纪要模板"></textarea></label>
        <output class="ai-result" aria-live="polite"></output>
        <footer><button class="ai-cancel" value="cancel">取消</button><button class="ai-generate" type="button">发送</button><button class="ai-test" type="button">保存并测试连接</button></footer>
      </form>
    </dialog>
    <dialog class="asr-dialog" aria-labelledby="asr-dialog-title">
      <form method="dialog" class="asr-form">
        <header><div><p class="section-kicker">SPEECH TO TEXT</p><h2 id="asr-dialog-title">配置转写服务</h2></div><button class="dialog-close" value="cancel" aria-label="关闭">×</button></header>
        <label><span>服务商</span><select class="asr-dialog-provider"><option value="local_whisper">本地 Whisper（离线）</option><option value="openai_realtime">OpenAI Realtime（流式）</option><option value="minimax">MiniMax（分片）</option><option value="silicon_flow">SiliconFlow（分片）</option></select></label>
        <label><span>识别语言</span><select class="asr-language"><option value="auto">自动识别</option><option value="zh-CN">中文</option><option value="en-US">English</option></select></label>
        <div class="asr-remote">
          <label><span>接口地址</span><input class="asr-endpoint" autocomplete="off" /></label>
          <label><span>模型</span><input class="asr-model" autocomplete="off" /></label>
          <label><span>API Key</span><input class="asr-key" type="password" placeholder="留空则复用已保存的密钥" autocomplete="off" /></label>
          <p class="ai-privacy">转写会把混音后的 16 kHz 音频上传到所选服务商；密钥保存在 Windows 凭据管理器。</p>
          <div class="ai-credential-row"><span class="asr-config-status">正在读取配置…</span><button class="asr-clear" type="button">清除密钥</button></div>
        </div>
        <section class="local-whisper" hidden aria-label="本地 Whisper">
          <label><span>本地模型</span><select class="local-model" aria-label="本地 Whisper 模型"></select></label>
          <p class="local-whisper-status" aria-live="polite">正在读取本地引擎状态…</p>
          <div class="local-whisper-actions">
            <button class="local-download-engine" type="button">下载引擎</button>
            <button class="local-download-model" type="button">下载模型</button>
            <button class="local-delete-model" type="button">删除模型</button>
          </div>
          <p class="ai-privacy">本地转写完全离线、不需要 API Key；引擎与模型保存在应用数据目录，删除模型不影响引擎。</p>
        </section>
        <output class="asr-result" aria-live="polite"></output>
        <footer><button value="cancel">取消</button><button class="asr-save" type="button">保存配置</button></footer>
      </form>
    </dialog>
    <dialog class="history-dialog" aria-labelledby="history-dialog-title">
      <section class="history-panel">
        <header><div><p class="section-kicker">RECORDING LIBRARY</p><h2 id="history-dialog-title">历史录音</h2></div><button class="history-close" type="button" aria-label="关闭">×</button></header>
        <input class="history-search" type="search" placeholder="搜索标题或会话编号" aria-label="搜索历史录音" />
        <div class="history-list"><p class="history-empty">正在读取录音…</p></div>
        <footer class="history-footer">
          <span class="library-stats">正在统计占用…</span>
          <div class="storage-settings">
            <label>自动清理<select class="auto-cleanup-days" aria-label="自动清理音频"><option value="0">关闭</option><option value="30">30 天</option><option value="90">90 天</option><option value="180">180 天</option></select></label>
            <label>存储上限<select class="storage-quota" aria-label="存储上限"><option value="0">不限制</option><option value="5120">5 GB</option><option value="10240">10 GB</option><option value="20480">20 GB</option></select></label>
            <span class="storage-hint">下次启动生效</span>
          </div>
          <div class="cleanup-row">
            <select class="cleanup-days" aria-label="清理多少天前的音频">
              <option value="30">30 天前</option>
              <option value="90">90 天前</option>
              <option value="180">180 天前</option>
            </select>
            <button class="cleanup-audio" type="button">清理旧音频</button>
          </div>
        </footer>
      </section>
    </dialog>
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
    case "recording": return { title: "正在录音", copy: "麦克风与系统声音正在分别写入本地 Opus 分片。", badge: "录音中" };
    case "paused": return { title: "录音已暂停", copy: "计时与波形已暂停，点击继续恢复会话。", badge: "已暂停" };
    case "stopping": return { title: "正在结束", copy: "正在安全结束会话，请稍候。", badge: "保存中" };
    case "finished": return { title: "录音已保存", copy: "两路音频分片已安全封口，可在右侧查看保存位置。", badge: "已完成" };
    case "error": return { title: "操作失败", copy: "请查看错误信息后重试。", badge: "错误" };
    default: return { title: "等待开始录音", copy: "选择设备后，麦克风与系统声音将分别保存在本地。", badge: "待机" };
  }
}

export function applyView(root: HTMLElement, state: RecorderViewState): void {
  const text = state.status === "finished" && state.recovered
    ? { title: "已恢复上次录音", copy: "应用上次未正常退出，已载入最近一次安全提交的音频。", badge: "已恢复" }
    : statusText(state.status);
  const statusChanged = root.dataset.status !== state.status;
  const sessionChanged = root.dataset.session !== (state.sessionId ?? "");
  const audioKey = `${state.microphoneChunks?.length ?? 0}/${state.systemChunks?.length ?? 0}`;
  const audioChanged = root.dataset.audio !== audioKey;
  root.dataset.status = state.status;
  root.dataset.session = state.sessionId ?? "";
  root.dataset.audio = audioKey;
  root.dataset.session = state.sessionId ?? "";
  root.querySelector<HTMLElement>(".status-title")!.textContent = text.title;
  root.querySelector<HTMLElement>(".status-copy")!.textContent = text.copy;
  root.querySelector<HTMLElement>(".content-status")!.textContent = text.badge;
  root.querySelector<HTMLElement>(".title-display")!.textContent = state.title;
  const elapsed = root.querySelector<HTMLTimeElement>(".elapsed")!;
  elapsed.textContent = formatElapsed(state.elapsedMs);
  elapsed.dateTime = `PT${Math.floor(state.elapsedMs / 1000)}S`;

  const active = state.status === "recording" || state.status === "paused";
  const busy = state.status === "stopping";
  const pause = root.querySelector<HTMLElement & { disabled: boolean }>(".pause-button")!;
  pause.disabled = !active || busy;
  pause.innerHTML = `${icon(state.status === "paused" ? "play" : "pause")}<span>${state.status === "paused" ? "继续" : "暂停"}</span>`;
  pause.title = state.status === "paused" ? "继续录音" : "暂停录音";
  const stop = root.querySelector<HTMLElement & { disabled: boolean }>(".stop-button")!;
  stop.disabled = !active || busy;
  stop.innerHTML = `${icon("stop")}<span>${busy ? "正在保存" : "结束并保存录音"}</span>`;
  const start = root.querySelector<HTMLElement>(".start-button")!;
  start.hidden = active || busy;
  start.innerHTML = `${iconNode(Mic)}<span>${state.status === "finished" ? "开始新录音" : "开始录音"}</span>`;

  const error = root.querySelector<HTMLElement>(".error-banner")!;
  error.hidden = !state.errorMessage;
  error.textContent = state.errorMessage ?? "";
  if (statusChanged || sessionChanged || audioChanged) {
    root.querySelector<HTMLElement>(".content-body")!.innerHTML = contentForStatus(state);
  }
}

function contentForStatus(state: RecorderViewState): string {
  const { status, sessionId } = state;
  if (status === "recording" || status === "paused") {
    return `<div class="transcript-placeholder"><span class="live-placeholder-icon">${iconNode(Captions)}</span><h3>${status === "paused" ? "字幕已暂停" : "正在聆听…"}</h3><p>实时字幕将在识别到语音后显示；开启 AI 同传可同步生成译文。</p><div class="skeleton"><i></i><i></i><i></i></div></div>`;
  }
  if (status === "stopping") return `<div class="saving"><span class="spinner" aria-hidden="true"></span><h3>正在安全结束会话</h3><p>请勿重复提交停止操作。</p></div>`;
  if (status === "finished") {
    const revealPath = state.microphonePath ?? state.systemPath;
    return `<div class="finished-card">
      <section class="session-summary">
        <span class="check">${iconNode(Check)}</span>
        <div><h3>${state.recovered ? "已恢复上次录音" : "录音已保存"}</h3><p>会话 ${sessionId?.slice(0, 12) ?? "—"} · 音频已安全保存在本地</p></div>
      </section>
      <section class="playback" aria-label="录音回放">
        <div class="track-legend" aria-label="音轨图例"><span class="system-track"><i></i>系统声音</span><span class="microphone-track"><i></i>麦克风</span></div>
        <div class="track-preview" aria-hidden="true"><div class="system-wave">${Array.from({ length: 64 }, (_, index) => `<i style="--wave:${index}"></i>`).join("")}</div><div class="microphone-wave">${Array.from({ length: 64 }, (_, index) => `<i style="--wave:${index + 3}"></i>`).join("")}</div></div>
        <div class="playback-row"><wa-button class="playback-toggle" type="button" size="small" variant="brand" aria-label="播放录音">${icon("play", "ui-icon play")} ${icon("pause", "ui-icon pause")}<span>播放</span></wa-button><time class="playback-current" datetime="PT0S">00:00</time><input class="playback-seek" type="range" min="0" max="1000" value="0" step="1" aria-label="播放进度" /><time class="playback-total" datetime="PT0S">00:00</time><select class="playback-rate" aria-label="播放速度"><option value="0.75">0.75×</option><option value="1" selected>1×</option><option value="1.25">1.25×</option><option value="1.5">1.5×</option><option value="2">2×</option></select></div>
        <p class="playback-hint">双轨音频已自动对齐 · 点击字幕可跳转到对应时间</p>
      </section>
      <div class="workbench-grid">
        <section class="transcript-panel workbench-card">
          <header><span class="panel-heading">${iconNode(Captions)}<strong>转写文本</strong></span><span class="transcript-state"></span></header>
          <div class="transcript-body"><p class="transcript-empty">尚未转写。配置服务后，可生成带时间戳和说话人的完整文本。</p></div>
          <div class="transcript-actions"><select class="asr-provider" aria-label="转写服务商"><option value="local_whisper">本地 Whisper</option><option value="minimax">MiniMax</option><option value="silicon_flow">SiliconFlow</option></select><wa-button class="transcribe" type="button" size="small" variant="brand">开始转写</wa-button><wa-button class="cancel-transcribe" type="button" size="small" hidden>取消</wa-button><wa-button class="asr-settings" type="button" size="small">转写设置</wa-button><select class="translate-target" aria-label="翻译语言"><option value="zh">中文</option><option value="en">English</option></select><wa-button class="translate-transcript" type="button" size="small">AI 翻译</wa-button></div>
        </section>
        <section class="notes-editor workbench-card">
          <header><span class="panel-heading">${iconNode(FileText)}<strong>会议笔记</strong></span><span class="notes-state"></span></header>
          <textarea class="meeting-notes" rows="9" placeholder="记录关键结论、决策和待办事项…"></textarea>
          <div class="notes-actions"><select class="notes-provider" aria-label="AI 服务商"><option value="minimax">MiniMax</option><option value="deep_seek">DeepSeek</option></select><wa-button class="save-notes" type="button" size="small">保存笔记</wa-button><wa-button class="summarize-notes" type="button" size="small" variant="brand">${iconNode(WandSparkles)}AI 生成纪要</wa-button></div>
        </section>
      </div>
      <section class="session-details">
        <header><span class="panel-heading">${iconNode(HardDrive)}<strong>本地文件</strong></span><span>${(state.microphoneChunks?.length ?? 0) + (state.systemChunks?.length ?? 0)} 个音频分片</span></header>
        <dl><div><dt>麦克风</dt><dd title="${escapeHtml(state.microphonePath ?? "")}">${escapeHtml(state.microphonePath ?? "未生成")}</dd></div><div><dt>系统声音</dt><dd title="${escapeHtml(state.systemPath ?? "")}">${escapeHtml(state.systemPath ?? "未生成")}</dd></div><div><dt>丢弃缓冲块</dt><dd>${state.droppedBlocks ?? 0}</dd></div></dl>
      </section>
      <div class="export-row"><wa-button class="export-session" type="button" size="small">${iconNode(Download)}导出会议资料</wa-button>${revealPath ? `<wa-button class="reveal-recording" type="button" size="small" data-path="${escapeHtml(revealPath)}">${iconNode(FolderOpen)}打开文件夹</wa-button>` : ""}<wa-button class="discard-audio" type="button" size="small" title="删除分片音频，保留转写与笔记">${iconNode(Trash2)}删除音频</wa-button><span class="export-state"></span></div>
    </div>`;
  }
  return `<div class="empty-content"><div class="cassette-mini" aria-hidden="true"><i></i><i></i></div><h3>内容将在这里出现</h3><p>开始录音后，这里将承载实时字幕。停止后可继续生成摘要、章节和待办。</p></div>`;
}

/// Renders provider segments with timestamps and speaker labels. When a saved
/// translation exists it is appended below the source text instead of
/// replacing it.
export function renderTranscript(
  container: HTMLElement,
  transcript: Transcript,
  translation?: { label: string; content: string },
): void {
  container.replaceChildren();
  if (transcript.segments.length === 0) {
    const paragraph = document.createElement("p");
    paragraph.textContent = transcript.text;
    container.append(paragraph);
  } else {
    for (const segment of transcript.segments) {
      const line = document.createElement("p");
      line.className = "transcript-line";
      line.dataset.startMs = String(segment.startMs);
      line.tabIndex = 0;
      line.setAttribute("role", "button");
      line.title = "跳转到这段录音";
      const marker = document.createElement("span");
      marker.className = "transcript-marker";
      marker.innerHTML = iconNode(MessageSquareText);
      const time = document.createElement("time");
      time.textContent = formatElapsed(segment.startMs);
      const speaker = document.createElement("strong");
      speaker.textContent = segment.speaker ? `${segment.speaker} ` : "";
      const text = document.createElement("span");
      text.textContent = segment.text;
      const content = document.createElement("span");
      content.className = "transcript-content";
      content.append(speaker, text);
      line.append(marker, time, content);
      container.append(line);
    }
  }
  if (!translation || !translation.content.trim()) return;
  const section = document.createElement("section");
  section.className = "transcript-translation-block";
  const heading = document.createElement("h4");
  heading.textContent = `译文（${translation.label}）`;
  const body = document.createElement("div");
  body.className = "transcript-translation-body";
  body.textContent = translation.content;
  section.append(heading, body);
  container.append(section);
}

function escapeHtml(value: string): string {
  const replacements: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
  return value.replace(/[&<>"']/g, (character) => replacements[character]);
}

