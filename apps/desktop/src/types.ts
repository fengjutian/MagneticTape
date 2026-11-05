export type RecorderStatus = "idle" | "recording" | "paused" | "stopping" | "finished" | "error";

export interface SessionState {
  status: Exclude<RecorderStatus, "stopping" | "error">;
  sessionId: string | null;
  startedAtEpochMs: number | null;
  elapsedMs: number;
  microphoneReady: boolean;
  systemAudioReady: boolean;
}

export interface RecorderViewState {
  title: string;
  status: RecorderStatus;
  elapsedMs: number;
  language: string;
  translation: string;
  retainAudio: boolean;
  volumeLevel: number;
  sessionId: string | null;
  errorMessage?: string;
}

