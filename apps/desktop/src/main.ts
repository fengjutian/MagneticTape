import { invoke } from "@tauri-apps/api/core";
import "./styles.css";

type SessionStatus = "idle" | "recording";

interface SessionState {
  status: SessionStatus;
  sessionId: string | null;
  startedAtEpochMs: number | null;
  microphoneReady: boolean;
  systemAudioReady: boolean;
}

const app = document.querySelector<HTMLElement>("#app");
if (!app) throw new Error("Missing application root");

app.innerHTML = `
  <section class="shell">
    <header class="topbar">
      <div class="brand">
        <span class="reel" aria-hidden="true"><i></i><i></i><i></i></span>
        <div><strong>MagneticTape</strong><small>会议声音，留在此刻</small></div>
      </div>
      <span class="build-tag">LOCAL · ALPHA</span>
    </header>

    <section class="hero">
      <p class="eyebrow">NEW RECORDING</p>
      <h1>准备记录这场谈话</h1>
      <p class="lede">麦克风与系统声音将保持独立时间轴，便于后续转写和定位回听。</p>

      <div class="source-grid">
        <article class="source-card" data-source="microphone">
          <span class="source-icon">MIC</span>
          <div><strong>麦克风</strong><small>等待 WASAPI 接入</small></div>
          <span class="status-dot"></span>
        </article>
        <article class="source-card" data-source="system">
          <span class="source-icon">SYS</span>
          <div><strong>系统声音</strong><small>等待 Loopback 接入</small></div>
          <span class="status-dot"></span>
        </article>
      </div>

      <div class="recorder">
        <div class="timer" aria-live="polite">00:00:00</div>
        <div class="track" aria-hidden="true">
          ${Array.from({ length: 42 }, (_, index) => `<i style="--n:${index}"></i>`).join("")}
        </div>
        <button class="record-button" type="button">
          <span></span><b>开始记录</b>
        </button>
        <p class="notice">当前版本仅建立桌面会话，尚不会采集或保存音频。</p>
      </div>
    </section>

    <footer>
      <span>48 kHz · 双音源时间轴</span>
      <span id="session-label">无活动会话</span>
    </footer>
  </section>
`;

const button = app.querySelector<HTMLButtonElement>(".record-button")!;
const timer = app.querySelector<HTMLElement>(".timer")!;
const sessionLabel = app.querySelector<HTMLElement>("#session-label")!;
let state: SessionState;
let timerHandle: number | undefined;

function formatElapsed(startedAt: number | null): string {
  if (startedAt === null) return "00:00:00";
  const seconds = Math.max(0, Math.floor((Date.now() - startedAt) / 1000));
  const hours = Math.floor(seconds / 3600).toString().padStart(2, "0");
  const minutes = Math.floor((seconds % 3600) / 60).toString().padStart(2, "0");
  const remainder = (seconds % 60).toString().padStart(2, "0");
  return `${hours}:${minutes}:${remainder}`;
}

function render(next: SessionState): void {
  state = next;
  const recording = state.status === "recording";
  document.body.classList.toggle("is-recording", recording);
  button.classList.toggle("is-stop", recording);
  button.querySelector("b")!.textContent = recording ? "停止记录" : "开始记录";
  sessionLabel.textContent = state.sessionId ? `会话 ${state.sessionId.slice(0, 8)}` : "无活动会话";
  timer.textContent = formatElapsed(state.startedAtEpochMs);

  window.clearInterval(timerHandle);
  if (recording) {
    timerHandle = window.setInterval(() => {
      timer.textContent = formatElapsed(state.startedAtEpochMs);
    }, 250);
  }
}

button.addEventListener("click", async () => {
  button.disabled = true;
  try {
    const command = state.status === "recording" ? "stop_session" : "start_session";
    render(await invoke<SessionState>(command));
  } catch (error) {
    console.error(error);
  } finally {
    button.disabled = false;
  }
});

render(await invoke<SessionState>("get_session_state"));

