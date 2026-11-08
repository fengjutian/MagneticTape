import { beforeEach, describe, expect, it } from "vitest";
import { applyInsights, applyTab, applyView, formatElapsed, renderApp, renderTranscript } from "./components";
import type { RecorderViewState, Transcript } from "./types";

const baseState: RecorderViewState = {
  title: "周会",
  status: "idle",
  elapsedMs: 0,
  language: "auto",
  translation: "none",
  retainAudio: true,
  volumeLevel: 0,
  sessionId: null,
  microphoneChunks: [],
  systemChunks: [],
};

function mount(state: RecorderViewState): HTMLElement {
  document.body.innerHTML = `<div id="app">${renderApp()}</div>`;
  const root = document.getElementById("app")!;
  applyView(root, state);
  return root;
}

beforeEach(() => {
  document.body.innerHTML = "";
});

describe("formatElapsed", () => {
  it("formats elapsed recording time", () => {
    expect(formatElapsed(0)).toBe("00:00");
    expect(formatElapsed(65_000)).toBe("01:05");
    expect(formatElapsed(3_600_000)).toBe("01:00:00");
    expect(formatElapsed(-1_000)).toBe("00:00");
  });
});

describe("applyView", () => {
  it("renders the idle state and disables recording controls", () => {
    const root = mount(baseState);
    expect(root.dataset.status).toBe("idle");
    expect(root.querySelector(".status-title")!.textContent).toBe("等待开始录音");
    expect((root.querySelector(".start-button") as HTMLElement).hidden).toBe(false);
    expect((root.querySelector(".pause-button") as HTMLButtonElement).disabled).toBe(true);
    expect((root.querySelector(".stop-button") as HTMLButtonElement).disabled).toBe(true);
  });

  it("switches to recording state", () => {
    const root = mount({ ...baseState, status: "recording", elapsedMs: 65_000, sessionId: "session-1" });
    expect(root.querySelector(".status-title")!.textContent).toBe("正在录音");
    expect(root.querySelector(".elapsed")!.textContent).toBe("01:05");
    expect((root.querySelector(".pause-button") as HTMLButtonElement).disabled).toBe(false);
    expect((root.querySelector(".start-button") as HTMLElement).hidden).toBe(true);
    expect(root.querySelector(".transcript-placeholder")).not.toBeNull();
  });

  it("renders the finished workbench with local Whisper available", () => {
    const root = mount({
      ...baseState,
      status: "finished",
      sessionId: "001122334455",
      microphoneChunks: ["mic-0.ogg"],
      systemChunks: ["sys-0.ogg"],
    });
    expect(root.querySelector(".status-title")!.textContent).toBe("录音已保存");
    expect(root.querySelector(".playback")).not.toBeNull();
    expect(root.querySelector(".transcript-panel")).not.toBeNull();
    const provider = root.querySelector<HTMLSelectElement>(".asr-provider")!;
    expect(Array.from(provider.options).map((option) => option.value)).toContain("local_whisper");
    expect(root.querySelector(".meeting-notes")).not.toBeNull();
    expect(root.querySelector(".export-session")).not.toBeNull();
  });

  it("labels recovered sessions", () => {
    const root = mount({ ...baseState, status: "finished", recovered: true, sessionId: "s" });
    expect(root.querySelector(".status-title")!.textContent).toBe("已恢复上次录音");
  });
});

describe("workspace", () => {
  it("switches tabs and exposes one panel at a time", () => {
    const root = mount(baseState);
    applyTab(root, "analysis");
    expect(root.dataset.tab).toBe("analysis");
    expect(root.querySelector<HTMLElement>('.tab-panel[data-panel="analysis"]')!.hidden).toBe(false);
    expect(root.querySelector<HTMLElement>('.tab-panel[data-panel="transcript"]')!.hidden).toBe(true);
    expect(root.querySelector<HTMLButtonElement>('.workspace-tab[data-tab="analysis"]')!.getAttribute("aria-selected")).toBe("true");
    expect(root.querySelector<HTMLElement>(".content-toolbar")!.hidden).toBe(true);
  });

  it("renders source-linked points and checklist tasks in the insight panel", () => {
    const root = mount(baseState);
    applyInsights(root, {
      title: "技术评审",
      status: "finished",
      segments: [
        { startMs: 0, endMs: 1_000, text: "系统稳定性优先。" },
        { startMs: 65_000, endMs: 66_000, text: "分阶段推进功能开发。" },
      ],
      notes: "- [ ] 制定技术方案\n- [x] 评估技术方案",
    });
    expect(root.querySelector(".insight-topic-text")!.textContent).toBe("技术评审");
    expect(root.querySelectorAll(".insight-points li")).toHaveLength(2);
    expect(root.querySelector(".insight-points")!.textContent).toContain("00:00");
    expect(root.querySelector(".todo-count")!.textContent).toBe("2 项");
    expect(root.querySelector(".insight-todos-list")!.textContent).toContain("评估技术方案");
  });

  it("separates decisions, disputes and open questions", () => {
    const root = mount(baseState);
    applyInsights(root, {
      title: "技术评审",
      status: "finished",
      segments: [{ startMs: 0, endMs: 1_000, text: "先修复稳定性。" }],
      notes: "- 决策：先修复稳定性\n- 争议：是否延后新功能\n- 验收指标是什么？",
    });
    expect(root.querySelector(".insight-decisions-list")!.textContent).toContain("先修复稳定性");
    expect(root.querySelector(".insight-disputes-text")!.textContent).toContain("是否延后新功能");
    expect(root.querySelector(".insight-questions-list")!.textContent).toContain("验收指标");
  });

  it("shows the compact scene summary and chunk count while recording", () => {
    const root = mount({
      ...baseState,
      status: "recording",
      sessionId: "session-1",
      microphoneChunks: ["mic-0.ogg", "mic-1.ogg"],
      systemChunks: ["sys-0.ogg"],
    });
    expect(root.querySelector<HTMLElement>(".scene-compact")!.hidden).toBe(false);
    expect(root.querySelector(".chunk-count")!.textContent).toBe("3");
    expect(root.querySelector(".header-elapsed")!.textContent).toBe("00:00");
  });
});

describe("renderTranscript", () => {
  const transcript: Transcript = {
    provider: "local_whisper",
    model: "tiny",
    language: "zh-CN",
    durationMs: 3_000,
    text: "大家好。\n开始吧。",
    segments: [
      { startMs: 0, endMs: 1_500, speaker: "S1", text: "大家好。" },
      { startMs: 1_500, endMs: 3_200, text: "开始吧。" },
    ],
    createdAtEpochMs: 0,
  };

  it("renders timestamps, speakers and seek metadata", () => {
    const container = document.createElement("div");
    renderTranscript(container, transcript);
    const lines = container.querySelectorAll<HTMLElement>(".transcript-line");
    expect(lines).toHaveLength(2);
    expect(lines[0].dataset.startMs).toBe("0");
    expect(lines[0].querySelector("time")!.textContent).toBe("00:00");
    expect(lines[0].querySelector("strong")!.textContent).toBe("发言人 A");
    expect(lines[1].dataset.startMs).toBe("1500");
  });

  it("appends a saved translation without replacing the source", () => {
    const container = document.createElement("div");
    renderTranscript(container, transcript, { label: "English", content: "Hello everyone." });
    expect(container.querySelectorAll(".transcript-line")).toHaveLength(2);
    const translation = container.querySelector(".transcript-translation-block");
    expect(translation).not.toBeNull();
    expect(translation!.textContent).toContain("English");
    expect(translation!.textContent).toContain("Hello everyone.");
  });

  it("renders per-segment actions and pinned state", () => {
    const container = document.createElement("div");
    renderTranscript(container, transcript, undefined, { pinned: new Set([1_500]) });
    const lines = container.querySelectorAll<HTMLElement>(".transcript-line");
    expect(lines[0].querySelector(".segment-actions")).not.toBeNull();
    expect(lines[0].querySelectorAll(".segment-action")).toHaveLength(5);
    expect(lines[1].classList.contains("pinned")).toBe(true);
    expect(lines[1].querySelector(".segment-pin")!.getAttribute("aria-pressed")).toBe("true");
    expect(lines[0].querySelector(".segment-pin")!.getAttribute("aria-pressed")).toBe("false");
  });

  it("renders long transcripts in full instead of clipping rows", () => {
    const segments = Array.from({ length: 420 }, (_, index) => ({
      startMs: index * 1_000,
      endMs: index * 1_000 + 900,
      text: `第 ${index} 段内容`,
    }));
    const container = document.createElement("div");
    renderTranscript(container, { ...transcript, segments });
    expect(container.querySelector(".virtual-transcript")).toBeNull();
    const rows = container.querySelectorAll(".transcript-line");
    expect(rows).toHaveLength(420);
    expect(rows[419].textContent).toContain("第 419 段内容");
  });

  it("falls back to plain text when no segments were returned", () => {
    const container = document.createElement("div");
    renderTranscript(container, { ...transcript, segments: [] });
    expect(container.querySelector(".transcript-line")).toBeNull();
    expect(container.textContent).toContain("大家好。");
  });
});
