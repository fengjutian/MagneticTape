export type RecorderStatus = "idle" | "recording" | "paused" | "stopping" | "finished" | "error";

export interface SessionState {
  status: Exclude<RecorderStatus, "stopping" | "error">;
  sessionId: string | null;
  startedAtEpochMs: number | null;
  elapsedMs: number;
  microphoneReady: boolean;
  systemAudioReady: boolean;
  microphonePath: string | null;
  systemPath: string | null;
  microphoneChunks: string[];
  systemChunks: string[];
  droppedBlocks: number;
  recovered: boolean;
  title: string;
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
  microphonePath?: string | null;
  systemPath?: string | null;
  microphoneChunks?: string[];
  systemChunks?: string[];
  droppedBlocks?: number;
  recovered?: boolean;
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

export interface AudioLevels {
  microphone: number;
  system: number;
}

export interface AiResponse {
  provider: "minimax" | "deep_seek";
  model: string;
  content: string;
}

export interface AiConfiguration {
  provider: "minimax" | "deep_seek";
  model: string;
  configured: boolean;
}

export interface RecordingListItem {
  sessionId: string;
  title: string;
  startedAtEpochMs: number;
  durationMs: number;
  status: "completed" | "recovered" | "interrupted";
  microphonePath: string | null;
  systemPath: string | null;
  sizeBytes: number;
}

export interface TranscriptSegment {
  startMs: number;
  endMs: number;
  speaker?: string;
  text: string;
}

export interface Transcript {
  provider: string;
  model: string;
  language: string;
  durationMs: number;
  text: string;
  segments: TranscriptSegment[];
  createdAtEpochMs: number;
}

export interface AsrConfiguration {
  provider: "minimax" | "silicon_flow";
  endpoint: string;
  model: string;
  language: string;
  configured: boolean;
}

export interface LiveTranscript {
  active: boolean;
  segments: TranscriptSegment[];
  provider?: "minimax" | "silicon_flow";
  model: string;
  language: string;
  error?: string;
}

export interface AsrProfile {
  endpoint: string;
  model: string;
  language: string;
}

export interface AppSettings {
  microphoneId: string | null;
  systemOutputId: string | null;
  language: string;
  translation: string;
  retainAudio: boolean;
  liveCaptions: boolean;
  asrProvider: "minimax" | "silicon_flow";
  asrProfiles: Record<string, AsrProfile>;
}

export interface AudioHealth {
  active: boolean;
  paused: boolean;
  stalled: boolean;
  microphoneError?: string;
  systemError?: string;
  needsRebuild: boolean;
}

export interface ExportResult {
  directory: string;
  files: string[];
}

