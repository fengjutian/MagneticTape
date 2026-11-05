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

export interface AudioDeviceInfo {
  id: string;
  name: string;
  kind: "microphone" | "system_output";
  isDefault: boolean;
  channels: number;
  sampleRate: number;
  sampleFormat: string;
}

export interface AudioDeviceSnapshot {
  microphones: AudioDeviceInfo[];
  systemOutputs: AudioDeviceInfo[];
}

