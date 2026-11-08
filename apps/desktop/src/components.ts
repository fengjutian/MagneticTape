import type {
  AudioDeviceInfo,
  AudioDeviceSnapshot,
  RecorderStatus,
  RecorderViewState,
  Transcript,
  TranscriptSegment,
} from "./types";
import { confirmationEntry, confirmationKey, effectiveText, type ConfirmationMap } from "./confirmations";
import { deriveInsights, parseTasks } from "./insights";
import { speakerKey, speakerLabel, type SpeakerNames } from "./session-prefs";
import {
  AudioLines,
  BookOpen,
  Captions,
  Check,
  ChevronDown,
  ChevronRight,
  CircleHelp,
  Clock3,
  Copy,
  Crosshair,
  Download,
  FileText,
  FolderOpen,
  GraduationCap,
  Grid2X2,
  HardDrive,
  History,
  Info,
  Languages,
  Lightbulb,
  ListTodo,
  Maximize2,
  MessageSquareText,
  Mic,
  MicVocal,
  MoreHorizontal,
  Pause,
  Pencil,
  Phone,
  Play,
  Radio,
  RefreshCw,
  Send,
  Settings,
  Settings2,
  Sparkles,
  Square,
  Star,
  Trash2,
  Users,
  Volume2,
  WandSparkles,
  type IconNode,
} from "lucide";

export type WorkspaceTab = "transcript" | "translation" | "analysis" | "tasks" | "chat";

export interface InsightInput {
  title: string;
  status: RecorderStatus;
  segments: TranscriptSegment[];
  notes: string;
  confirmations?: ConfirmationMap;
}

const iconSet = { pause: Pause, play: Play, stop: Square } as const;

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

function icon(name: keyof typeof iconSet, className = "ui-icon"): string {
  return iconNode(iconSet[name], className);
}

const scenes = [
  { id: "meeting", label: "会议", description: "多人讨论、纪要", icon: Users },
  { id: "study", label: "学习", description: "课程、讲座", icon: GraduationCap },
  { id: "idea", label: "灵感", description: "个人语音记录", icon: Lightbulb },
  { id: "interview", label: "访谈", description: "一对多问答", icon: MicVocal },
  { id: "call", label: "电话记录", description: "通话纪要", icon: Phone },
  { id: "other", label: "自由录音", description: "不做结构假设", icon: Grid2X2 },
] as const;

export function renderApp(): string {
  return `
    <div class="app-shell">
      <header class="app-header">
        <div class="brand-block">
          <span class="app-logo">${iconNode(Mic)}</span>
          <div class="brand-copy">
            <h1>MagneticTape</h1>
            <p>AI Recording Workspace</p>
          </div>
        </div>
        <nav class="app-nav" aria-label="全局导航">
          <button class="nav-action nav-workspace active" type="button" aria-current="page" title="录音工作台">${iconNode(Mic)}<span>工作台</span></button>
          <button class="nav-action history-button" type="button" title="打开历史录音">${iconNode(History)}<span>历史录音</span></button>
          <button class="nav-action knowledge-button" type="button" title="打开知识中心">${iconNode(BookOpen)}<span>知识中心</span></button>
          <button class="nav-action settings-button" type="button" title="打开设置">${iconNode(Settings)}<span>设置</span></button>
        </nav>
        <div class="app-actions">
          <span class="app-local-badge">${iconNode(Check)}本地保存</span>
          <button class="nav-action insight-drawer-toggle" type="button" title="打开 AI 洞察" aria-label="打开 AI 洞察">${iconNode(Sparkles)}<span>AI 洞察</span></button>
        </div>
      </header>

      <div class="workspace-grid">
        <section class="panel control-panel" aria-labelledby="control-heading">
          <header class="panel-title-row control-head">
            <div class="control-title">
              <h2 id="control-heading">录音控制</h2>
              <span class="control-sub">本地优先</span>
            </div>
            <span class="control-status-badge"><i></i><span class="control-status-label">待机</span></span>
          </header>

          <div class="control-scroll">
            <div class="session-title-line">
              <button class="title-display" id="recording-title" type="button" aria-label="编辑录音标题">新录音</button>
              <input class="title-input" type="text" maxlength="80" aria-label="录音标题" hidden />
            </div>

            <section class="scene-section" aria-labelledby="scene-heading">
              <div class="section-title-row">
                <h3 id="scene-heading">录音场景</h3>
                <span class="section-title-actions">
                  <button class="round-help" type="button" title="场景会改变 AI 分析的结构" aria-label="场景说明">${iconNode(CircleHelp)}</button>
                </span>
              </div>
              <div class="scene-grid" role="group" aria-label="选择录音场景">
                ${scenes.map((scene, index) => `
                  <button class="scene-card${index === 0 ? " selected" : ""}" type="button" data-scene="${scene.id}" data-description="${scene.description}" aria-pressed="${index === 0}" title="${scene.label}：${scene.description}">
                    ${iconNode(scene.icon)}<span>${scene.label}</span>
                  </button>`).join("")}
              </div>
              <div class="scene-compact" hidden>
                <span class="scene-compact-label">场景</span>
                <strong class="scene-compact-name">会议</strong>
                <small class="scene-compact-desc">多人讨论、纪要</small>
              </div>
            </section>

            <div class="config-groups">
              <details class="config-group" open>
                <summary>
                  <span class="config-icon">${iconNode(Languages)}</span>
                  <span class="config-copy"><strong>语音配置</strong><small class="language-summary">自动识别 · 不翻译</small></span>
                  <span class="config-chevron">${iconNode(ChevronDown)}</span>
                </summary>
                <div class="config-body">
                  <label class="field-row"><span>${iconNode(FileText)}转写语言</span><select data-option="language" aria-label="转写语言"><option value="auto">自动识别</option><option value="zh-CN">中文</option><option value="en-US">English</option></select></label>
                  <label class="field-row"><span>${iconNode(Radio)}AI 同传</span><select data-option="translation" aria-label="实时翻译语言"><option value="none">不翻译</option><option value="zh-CN">实时翻译为中文</option><option value="en-US">Live English translation</option></select></label>
                </div>
              </details>

              <details class="config-group">
                <summary>
                  <span class="config-icon">${iconNode(AudioLines)}</span>
                  <span class="config-copy"><strong>音频配置</strong><small class="audio-device-summary">正在检测设备…</small></span>
                  <span class="config-chevron">${iconNode(ChevronDown)}</span>
                </summary>
                <div class="config-body">
                  <label class="field-row field-device"><span>${iconNode(Mic)}麦克风</span><select class="microphone-select" aria-label="麦克风设备"><option>正在检测…</option></select><small class="microphone-detail"></small></label>
                  <label class="field-row field-device"><span>${iconNode(Volume2)}系统声音</span><select class="system-output-select" aria-label="系统输出设备"><option>正在检测…</option></select><small class="system-output-detail"></small></label>
                  <button class="inline-action refresh-devices" type="button" title="重新检测音频设备">${iconNode(RefreshCw)}<span>重新检测设备</span></button>
                </div>
              </details>

              <details class="config-group">
                <summary>
                  <span class="config-icon ai-config">${iconNode(Settings2)}</span>
                  <span class="config-copy"><strong>AI 配置</strong><small class="ai-model-summary">MiniMax · 高速版</small></span>
                  <span class="config-status-dot" title="AI 服务状态"></span>
                  <span class="config-chevron">${iconNode(ChevronDown)}</span>
                </summary>
                <div class="config-body">
                  <button class="config-action ai-settings" type="button" title="配置 AI Provider">${iconNode(Sparkles)}<span>AI 服务与密钥</span><small>MiniMax / DeepSeek</small></button>
                  <button class="config-action live-caption" type="button" title="录音时实时生成字幕和增量译文（本地流式引擎就绪时约 1 秒出字）">${iconNode(Captions)}<span>实时字幕</span><small class="live-state">关</small></button>
                </div>
              </details>
            </div>

            <section class="record-card status-area" aria-live="polite">
              <div class="record-card-top">
                <span class="record-state-badge"><i></i><span class="record-state-label">待机</span></span>
                <span class="record-safety">${iconNode(Check)}本地安全保存 · <span class="chunk-count">0</span> 片</span>
              </div>
              <time class="elapsed" datetime="PT0S">00:00</time>
              <h3 class="status-title">等待开始录音</h3>
              <p class="status-copy">麦克风与系统声音将分别保存在本地。</p>
              <div class="waveform" aria-hidden="true">${Array.from({ length: 48 }, (_, index) => `<i style="--bar:${index};height:${6 + (index % 6) * 3}px"></i>`).join("")}</div>
              <div class="record-buttons">
                <button class="start-button" type="button">${iconNode(Mic)}<span>开始录音</span></button>
                <button class="pause-button" type="button" disabled title="暂停录音">${icon("pause")}<span>暂停</span></button>
                <button class="stop-button" type="button" disabled title="停止并保存">${icon("stop")}<span>结束</span></button>
              </div>
              <div class="level-meters" aria-label="实时输入电平">
                <div class="level-channel microphone-level">
                  <span class="level-name">${iconNode(Mic)}麦克风<em class="level-state">正常</em></span>
                  <div class="level-track" role="meter" aria-label="麦克风电平" aria-valuemin="-60" aria-valuemax="0" aria-valuenow="-60"><i></i></div>
                  <output class="level-readout">−60 dB</output>
                </div>
                <div class="level-channel system-level">
                  <span class="level-name">${iconNode(Volume2)}系统声音<em class="level-state">正常</em></span>
                  <div class="level-track" role="meter" aria-label="系统声音电平" aria-valuemin="-60" aria-valuemax="0" aria-valuenow="-60"><i></i></div>
                  <output class="level-readout">−60 dB</output>
                </div>
              </div>
              <p class="device-warning" role="status" hidden></p>
            </section>
          </div>
        </section>

        <section class="panel content-panel" aria-labelledby="content-heading">
          <header class="content-header">
            <div class="content-heading">
              <h2 id="content-heading" class="content-title">实时转录</h2>
              <span class="content-status live-pill"><i></i><span class="content-status-text">待机</span></span>
            </div>
            <div class="content-header-actions">
              <span class="header-elapsed">00:00</span>
              <button class="panel-icon-button insight-toggle" type="button" title="显示 AI 洞察" aria-label="显示 AI 洞察">${iconNode(Sparkles)}</button>
              <button class="panel-icon-button focus-toggle" type="button" title="专注阅读" aria-label="切换专注阅读">${iconNode(Maximize2)}</button>
            </div>
          </header>

          <div class="workspace-tabs" role="tablist" aria-label="内容视图">
            <button class="workspace-tab active" type="button" role="tab" aria-selected="true" data-tab="transcript"><span>转录</span></button>
            <button class="workspace-tab" type="button" role="tab" aria-selected="false" data-tab="translation"><span>翻译</span></button>
            <button class="workspace-tab" type="button" role="tab" aria-selected="false" data-tab="analysis"><span>AI 分析</span></button>
            <button class="workspace-tab" type="button" role="tab" aria-selected="false" data-tab="tasks"><span>任务</span></button>
            <button class="workspace-tab" type="button" role="tab" aria-selected="false" data-tab="chat"><span>AI 对话</span></button>
          </div>

          <div class="content-body" role="tabpanel" aria-label="实时内容面板"></div>

          <footer class="content-toolbar" aria-label="转录操作">
            <button class="toolbar-action edit-transcript" type="button" disabled>${iconNode(Pencil)}<span>编辑</span></button>
            <button class="toolbar-action mark-transcript" type="button" disabled>${iconNode(Crosshair)}<span>标记重点</span></button>
            <button class="toolbar-action copy-transcript" type="button" disabled>${iconNode(Copy)}<span>复制</span></button>
            <button class="toolbar-action seek-transcript" type="button" disabled>${iconNode(Clock3)}<span>跳转音频</span></button>
            <div class="more-wrap">
              <button class="toolbar-action more-button" type="button" aria-haspopup="true" aria-expanded="false">${iconNode(MoreHorizontal)}<span>更多</span></button>
              <div class="more-menu" role="menu" hidden>
                <button class="reveal-recording" type="button" role="menuitem">${iconNode(FolderOpen)}打开录音文件夹</button>
                <button class="discard-audio" type="button" role="menuitem">${iconNode(Trash2)}删除音频</button>
              </div>
            </div>
            <span class="error-banner" role="alert" hidden></span>
          </footer>
        </section>

        <aside class="insight-column" aria-label="AI 洞察">
          <div class="insight-rail" aria-label="AI 洞察快捷入口">
            <button class="rail-action" type="button" data-rail-target=".insight-topic" title="当前主题" aria-label="当前主题">${iconNode(Sparkles)}</button>
            <button class="rail-action" type="button" data-rail-target=".insight-points-card" title="关键观点" aria-label="关键观点">${iconNode(Lightbulb)}</button>
            <button class="rail-action" type="button" data-rail-target=".insight-decisions" title="决策事项" aria-label="决策事项">${iconNode(Check)}</button>
            <button class="rail-action" type="button" data-rail-target=".insight-todos" title="待办事项" aria-label="待办事项">${iconNode(ListTodo)}</button>
            <button class="rail-action" type="button" data-rail-target=".insight-questions" title="待确认问题" aria-label="待确认问题">${iconNode(CircleHelp)}</button>
            <button class="rail-expand" type="button" title="展开 AI 洞察" aria-label="展开 AI 洞察">${iconNode(ChevronRight)}</button>
          </div>
          <div class="insight-resizer" role="separator" aria-orientation="vertical" aria-label="调整 AI 洞察宽度" tabindex="0" title="拖拽调整宽度，双击恢复默认"></div>
          <section class="panel insight-panel" aria-labelledby="insight-heading">
            <header class="insight-head">
              <div class="insight-heading">
                <span class="insight-spark">${iconNode(Sparkles)}</span>
                <h2 id="insight-heading">AI 洞察</h2>
                <span class="ai-pill">AI</span>
              </div>
              <button class="panel-icon-button insight-collapse" type="button" title="收起 AI 洞察" aria-label="收起 AI 洞察">${iconNode(ChevronRight)}</button>
            </header>
            <div class="insight-scroll">
              <article class="insight-card insight-topic">
                <header><span>当前主题</span><span class="insight-badge summary">AI 总结</span></header>
                <p class="insight-topic-text">开始录音后自动识别主题</p>
              </article>
              <article class="insight-card insight-points-card">
                <header>${iconNode(Lightbulb)}<span>关键观点</span></header>
                <ul class="insight-points"><li class="insight-placeholder">转写后自动提炼关键观点</li></ul>
              </article>
              <article class="insight-card insight-decisions">
                <header>${iconNode(Check)}<span>决策事项</span><span class="insight-badge pending">待确认</span></header>
                <ul class="insight-decisions-list"><li class="insight-placeholder">尚未形成明确决策</li></ul>
              </article>
              <article class="insight-card insight-disputes">
                <header>${iconNode(CircleHelp)}<span>争议与分歧</span></header>
                <p class="insight-disputes-text">目前尚未检测到明确分歧。</p>
              </article>
              <article class="insight-card insight-todos">
                <header>${iconNode(ListTodo)}<span>待办事项</span><span class="todo-count">0 项</span></header>
                <ul class="insight-todos-list"><li class="insight-placeholder">暂未识别到待办</li></ul>
              </article>
              <article class="insight-card insight-questions">
                <header>${iconNode(CircleHelp)}<span>待确认问题</span></header>
                <ul class="insight-questions-list"><li class="insight-placeholder">暂无待确认问题</li></ul>
              </article>
            </div>
          </section>
        </aside>
      </div>

      <section class="review-view" aria-labelledby="review-title" hidden>
        <header class="review-header">
          <button class="review-back" type="button" title="返回工作台">${iconNode(ChevronRight)}<span>工作台</span></button>
          <div class="review-heading">
            <h1 id="review-title" class="review-title">录音复盘</h1>
            <p class="review-meta">
              <span class="review-date">—</span>
              <span class="review-duration">—</span>
              <span class="review-scene">—</span>
            </p>
          </div>
          <div class="review-header-actions">
            <button class="review-export action-button primary" type="button">${iconNode(Download)}<span>导出</span></button>
            <button class="review-more panel-icon-button" type="button" title="更多操作" aria-label="更多操作">${iconNode(MoreHorizontal)}</button>
          </div>
        </header>

        <div class="review-body">
          <main class="review-main">
            <section class="review-card review-summary">
              <header><h2>录音摘要</h2><span class="review-badge ai">AI 总结 · 待确认</span></header>
              <p class="review-summary-text">尚未生成摘要，可在 AI 分析中生成纪要。</p>
            </section>

            <section class="review-card review-points">
              <header><h2>关键观点</h2><span class="review-count">0 条</span></header>
              <ul class="review-point-list"><li class="review-empty">转写后自动提炼关键观点</li></ul>
            </section>

            <section class="review-card review-decisions">
              <header><h2>决策记录</h2><span class="review-count" data-review-count="decisions">0 条待确认</span></header>
              <ul class="review-decision-list"><li class="review-empty">尚未形成明确决策</li></ul>
            </section>

            <section class="review-card review-tasks">
              <header><h2>待办事项</h2><span class="review-count" data-review-count="tasks">0 条待确认</span></header>
              <div class="review-task-list"><p class="review-empty">暂未识别到待办</p></div>
            </section>

            <section class="review-card review-transcript-card">
              <header>
                <h2>完整转录</h2>
                <span class="review-count review-transcript-count">0 段</span>
                <button class="review-transcript-toggle" type="button">展开全文</button>
              </header>
              <div class="review-transcript-body transcript-body"></div>
            </section>
          </main>

          <aside class="review-side">
            <section class="review-card review-pending">
              <header><h2>待确认队列</h2><span class="review-pending-count">0</span></header>
              <ul class="review-pending-list"><li class="review-empty">暂无待确认内容</li></ul>
              <button class="review-confirm-all" type="button" disabled>全部确认…</button>
            </section>

            <section class="review-card review-chat-entry">
              <header><h2>AI 对话</h2></header>
              <p>基于本次录音继续追问，回答会引用原始片段。</p>
              <button class="review-open-chat" type="button">${iconNode(MessageSquareText)}<span>打开 AI 对话</span></button>
            </section>

            <section class="review-card review-info">
              <header><h2>录音信息</h2></header>
              <dl class="review-info-list"></dl>
            </section>
          </aside>
        </div>
      </section>

      <footer class="player-bar playback" aria-label="全局播放器">
        <button class="playback-toggle" type="button" disabled aria-label="播放录音">${icon("play", "ui-icon play")}${icon("pause", "ui-icon pause")}<span class="playback-toggle-label">播放</span></button>
        <div class="player-time">
          <time class="playback-current" datetime="PT0S">00:00</time><span class="player-time-divider">/</span><time class="playback-total" datetime="PT0S">00:00</time>
        </div>
        <input class="playback-seek" type="range" min="0" max="1000" value="0" step="1" aria-label="播放进度" />
        <select class="playback-rate" aria-label="播放速度"><option value="0.75">0.75×</option><option value="1" selected>1.0×</option><option value="1.25">1.25×</option><option value="1.5">1.5×</option><option value="2">2.0×</option></select>
        <div class="player-volume">
          ${iconNode(Volume2)}
          <input class="playback-volume" type="range" min="0" max="100" value="100" step="1" aria-label="音量" />
        </div>
        <button class="player-expand" type="button" title="全屏" aria-label="切换全屏">${iconNode(Maximize2)}</button>
        <p class="playback-hint" hidden></p>
      </footer>
    </div>

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
        <label class="asr-hotwords-row"><span>热词 / 提示词（可选）</span><textarea class="asr-hotwords" rows="2" maxlength="512" placeholder="专有名词、产品名、参会人姓名，用逗号或顿号分隔"></textarea></label>
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
            <button class="local-download-engine" type="button">下载 CPU 引擎</button>
            <button class="local-download-engine-cuda" type="button">下载 CUDA 引擎</button>
            <button class="local-download-model" type="button">下载模型</button>
            <button class="local-delete-model" type="button">删除模型</button>
          </div>
          <p class="ai-privacy">本地转写完全离线、不需要 API Key；检测到 NVIDIA GPU 时可下载 CUDA 引擎加速。引擎与模型保存在应用数据目录，删除模型不影响引擎。</p>
        </section>
        <section class="sherpa-live" aria-label="本地流式字幕">
          <label class="sherpa-toggle"><input class="live-streaming-toggle" type="checkbox" checked /><span>本地流式实时字幕</span></label>
          <p class="sherpa-status" aria-live="polite">正在读取本地流式引擎状态…</p>
          <div class="sherpa-actions">
            <button class="sherpa-download-engine" type="button">下载流式引擎（24 MB）</button>
            <button class="sherpa-download-asr" type="button">下载中文流式模型（74 MB）</button>
            <button class="sherpa-download-speaker" type="button">下载声纹模型（28 MB）</button>
            <button class="sherpa-download-diarization" type="button">下载分离模型（7 MB）</button>
          </div>
          <p class="ai-privacy">流式识别完全离线，字幕约 0.5–1 秒出字；声纹模型用来区分不同发言人，未安装时只显示“发言人”。录后仍会用所选转写服务生成定稿。</p>
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
          <div class="diagnostics-row">
            <button class="open-logs" type="button">打开日志目录</button>
            <button class="export-diagnostics" type="button">导出诊断包</button>
            <span class="diagnostics-state" aria-live="polite"></span>
          </div>
        </footer>
      </section>
    </dialog>

    <dialog class="settings-dialog" aria-labelledby="settings-dialog-title">
      <section class="settings-panel">
        <header><div><p class="section-kicker">PREFERENCES</p><h2 id="settings-dialog-title">设置</h2></div><button class="settings-close" type="button" aria-label="关闭">×</button></header>
        <div class="settings-list">
          <button class="settings-option settings-open-ai" type="button"><span class="settings-option-icon">${iconNode(Sparkles)}</span><span><strong>AI 服务</strong><small>MiniMax / DeepSeek、模型与 API Key</small></span>${iconNode(ChevronRight)}</button>
          <button class="settings-option settings-open-asr" type="button"><span class="settings-option-icon">${iconNode(Captions)}</span><span><strong>转写服务</strong><small>本地 Whisper、云端 ASR 与热词</small></span>${iconNode(ChevronRight)}</button>
          <button class="settings-option settings-open-library" type="button"><span class="settings-option-icon">${iconNode(HardDrive)}</span><span><strong>录音资料库</strong><small>存储、清理与诊断</small></span>${iconNode(ChevronRight)}</button>
        </div>
        <p class="settings-footnote">所有设置保存在本机；API Key 加密保存在 Windows 凭据管理器。</p>
      </section>
    </dialog>

    <dialog class="knowledge-dialog" aria-labelledby="knowledge-dialog-title">
      <section class="knowledge-panel">
        <header><div><p class="section-kicker">KNOWLEDGE CENTER</p><h2 id="knowledge-dialog-title">知识库</h2></div><button class="knowledge-close" type="button" aria-label="关闭">×</button></header>
        <div class="knowledge-empty">
          <span>${iconNode(BookOpen)}</span>
          <h3>知识沉淀即将上线</h3>
          <p>后续将支持跨录音检索、主题关联与 AI 问答。当前请先在历史记录中打开单次录音。</p>
          <button class="knowledge-open-history" type="button">打开历史录音</button>
        </div>
      </section>
    </dialog>
  `;
}

export function applyAudioDevices(root: HTMLElement, snapshot: AudioDeviceSnapshot): void {
  updateDeviceSelect(root, ".microphone-select", ".microphone-detail", snapshot.microphones, "未检测到麦克风");
  updateDeviceSelect(root, ".system-output-select", ".system-output-detail", snapshot.systemOutputs, "未检测到系统输出");
  const microphone = snapshot.microphones.find((device) => device.isDefault) ?? snapshot.microphones[0];
  const system = snapshot.systemOutputs.find((device) => device.isDefault) ?? snapshot.systemOutputs[0];
  const summary = root.querySelector<HTMLElement>(".audio-device-summary");
  if (summary) {
    summary.textContent = microphone && system
      ? `${shortDeviceName(microphone.name)} · ${shortDeviceName(system.name)}`
      : "未检测到可用设备";
  }
}

function shortDeviceName(name: string): string {
  return name.length > 16 ? `${name.slice(0, 15)}…` : name;
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

function statusText(status: RecorderStatus): { title: string; copy: string; badge: string; state: string } {
  switch (status) {
    case "recording": return { title: "正在录音", copy: "麦克风与系统声音正在分别写入本地 Opus 分片。", badge: "录音中", state: "录音中" };
    case "paused": return { title: "录音已暂停", copy: "计时与波形已暂停，点击继续恢复会话。", badge: "已暂停", state: "已暂停" };
    case "stopping": return { title: "正在结束", copy: "正在安全结束会话，请稍候。", badge: "保存中", state: "保存中" };
    case "finished": return { title: "录音已保存", copy: "两路音频分片已安全封口，可在右侧查看保存位置。", badge: "已完成", state: "已完成" };
    case "error": return { title: "操作失败", copy: "请查看错误信息后重试。", badge: "错误", state: "错误" };
    default: return { title: "等待开始录音", copy: "选择设备后，麦克风与系统声音将分别保存在本地。", badge: "待机", state: "待机" };
  }
}

export function applyView(root: HTMLElement, state: RecorderViewState, forceContent = false): void {
  const text = state.status === "finished" && state.recovered
    ? { title: "已恢复上次录音", copy: "应用上次未正常退出，已载入最近一次安全提交的音频。", badge: "已恢复", state: "已恢复" }
    : statusText(state.status);
  const previousStatus = root.dataset.status as RecorderStatus | undefined;
  const statusChanged = previousStatus !== state.status;
  const sessionChanged = root.dataset.session !== (state.sessionId ?? "");
  const active = state.status === "recording" || state.status === "paused";
  const busy = state.status === "stopping";
  root.dataset.status = state.status;
  root.dataset.session = state.sessionId ?? "";
  const shell = root.querySelector<HTMLElement>(".app-shell");
  if (shell) shell.dataset.status = state.status;

  root.querySelector<HTMLElement>(".status-title")!.textContent = text.title;
  root.querySelector<HTMLElement>(".status-copy")!.textContent = text.copy;
  root.querySelector<HTMLElement>(".record-state-label")!.textContent = text.state;
  root.querySelector<HTMLElement>(".control-status-label")!.textContent = text.state;
  root.querySelector<HTMLElement>(".content-status-text")!.textContent = state.status === "recording" ? "LIVE" : text.badge;
  root.querySelector<HTMLElement>(".title-display")!.textContent = state.title;
  const contentTitle = root.querySelector<HTMLElement>(".content-title");
  if (contentTitle) contentTitle.textContent = state.status === "idle" ? "实时转录" : (state.title || "实时转录");

  const elapsed = root.querySelector<HTMLTimeElement>(".elapsed")!;
  elapsed.textContent = formatElapsed(state.elapsedMs);
  elapsed.dateTime = `PT${Math.floor(state.elapsedMs / 1000)}S`;
  const headerElapsed = root.querySelector<HTMLElement>(".header-elapsed");
  if (headerElapsed) headerElapsed.textContent = formatElapsed(state.elapsedMs);
  const chunkCount = (state.microphoneChunks?.length ?? 0) + (state.systemChunks?.length ?? 0);
  const chunkLabel = root.querySelector<HTMLElement>(".chunk-count");
  if (chunkLabel) chunkLabel.textContent = String(chunkCount);

  const wasActive = previousStatus === "recording" || previousStatus === "paused";
  if (active && !wasActive) {
    root.querySelectorAll<HTMLDetailsElement>("details.config-group").forEach((group) => { group.open = false; });
  }
  const sceneCompact = root.querySelector<HTMLElement>(".scene-compact");
  if (sceneCompact) {
    sceneCompact.hidden = !active;
    const selected = root.querySelector<HTMLButtonElement>(".scene-card.selected");
    const name = sceneCompact.querySelector<HTMLElement>(".scene-compact-name");
    const description = sceneCompact.querySelector<HTMLElement>(".scene-compact-desc");
    if (name) name.textContent = selected?.querySelector("span")?.textContent ?? "会议";
    if (description) description.textContent = selected?.dataset.description ?? "";
  }

  const pause = root.querySelector<HTMLButtonElement>(".pause-button")!;
  pause.disabled = !active || busy;
  pause.hidden = !active;
  pause.innerHTML = `${icon(state.status === "paused" ? "play" : "pause")}<span>${state.status === "paused" ? "继续" : "暂停"}</span>`;
  pause.title = state.status === "paused" ? "继续录音" : "暂停录音";

  const stop = root.querySelector<HTMLButtonElement>(".stop-button")!;
  stop.disabled = !active || busy;
  stop.hidden = !active;
  stop.innerHTML = `${icon("stop")}<span>${busy ? "正在保存" : "结束"}</span>`;

  const start = root.querySelector<HTMLButtonElement>(".start-button")!;
  start.hidden = active || busy;
  start.innerHTML = `${iconNode(Mic)}<span>${state.status === "finished" ? "开始新录音" : "开始录音"}</span>`;

  root.querySelector<HTMLElement>(".live-caption")?.classList.toggle("disabled", busy);
  root.querySelectorAll<HTMLButtonElement>(".scene-card").forEach((button) => {
    button.disabled = active || busy;
  });

  const error = root.querySelector<HTMLElement>(".error-banner")!;
  error.hidden = !state.errorMessage;
  error.textContent = state.errorMessage ?? "";

  const finished = state.status === "finished";
  const discard = root.querySelector<HTMLElement>(".discard-audio");
  const reveal = root.querySelector<HTMLElement>(".reveal-recording");
  if (discard) discard.hidden = !finished;
  if (reveal) reveal.hidden = !finished || !(state.microphonePath ?? state.systemPath);

  if (state.status !== "finished" && state.status !== "idle") {
    const total = root.querySelector<HTMLTimeElement>(".playback-total");
    if (total) total.textContent = formatElapsed(state.elapsedMs);
  }
  if (state.status === "idle") {
    const current = root.querySelector<HTMLTimeElement>(".playback-current");
    const total = root.querySelector<HTMLTimeElement>(".playback-total");
    if (current) current.textContent = "00:00";
    if (total) total.textContent = "00:00";
  }

  const toolbar = root.querySelector<HTMLElement>(".content-toolbar");
  toolbar?.classList.toggle("recording", active);
  const hasTranscript = root.querySelector(".transcript-line") !== null;
  setToolbarEnabled(root, hasTranscript);
  updateLanguageSummary(root, state);

  if (statusChanged || sessionChanged || forceContent || root.querySelector(".tab-panel") === null) {
    root.querySelector<HTMLElement>(".content-body")!.innerHTML = contentForStatus(state);
    applyTab(root, (root.dataset.tab as WorkspaceTab | undefined) ?? "transcript");
  }
}

function updateLanguageSummary(root: HTMLElement, state: RecorderViewState): void {
  const language = state.language === "zh-CN" ? "中文" : state.language === "en-US" ? "English" : "自动识别";
  const translation = state.translation === "zh-CN" ? "中文" : state.translation === "en-US" ? "English" : state.translation === "none" ? "不翻译" : state.translation;
  const summary = root.querySelector<HTMLElement>(".language-summary");
  if (summary) summary.textContent = `${language} · ${translation}`;
}

function setToolbarEnabled(root: HTMLElement, enabled: boolean): void {
  for (const selector of [".edit-transcript", ".mark-transcript", ".copy-transcript", ".seek-transcript"]) {
    const button = root.querySelector<HTMLButtonElement>(selector);
    if (button) button.disabled = !enabled;
  }
}

export function syncTranscriptActions(root: HTMLElement): void {
  setToolbarEnabled(root, root.querySelector(".transcript-line") !== null);
}

export function applyTab(root: HTMLElement, tab: WorkspaceTab): void {
  root.dataset.tab = tab;
  root.querySelectorAll<HTMLButtonElement>(".workspace-tab").forEach((button) => {
    const active = button.dataset.tab === tab;
    button.classList.toggle("active", active);
    button.setAttribute("aria-selected", String(active));
    button.tabIndex = active ? 0 : -1;
  });
  root.querySelectorAll<HTMLElement>(".tab-panel").forEach((panel) => {
    panel.hidden = panel.dataset.panel !== tab;
  });
  const body = root.querySelector<HTMLElement>(".content-body");
  if (body) body.setAttribute("aria-label", tabLabel(tab));
  const toolbar = root.querySelector<HTMLElement>(".content-toolbar");
  if (toolbar) toolbar.hidden = tab !== "transcript";
}

function tabLabel(tab: WorkspaceTab): string {
  const labels: Record<WorkspaceTab, string> = {
    transcript: "转录",
    translation: "翻译",
    analysis: "AI 分析",
    tasks: "任务",
    chat: "AI 对话",
  };
  return labels[tab];
}

function contentForStatus(state: RecorderViewState): string {
  const { status } = state;
  return `
    <section class="tab-panel transcript-panel" data-panel="transcript">${transcriptPanel(state)}</section>
    <section class="tab-panel translation-panel" data-panel="translation" hidden>${translationPanel(status)}</section>
    <section class="tab-panel analysis-panel" data-panel="analysis" hidden>${analysisPanel(state)}</section>
    <section class="tab-panel tasks-panel" data-panel="tasks" hidden>${tasksPanel()}</section>
    <section class="tab-panel workspace-chat-panel" data-panel="chat" hidden>${workspaceChatPanel()}</section>
  `;
}

function transcriptPanel(state: RecorderViewState): string {
  if (state.status === "finished") return finishedTranscriptPanel(state);
  if (state.status === "stopping") {
    return `<div class="panel-state saving"><span class="spinner" aria-hidden="true"></span><h3>正在安全结束会话</h3><p>请勿重复提交停止操作。</p></div>`;
  }
  if (state.status === "recording" || state.status === "paused") {
    return `
      <div class="transcript-scroll">
        <div class="transcript-stream">
          <div class="transcript-placeholder">
            <span class="live-placeholder-icon">${iconNode(Captions)}</span>
            <h3>${state.status === "paused" ? "转录已暂停" : "正在聆听…"}</h3>
            <p>实时字幕将在识别到语音后显示；开启 AI 同传可同步生成译文。</p>
          </div>
        </div>
        <div class="recognition-status" hidden><span class="recognition-dots" aria-hidden="true"><i></i><i></i><i></i></span><span>正在识别下一段语音…</span></div>
      </div>
      <button class="follow-latest" type="button" hidden>${iconNode(ChevronDown)}<span>回到最新内容</span><span class="follow-count" hidden>0</span></button>`;
  }
  return `<div class="panel-state empty-content"><span class="empty-illustration">${iconNode(Captions)}</span><h3>开始录音后，实时内容会出现在这里</h3><p>录音会安全保存在本地；开启实时字幕后，转录与 AI 洞察将同步更新。</p></div>`;
}

function finishedTranscriptPanel(state: RecorderViewState): string {
  return `
    <div class="transcript-scroll finished-scroll">
      <div class="finished-card finished-transcript-card">
        <section class="session-summary">
          <span class="check">${iconNode(Check)}</span>
          <div><h3>${state.recovered ? "已恢复上次录音" : "录音已保存"}</h3><p>会话 ${state.sessionId?.slice(0, 12) ?? "—"} · 音频已安全保存在本地</p></div>
          <span class="finished-pill">已保存</span>
        </section>
        <section class="transcript-workbench workbench-card">
          <header>
            <span class="panel-heading">${iconNode(Captions)}<strong>转写文本</strong></span>
            <span class="transcript-state">尚未转写</span>
          </header>
          <div class="transcript-body"><p class="transcript-empty">尚未转写。配置服务后，可生成带时间戳和说话人的完整文本。</p></div>
          <div class="transcript-actions">
            <select class="asr-provider" aria-label="转写服务商"><option value="local_whisper">本地 Whisper</option><option value="minimax">MiniMax</option><option value="silicon_flow">SiliconFlow</option></select>
            <button class="action-button primary transcribe" type="button">${iconNode(WandSparkles)}<span>开始转写</span></button>
            <button class="action-button cancel-transcribe" type="button" hidden>取消</button>
            <button class="action-button asr-settings" type="button">${iconNode(Settings2)}<span>转写设置</span></button>
            <select class="translate-target" aria-label="翻译语言"><option value="zh">中文</option><option value="en">English</option></select>
            <button class="action-button translate-transcript" type="button">${iconNode(Languages)}<span>AI 翻译</span></button>
            <button class="action-button enter-review" type="button">${iconNode(FileText)}<span>进入复盘</span></button>
            <button class="action-button export-session" type="button">${iconNode(Download)}<span>导出资料</span></button>
            <span class="export-state"></span>
          </div>
        </section>
      </div>
    </div>
  `;
}

function translationPanel(status: RecorderStatus): string {
  if (status === "finished") {
    return `<div class="translation-layout"><div class="translation-source"><span class="translation-label">原文</span><div class="transcript-body translation-original"><p class="transcript-empty">完成转写后可生成双语翻译。</p></div></div><div class="translation-result"><span class="translation-label ai-label">${iconNode(Languages)}译文</span><div class="translation-content"><p class="transcript-empty">译文会显示在这里，并保留原文与时间戳。</p></div></div></div>`;
  }
  if (status === "recording" || status === "paused") {
    return `<div class="live-translation-panel"><div class="live-translation-list"><div class="panel-state"><span class="empty-illustration ai-bg">${iconNode(Languages)}</span><h3>双语翻译</h3><p>录音中的 AI 同传结果会在这里按段显示；开启实时字幕后自动生成。</p></div></div></div>`;
  }
  return `<div class="panel-state">${translationEmpty()}</div>`;
}

function translationEmpty(): string {
  return `<span class="empty-illustration ai-bg">${iconNode(Languages)}</span><h3>还没有可翻译的内容</h3><p>完成录音和转写后，可生成中文或英文译文。</p>`;
}

function analysisPanel(state: RecorderViewState): string {
  if (state.status !== "finished") {
    return `<div class="panel-state"><span class="empty-illustration ai-bg">${iconNode(Sparkles)}</span><h3>AI 分析等待内容</h3><p>录音转写完成后，这里会整理主题、关键观点、待办事项与会议摘要。</p></div>`;
  }
  return `
    <div class="analysis-layout">
      <section class="notes-editor workbench-card">
        <header><span class="panel-heading">${iconNode(FileText)}<strong>会议笔记</strong></span><span class="notes-state"></span></header>
        <textarea class="meeting-notes" rows="10" placeholder="记录关键结论、决策和待办事项…"></textarea>
        <div class="notes-actions">
          <select class="notes-provider" aria-label="AI 服务商"><option value="minimax">MiniMax</option><option value="deep_seek">DeepSeek</option></select>
          <button class="action-button save-notes" type="button">保存笔记</button>
          <button class="action-button primary summarize-notes" type="button">${iconNode(WandSparkles)}<span>AI 生成纪要</span></button>
        </div>
      </section>
      <section class="analysis-hint"><span>${iconNode(Sparkles)}</span><p>AI 生成的内容默认标记为“待确认”，确认后才会进入正式会议记录。</p></section>
    </div>`;
}

function tasksPanel(): string {
  return `<div class="panel-state tasks-empty"><span class="empty-illustration">${iconNode(ListTodo)}</span><h3>还没有任务</h3><p>任务会从转写中提取，并在你确认后进入正式记录。</p></div><div class="tasks-content" hidden></div>`;
}

function workspaceChatPanel(): string {
  return `
    <div class="workspace-chat-layout">
      <div class="workspace-chat-thread chat-thread" aria-live="polite">
        <button class="chat-suggestion" type="button">提问：刚才讨论的三个技术问题是什么？</button>
      </div>
      <form class="chat-form workspace-chat-form">
        <input class="chat-input" type="text" autocomplete="off" placeholder="基于当前录音提问…" aria-label="基于当前录音向 AI 提问" />
        <button class="chat-send" type="submit" aria-label="发送问题" title="发送">${iconNode(Send)}</button>
      </form>
    </div>`;
}

interface TranscriptRenderOptions {
  partial?: boolean;
  pinned?: Set<number>;
  speakerNames?: SpeakerNames;
}

const VIRTUAL_THRESHOLD = 300;
const VIRTUAL_ROW_HEIGHT = 112;
const virtualCleanup = new WeakMap<HTMLElement, () => void>();

function buildTranscriptLine(
  segment: TranscriptSegment,
  index: number,
  total: number,
  options: TranscriptRenderOptions,
): HTMLElement {
  const line = document.createElement("article");
  line.className = "transcript-line";
  if (options.partial && index === total - 1) line.classList.add("partial");
  const pinned = options.pinned?.has(segment.startMs) ?? false;
  if (pinned) line.classList.add("pinned");
  line.dataset.startMs = String(segment.startMs);
  line.tabIndex = 0;
  line.setAttribute("role", "button");
  line.title = "跳转到这段录音";

  const meta = document.createElement("header");
  meta.className = "transcript-meta";
  const dot = document.createElement("span");
  dot.className = `speaker-dot speaker-${index % 4}`;
  const speaker = document.createElement("strong");
  speaker.className = "transcript-speaker speaker-editable";
  speaker.textContent = speakerLabel(segment, index, options.speakerNames);
  speaker.dataset.speakerKey = speakerKey(segment, index);
  speaker.title = "重命名说话人";
  speaker.setAttribute("role", "button");
  speaker.tabIndex = 0;
  const time = document.createElement("time");
  time.textContent = formatElapsed(segment.startMs);
  const state = document.createElement("span");
  state.className = "recognized-pill";
  state.textContent = options.partial && index === total - 1 ? "识别中" : "已识别";
  meta.append(dot, speaker, time, state);

  const content = document.createElement("p");
  content.className = "transcript-content";
  content.textContent = segment.text;

  const actions = document.createElement("div");
  actions.className = "segment-actions";
  actions.setAttribute("role", "group");
  actions.setAttribute("aria-label", "片段操作");
  actions.innerHTML = `
    <button class="segment-action segment-play" type="button" title="播放这一段" aria-label="播放这一段">${icon("play")}</button>
    <button class="segment-action segment-edit" type="button" title="编辑文本" aria-label="编辑文本">${iconNode(Pencil)}</button>
    <button class="segment-action segment-copy" type="button" title="复制" aria-label="复制这段文字">${iconNode(Copy)}</button>
    <button class="segment-action segment-pin${pinned ? " active" : ""}" type="button" title="${pinned ? "取消固定" : "固定片段"}" aria-label="${pinned ? "取消固定" : "固定片段"}" aria-pressed="${pinned}">${iconNode(Star)}</button>
    <button class="segment-action segment-delete" type="button" title="删除片段" aria-label="删除片段">${iconNode(Trash2)}</button>`;
  meta.append(actions);
  line.append(meta, content);
  return line;
}

function findScrollParent(element: HTMLElement): HTMLElement | null {
  let node: HTMLElement | null = element;
  while (node) {
    const overflow = getComputedStyle(node).overflowY;
    if (overflow === "auto" || overflow === "scroll") return node;
    node = node.parentElement;
  }
  return null;
}

function renderVirtualTranscript(
  container: HTMLElement,
  transcript: Transcript,
  options: TranscriptRenderOptions,
): void {
  const { segments } = transcript;
  container.classList.add("virtualized-transcript");
  const wrapper = document.createElement("div");
  wrapper.className = "virtual-transcript";
  wrapper.style.height = `${segments.length * VIRTUAL_ROW_HEIGHT}px`;
  container.append(wrapper);

  const scroller = findScrollParent(container);
  let first = -1;
  let last = -1;
  let frame = 0;

  const bounds = (): { start: number; end: number } => {
    if (!scroller) return { start: 0, end: Math.min(segments.length, 12) };
    const containerRect = container.getBoundingClientRect();
    const scrollerRect = scroller.getBoundingClientRect();
    const offset = container === scroller
      ? 0
      : containerRect.top - scrollerRect.top + scroller.scrollTop;
    const viewTop = Math.max(0, scroller.scrollTop - offset);
    const viewHeight = scroller.clientHeight || 640;
    return {
      start: Math.max(0, Math.floor(viewTop / VIRTUAL_ROW_HEIGHT) - 3),
      end: Math.min(segments.length, Math.ceil((viewTop + viewHeight) / VIRTUAL_ROW_HEIGHT) + 3),
    };
  };

  const paint = (): void => {
    const { start, end } = bounds();
    if (start === first && end === last) return;
    first = start;
    last = end;
    const fragment = document.createDocumentFragment();
    for (let index = start; index < end; index += 1) {
      const row = buildTranscriptLine(segments[index], index, segments.length, options);
      row.classList.add("virtual-row");
      row.style.top = `${index * VIRTUAL_ROW_HEIGHT}px`;
      fragment.append(row);
    }
    wrapper.replaceChildren(fragment);
  };

  paint();
  if (!scroller) return;

  const onScroll = (): void => {
    if (frame) return;
    frame = window.requestAnimationFrame(() => {
      frame = 0;
      paint();
    });
  };
  scroller.addEventListener("scroll", onScroll, { passive: true });
  let observer: ResizeObserver | undefined;
  if (typeof ResizeObserver !== "undefined") {
    observer = new ResizeObserver(() => paint());
    observer.observe(scroller);
  }
  virtualCleanup.set(container, () => {
    scroller.removeEventListener("scroll", onScroll);
    observer?.disconnect();
    if (frame) window.cancelAnimationFrame(frame);
  });
}

/// Renders provider segments with timestamps and speaker labels. When a saved
/// translation exists it is appended below the source text instead of
/// replacing it. Transcripts over 300 segments are windowed: only the visible
/// rows (plus overscan) exist in the DOM.
export function renderTranscript(
  container: HTMLElement,
  transcript: Transcript,
  translation?: { label: string; content: string },
  options: { partial?: boolean; pinned?: Set<number>; speakerNames?: SpeakerNames } = {},
): void {
  virtualCleanup.get(container)?.();
  virtualCleanup.delete(container);
  container.replaceChildren();
  container.classList.remove("virtualized-transcript");

  if (transcript.segments.length === 0) {
    const paragraph = document.createElement("p");
    paragraph.textContent = transcript.text;
    container.append(paragraph);
  } else if (transcript.segments.length > VIRTUAL_THRESHOLD) {
    renderVirtualTranscript(container, transcript, options);
  } else {
    const fragment = document.createDocumentFragment();
    transcript.segments.forEach((segment, index) => {
      fragment.append(buildTranscriptLine(segment, index, transcript.segments.length, options));
    });
    container.append(fragment);
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

export function applyInsights(root: HTMLElement, input: InsightInput): void {
  const topic = root.querySelector<HTMLElement>(".insight-topic-text");
  const points = root.querySelector<HTMLElement>(".insight-points");
  const todos = root.querySelector<HTMLElement>(".insight-todos-list");
  const todoCount = root.querySelector<HTMLElement>(".todo-count");
  const decisions = root.querySelector<HTMLElement>(".insight-decisions-list");
  const disputes = root.querySelector<HTMLElement>(".insight-disputes-text");
  const questions = root.querySelector<HTMLElement>(".insight-questions-list");
  if (!topic || !points || !todos || !decisions || !questions) return;

  const hasSegments = input.segments.length > 0;
  const derived = deriveInsights(input.notes, input.segments);
  const confirmations = input.confirmations ?? {};
  if (input.status === "idle") {
    topic.textContent = "开始录音后自动识别主题";
  } else if (input.status === "finished") {
    topic.textContent = input.title || "本次录音";
  } else {
    topic.textContent = input.title && input.title !== "新录音" ? input.title : (hasSegments ? "实时主题识别中" : "等待首段转写");
  }

  if (derived.points.length === 0) {
    points.innerHTML = `<li class="insight-placeholder">转写后自动提炼关键观点</li>`;
  } else {
    points.innerHTML = derived.points.map((point) => {
      const time = point.startMs === undefined ? "" : formatElapsed(point.startMs);
      const source = point.startMs === undefined
        ? ""
        : `<button class="insight-source" type="button" data-start-ms="${point.startMs}" title="跳转到这段录音">${escapeHtml(time)}</button>`;
      return `<li><p>${escapeHtml(point.text)}</p>${source}</li>`;
    }).join("");
  }

  const parsedTasks = derived.tasks;
  todoCount!.textContent = `${parsedTasks.length} 项`;
  todos.innerHTML = parsedTasks.length > 0
    ? parsedTasks.slice(0, 4).map((task) => {
      const entry = confirmationEntry(confirmations, "task", task.text);
      const text = effectiveText(entry, task.text);
      const status = task.done
        ? `<span class="task-status">已完成</span>`
        : entry?.state === "confirmed"
          ? `<span class="task-status confirmed">已确认</span>`
          : entry?.state === "ignored"
            ? `<span class="task-status ignored">已忽略</span>`
            : confirmButton("task", task.text);
      return `<li data-confirm-key="${confirmationKey("task", task.text)}"><span class="task-check">${iconNode(Check)}</span><p>${escapeHtml(text)}</p>${status}</li>`;
    }).join("")
    : `<li class="insight-placeholder">${input.status === "finished" ? "暂未识别到待办" : "录音结束后提取待办"}</li>`;

  decisions.innerHTML = derived.decisions.length > 0
    ? derived.decisions.slice(0, 3).map((line) => {
      const entry = confirmationEntry(confirmations, "decision", line);
      const text = effectiveText(entry, line);
      const trailing = entry?.state === "confirmed"
        ? `<span class="insight-badge confirmed">已确认</span>`
        : entry?.state === "ignored"
          ? `<span class="insight-badge ignored">已忽略</span>`
          : `<span class="insight-badge pending">待确认</span>${confirmButton("decision", line)}`;
      return `<li data-confirm-key="${confirmationKey("decision", line)}"><p>${escapeHtml(text)}</p>${trailing}</li>`;
    }).join("")
    : `<li class="insight-placeholder">尚未形成明确决策</li>`;

  if (disputes) {
    disputes.textContent = derived.disputes.length > 0
      ? derived.disputes[0]
      : "目前尚未检测到明确分歧。";
  }

  questions.innerHTML = derived.questions.length > 0
    ? derived.questions.slice(0, 3).map((line) => `<li><p>${escapeHtml(line)}</p></li>`).join("")
    : `<li class="insight-placeholder">${hasSegments ? "暂无待确认问题" : "录音开始后提取待确认问题"}</li>`;

  root.querySelector<HTMLElement>(".insight-panel")?.classList.toggle("has-content", hasSegments || derived.highlights.length > 0);
}

function confirmButton(kind: "decision" | "task", text: string): string {
  return `<button class="insight-confirm" type="button" data-confirm-kind="${kind}" data-confirm-text="${escapeHtml(text)}" data-confirm-state="confirmed">确认</button>`;
}

export function applyTasks(root: HTMLElement, notes: string): void {
  const tasks = parseTasks(notes);
  const content = root.querySelector<HTMLElement>(".tasks-content");
  const empty = root.querySelector<HTMLElement>(".tasks-empty");
  if (!content || !empty) return;
  content.hidden = tasks.length === 0;
  empty.hidden = tasks.length > 0;
  if (tasks.length === 0) return;
  content.innerHTML = `<ul>${tasks.map((task) => `<li class="${task.done ? "done" : ""}"><span>${iconNode(Check)}</span><p>${escapeHtml(task.text)}</p><small>${task.done ? "已完成" : "待确认"}</small></li>`).join("")}</ul>`;
}

function escapeHtml(value: string): string {
  const replacements: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
  return value.replace(/[&<>"']/g, (character) => replacements[character]);
}
