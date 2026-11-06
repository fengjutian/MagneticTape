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
  status: "completed" | "recovered" | "interrupted" | "transcript_only";
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
  provider: "minimax" | "silicon_flow" | "openai_realtime";
  endpoint: string;
  model: string;
  language: string;
  configured: boolean;
}

export interface LiveTranscript {
  active: boolean;
  pendingChunks: number;
  processedChunks: number;
  segments: TranscriptSegment[];
  provider?: "minimax" | "silicon_flow";
  model: string;
  language: string;
  error?: string;
  partialText: string;
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
  asrProvider: "minimax" | "silicon_flow" | "openai_realtime";
  asrProfiles: Record<string, AsrProfile>;
  autoCleanupDays: number;
  storageQuotaMb: number;
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

/// Progress emitted by the backend while transcription, translation or
/// minutes generation is running.
export interface JobProgress {
  stage: "transcribe" | "translate" | "summarize";
  done: number;
  total: number;
}

export interface SavedTranslation {
  code: string;
  content: string;
}

/// One 15 second Opus chunk placed on the shared session timeline.
export interface PlaybackChunk {
  path: string;
  startMs: number;
  durationMs: number;
}

/// Both recording tracks on one timeline, used by the unified player.
export interface PlaybackTracks {
  durationMs: number;
  microphone: PlaybackChunk[];
  system: PlaybackChunk[];
}

export interface LibraryStats {
  sessionCount: number;
  totalBytes: number;
  audioBytes: number;
  transcriptOnlyCount: number;
}

export interface CleanupResult {
  sessions: number;
  bytesFreed: number;
}

