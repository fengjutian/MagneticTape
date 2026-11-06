import { describe, expect, it } from "vitest";
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
} from "./transcription";

describe("transcription state machine", () => {
  it("starts in a cancellable busy state", () => {
    const state = beginTranscription();
    expect(state.phase).toBe("starting");
    expect(isTranscriptionBusy(state)).toBe(true);
    expect(canCancelTranscription(state)).toBe(true);
    expect(transcriptionStatus(state)).toBe("正在解码并混音…");
  });

  it("tracks progress and clamps counters", () => {
    let state = applyTranscriptionProgress(beginTranscription(), { done: 2, total: 8 });
    expect(state).toMatchObject({ phase: "progress", done: 2, total: 8 });
    expect(transcriptionStatus(state)).toBe("正在转写 2/8 段");

    state = applyTranscriptionProgress(state, { done: 99, total: 8 });
    expect(state.done).toBe(8);

    state = applyTranscriptionProgress(state, { done: -3, total: 8 });
    expect(state.done).toBe(0);
  });

  it("formats progress without a known total", () => {
    const state = applyTranscriptionProgress(beginTranscription(), { done: 3, total: 0 });
    expect(transcriptionStatus(state)).toBe("正在转写 3 段");
  });

  it("ignores progress once finished", () => {
    const done = transcriptionSucceeded(beginTranscription());
    expect(applyTranscriptionProgress(done, { done: 1, total: 2 })).toBe(done);
    expect(requestTranscriptionCancel(done)).toBe(done);
    expect(canCancelTranscription(done)).toBe(false);
  });

  it("succeeds and clears the status line", () => {
    const state = transcriptionSucceeded(
      applyTranscriptionProgress(beginTranscription(), { done: 3, total: 3 }),
    );
    expect(state.phase).toBe("done");
    expect(transcriptionStatus(state)).toBeNull();
    expect(isTranscriptionBusy(state)).toBe(false);
    expect(transcriptionError(state)).toBeNull();
  });

  it("treats cancellation as a normal outcome", () => {
    let state = requestTranscriptionCancel(beginTranscription());
    expect(state.phase).toBe("cancelling");
    expect(transcriptionStatus(state)).toBe("正在取消…");
    expect(canCancelTranscription(state)).toBe(false);

    state = transcriptionFailed(state, "已取消转写");
    expect(state.phase).toBe("cancelled");
    expect(state.error).toBeNull();
    expect(transcriptionStatus(state)).toBe("已取消转写");
    expect(transcriptionError(state)).toBeNull();
  });

  it("exposes failures as an error message", () => {
    const state = transcriptionFailed(beginTranscription(), "网络中断");
    expect(state.phase).toBe("error");
    expect(transcriptionStatus(state)).toBe("转写失败");
    expect(transcriptionError(state)).toBe("转写失败：网络中断");
    expect(isTranscriptionBusy(state)).toBe(false);
  });

  it("keeps idle silent", () => {
    expect(transcriptionStatus(idleTranscription)).toBeNull();
    expect(transcriptionError(idleTranscription)).toBeNull();
    expect(isTranscriptionBusy(idleTranscription)).toBe(false);
    expect(canCancelTranscription(idleTranscription)).toBe(false);
    expect(requestTranscriptionCancel(idleTranscription)).toBe(idleTranscription);
  });
});
