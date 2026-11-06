import type { JobProgress } from "./types";

export type TranscriptionPhase =
  | "idle"
  | "starting"
  | "progress"
  | "cancelling"
  | "done"
  | "cancelled"
  | "error";

export interface TranscriptionState {
  phase: TranscriptionPhase;
  done: number;
  total: number;
  error: string | null;
}

export const idleTranscription: TranscriptionState = {
  phase: "idle",
  done: 0,
  total: 0,
  error: null,
};

export function beginTranscription(): TranscriptionState {
  return { phase: "starting", done: 0, total: 0, error: null };
}

/// Applies a backend progress event. Progress is ignored once the job is no
/// longer running, and out-of-range counters are clamped.
export function applyTranscriptionProgress(
  state: TranscriptionState,
  progress: Pick<JobProgress, "done" | "total">,
): TranscriptionState {
  if (state.phase !== "starting" && state.phase !== "progress") return state;
  const total = Math.max(0, Math.floor(progress.total));
  const done = Math.min(Math.max(0, Math.floor(progress.done)), total || Number.MAX_SAFE_INTEGER);
  return { ...state, phase: "progress", done, total, error: null };
}

export function requestTranscriptionCancel(state: TranscriptionState): TranscriptionState {
  if (state.phase !== "starting" && state.phase !== "progress") return state;
  return { ...state, phase: "cancelling" };
}

export function transcriptionSucceeded(state: TranscriptionState): TranscriptionState {
  if (state.phase !== "starting" && state.phase !== "progress" && state.phase !== "cancelling") {
    return state;
  }
  return { phase: "done", done: state.total, total: state.total, error: null };
}

/// `已取消转写` is a normal outcome, not an error banner.
export function transcriptionFailed(state: TranscriptionState, message: string): TranscriptionState {
  if (message.includes("已取消")) {
    return { ...state, phase: "cancelled", error: null };
  }
  return { ...state, phase: "error", error: message };
}

export function isTranscriptionBusy(state: TranscriptionState): boolean {
  return state.phase === "starting" || state.phase === "progress" || state.phase === "cancelling";
}

export function canCancelTranscription(state: TranscriptionState): boolean {
  return state.phase === "starting" || state.phase === "progress";
}

/// Text for the transcript status line; `null` means "leave it to the caller".
export function transcriptionStatus(state: TranscriptionState): string | null {
  switch (state.phase) {
    case "starting":
      return "正在解码并混音…";
    case "progress":
      return state.total > 0
        ? `正在转写 ${state.done}/${state.total} 段`
        : `正在转写 ${state.done} 段`;
    case "cancelling":
      return "正在取消…";
    case "cancelled":
      return "已取消转写";
    case "error":
      return "转写失败";
    case "done":
    case "idle":
      return null;
  }
}

/// User-visible error for failed runs; cancelled runs stay silent.
export function transcriptionError(state: TranscriptionState): string | null {
  if (state.phase !== "error" || !state.error) return null;
  return `转写失败：${state.error}`;
}
