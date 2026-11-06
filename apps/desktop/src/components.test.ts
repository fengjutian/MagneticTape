import { beforeEach, describe, expect, it } from "vitest";
import { applyView, formatElapsed, renderApp, renderTranscript } from "./components";
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
    expect(lines[0].querySelector("strong")!.textContent).toContain("S1");
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

  it("falls back to plain text when no segments were returned", () => {
    const container = document.createElement("div");
    renderTranscript(container, { ...transcript, segments: [] });
    expect(container.querySelector(".transcript-line")).toBeNull();
    expect(container.textContent).toContain("大家好。");
  });
});
