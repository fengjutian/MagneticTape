import type {
  AppSettings,
  AsrConfiguration,
  AudioDeviceSnapshot,
  LocalWhisperStatus,
  SessionState,
  Transcript,
} from "../types";

/**
 * In-memory Tauri bridge used by Playwright browser tests.
 *
 * It implements the small `window.__TAURI_INTERNALS__` surface that
 * `@tauri-apps/api` relies on, so the real UI can run in Chromium without the
 * Rust backend. Only enabled when `VITE_E2E=1`.
 */

const SESSION_ID = "e2e-session-001";

interface MockState {
  status: SessionState["status"];
  title: string;
  startedAtEpochMs: number;
  elapsedMs: number;
  microphoneChunks: string[];
  systemChunks: string[];
  transcript: Transcript | null;
  notes: string;
  settings: AppSettings;
}

const defaultSettings: AppSettings = {
  microphoneId: null,
  systemOutputId: null,
  language: "auto",
  translation: "auto",
  retainAudio: true,
  liveCaptions: false,
  liveStreaming: true,
  asrProvider: "minimax",
  asrProfiles: {},
  localWhisperModel: "small",
  whisperHotwords: "",
  autoCleanupDays: 0,
  storageQuotaMb: 0,
};

function sessionView(state: MockState): SessionState {
  return {
    status: state.status,
    sessionId: state.status === "idle" ? null : SESSION_ID,
    startedAtEpochMs: state.startedAtEpochMs,
    elapsedMs: state.elapsedMs,
    microphoneReady: state.status !== "idle",
    systemAudioReady: state.status !== "idle",
    microphonePath: state.microphoneChunks[0] ?? null,
    systemPath: state.systemChunks[0] ?? null,
    microphoneChunks: state.microphoneChunks,
    systemChunks: state.systemChunks,
    droppedBlocks: 0,
    recovered: false,
    title: state.title,
  };
}

function fixtureTranscript(): Transcript {
  return {
    provider: "minimax",
    model: "E2E-ASR",
    language: "zh-CN",
    durationMs: 65_000,
    text: "大家好。\n开始吧。",
    segments: [
      { startMs: 0, endMs: 1_500, speaker: "S1", text: "大家好。" },
      { startMs: 1_500, endMs: 3_200, text: "开始吧。" },
    ],
    createdAtEpochMs: Date.now(),
  };
}

function asrConfigurations(settings: AppSettings): AsrConfiguration[] {
  const base = {
    endpoint: "https://api.minimax.cn/v1/speech_to_text",
    model: "asr-1.0",
    language: "auto",
    configured: settings.asrProvider !== "local_whisper",
  };
  return [
    { ...base, provider: "minimax" },
    { ...base, provider: "silicon_flow", endpoint: "https://api.siliconflow.cn/v1/audio/transcriptions", configured: false },
    { ...base, provider: "openai_realtime", endpoint: "wss://api.openai.com/v1/realtime", configured: false },
    { ...base, provider: "local_whisper", endpoint: "local://whisper.cpp", model: "small", configured: true },
  ];
}

function localWhisperStatus(): LocalWhisperStatus {
  return {
    engineInstalled: true,
    engineBytes: 23_147_520,
    engineVersion: "b5454",
    engine: "cpu",
    engines: [
      { id: "cpu", label: "CPU 引擎", sizeBytes: 8_928_640, installed: true },
      { id: "cuda", label: "CUDA 引擎", sizeBytes: 284_761_231, installed: false },
    ],
    gpuName: "E2E NVIDIA GPU",
    model: "small",
    modelInstalled: true,
    models: [
      { id: "tiny", label: "Tiny", sizeBytes: 77_691_713, installed: false },
      { id: "small", label: "Small", sizeBytes: 487_601_967, installed: true },
    ],
    totalBytes: 510_749_487,
  };
}

function audioDevices(): AudioDeviceSnapshot {
  return {
    microphones: [
      {
        id: "mic-e2e",
        name: "E2E 麦克风",
        kind: "microphone",
        isDefault: true,
        channels: 1,
        sampleRate: 48_000,
        sampleFormat: "f32",
      },
    ],
    systemOutputs: [
      {
        id: "sys-e2e",
        name: "E2E 系统声音",
        kind: "system_output",
        isDefault: true,
        channels: 2,
        sampleRate: 48_000,
        sampleFormat: "f32",
      },
    ],
  };
}

export function installTauriMock(): void {
  const state: MockState = {
    status: "idle",
    title: "E2E 周会",
    startedAtEpochMs: 0,
    elapsedMs: 0,
    microphoneChunks: [],
    systemChunks: [],
    transcript: null,
    notes: "",
    settings: structuredClone(defaultSettings),
  };
  let nextCallbackId = 1;
  const callbacks = new Map<number, (payload: unknown) => void>();

  const dispatch = async (command: string, args: Record<string, unknown>): Promise<unknown> => {
    switch (command) {
      case "get_session_state":
        return sessionView(state);
      case "recover_latest_session":
        return null;
      case "get_settings":
        return structuredClone(state.settings);
      case "save_settings":
        state.settings = { ...state.settings, ...structuredClone(args.settings as Partial<AppSettings>) };
        return structuredClone(state.settings);
      case "list_audio_devices":
        return audioDevices();
      case "get_audio_levels":
        return { microphone: 0.24, system: 0.12 };
      case "get_audio_health":
        return { active: state.status === "recording", paused: false, stalled: false, needsRebuild: false };
      case "start_session":
        state.status = "recording";
        state.startedAtEpochMs = Date.now();
        state.elapsedMs = 0;
        state.title = (args.title as string) || state.title;
        state.microphoneChunks = [];
        state.systemChunks = [];
        state.transcript = null;
        return sessionView(state);
      case "pause_session":
        state.status = "paused";
        return sessionView(state);
      case "resume_session":
        state.status = "recording";
        return sessionView(state);
      case "stop_session":
        state.status = "finished";
        state.elapsedMs = 65_000;
        state.microphoneChunks = ["mic-00000.ogg", "mic-00001.ogg"];
        state.systemChunks = ["sys-00000.ogg", "sys-00001.ogg"];
        return sessionView(state);
      case "set_session_title":
        state.title = args.title as string;
        return sessionView(state);
      case "get_transcript":
        return state.transcript;
      case "transcribe_session":
        state.transcript = fixtureTranscript();
        return state.transcript;
      case "get_translation":
        return null;
      case "translate_transcript":
        return "大家好（译文）。";
      case "get_session_notes":
        return state.notes;
      case "save_session_notes":
        state.notes = args.content as string;
        return null;
      case "cancel_transcription":
        return true;
      case "export_session":
        return {
          directory: "C:\\Users\\e2e\\Documents\\MagneticTape\\e2e",
          files: ["meeting.md", "transcript.txt", "audio.wav"],
        };
      case "reveal_export":
        return null;
      case "get_session_playback":
        return {
          durationMs: 65_000,
          microphone: [{ path: "mic-00000.ogg", startMs: 0, durationMs: 5_000 }],
          system: [{ path: "sys-00000.ogg", startMs: 0, durationMs: 5_000 }],
        };
      case "get_library_stats":
        return { sessionCount: 0, totalBytes: 0, audioBytes: 0, transcriptOnlyCount: 0 };
      case "list_recordings":
        return [];
      case "cleanup_audio_older_than":
        return { sessions: 0, bytesFreed: 0 };
      case "discard_session_audio":
        state.microphoneChunks = [];
        state.systemChunks = [];
        return sessionView(state);
      case "get_asr_configurations":
        return asrConfigurations(state.settings);
      case "configure_asr":
        return {
          provider: args.provider,
          endpoint: args.endpoint ?? "https://api.minimax.cn/v1/speech_to_text",
          model: args.model ?? "asr-1.0",
          language: args.language ?? "auto",
          configured: true,
        };
      case "clear_asr":
        return null;
      case "get_ai_configurations":
        return [{ provider: "minimax", model: "MiniMax-M3.1-Flash", configured: true }];
      case "test_ai":
      case "generate_ai":
        return {
          provider: args.provider ?? "minimax",
          model: "E2E-AI",
          content: "E2E 纪要",
        };
      case "get_local_whisper_status":
      case "download_local_whisper_engine":
      case "download_local_whisper_model":
      case "delete_local_whisper_model":
        return localWhisperStatus();
      case "get_sherpa_status":
      case "download_sherpa":
        return {
          engineInstalled: true,
          engineBytes: 24_139_676,
          engineVersion: "1.12.9",
          asrModelInstalled: true,
          asrModel: "zh-14m",
          asrModelLabel: "中文流式 Zipformer 14M",
          asrModelBytes: 74_004_050,
          speakerModelInstalled: true,
          speakerModelBytes: 28_281_138,
          diarizationInstalled: true,
          diarizationModelBytes: 6_958_444,
          totalBytes: 133_384_308,
          ready: true,
        };
      case "start_live_transcription":
        return {
          active: true,
          pendingChunks: 0,
          processedChunks: 0,
          segments: [],
          model: "E2E",
          language: "auto",
          partialText: "",
          streaming: true,
        };
      case "get_live_transcript":
      case "poll_live_transcription":
        return {
          active: false,
          pendingChunks: 0,
          processedChunks: 0,
          segments: [],
          model: "E2E",
          language: "auto",
          partialText: "",
          streaming: true,
        };
      case "stop_live_transcription":
        return null;
      default:
        if (command.startsWith("plugin:event|")) {
          return command === "plugin:event|listen" ? 1 : null;
        }
        console.warn(`[e2e] unhandled invoke: ${command}`);
        return null;
    }
  };

  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
    invoke: dispatch,
    transformCallback: (callback: (payload: unknown) => void) => {
      const id = nextCallbackId++;
      callbacks.set(id, callback);
      return id;
    },
    unregisterCallback: (id: number) => callbacks.delete(id),
    convertFileSrc: (path: string) => path,
  };
}
