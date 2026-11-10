import type { AppSettings, AsrConfiguration, AsrProviderId } from "./types";

const LANGUAGES = ["auto", "zh-CN", "en-US"];
const TRANSLATIONS = ["auto", "none", "zh-CN", "en-US"];
const ASR_PROVIDERS: AsrProviderId[] = [
  "minimax",
  "silicon_flow",
  "openai_realtime",
  "local_whisper",
];
const CLEANUP_DAYS = [0, 30, 90, 180];
const QUOTA_MB = [0, 5120, 10240, 20480];
const MAX_HOTWORDS = 512;

export interface SettingsUiSnapshot {
  microphoneId: string | null;
  systemOutputId: string | null;
  language: string;
  translation: string;
  retainAudio: boolean;
  liveCaptions: boolean;
  liveStreaming: boolean;
  autoCleanupDays: number;
  storageQuotaMb: number;
}

/// Merges backend settings into the local copy, falling back to safe defaults
/// for missing or unknown values (the backend normally sanitizes these).
export function mergeSettings(
  current: AppSettings,
  incoming: Partial<AppSettings> | null | undefined,
): AppSettings {
  const next = { ...current, ...(incoming ?? {}) };
  const language = LANGUAGES.includes(next.language) ? next.language : "auto";
  const translation = TRANSLATIONS.includes(next.translation) ? next.translation : "auto";
  const asrProvider = ASR_PROVIDERS.includes(next.asrProvider) ? next.asrProvider : "minimax";
  return {
    ...next,
    microphoneId: next.microphoneId || null,
    systemOutputId: next.systemOutputId || null,
    language,
    translation,
    retainAudio: next.retainAudio !== false,
    liveCaptions: Boolean(next.liveCaptions),
    liveStreaming: next.liveStreaming !== false,
    asrProvider,
    asrProfiles: next.asrProfiles ?? {},
    localWhisperModel: next.localWhisperModel || "small",
    whisperHotwords: (next.whisperHotwords ?? "").trim().slice(0, MAX_HOTWORDS),
    autoCleanupDays: CLEANUP_DAYS.includes(next.autoCleanupDays) ? next.autoCleanupDays : 0,
    storageQuotaMb: QUOTA_MB.includes(next.storageQuotaMb) ? next.storageQuotaMb : 0,
  };
}

/// Builds the settings payload from the current UI state without mutating it.
export function settingsFromUi(current: AppSettings, ui: SettingsUiSnapshot): AppSettings {
  return mergeSettings(current, ui);
}

/// Switches to local Whisper and stores its model / hotwords.
export function withLocalWhisper(
  current: AppSettings,
  model: string,
  hotwords: string,
): AppSettings {
  return mergeSettings(current, {
    asrProvider: "local_whisper",
    localWhisperModel: model || current.localWhisperModel,
    whisperHotwords: hotwords,
  });
}

/// Records a cloud provider configuration returned by `configure_asr`.
export function withCloudProfile(
  current: AppSettings,
  configuration: AsrConfiguration,
): AppSettings {
  return mergeSettings(current, {
    asrProvider: configuration.provider,
    asrProfiles: {
      ...current.asrProfiles,
      [configuration.provider]: {
        endpoint: configuration.endpoint,
        model: configuration.model,
        language: configuration.language,
      },
    },
  });
}

/// Picks the provider used by live captions: local when selected, otherwise a
/// configured cloud provider, falling back to MiniMax.
export function resolveLiveProvider(
  settings: AppSettings,
  configurations: AsrConfiguration[],
): AsrProviderId {
  if (settings.asrProvider === "local_whisper") return "local_whisper";
  return (
    configurations.find(
      (configuration) => configuration.provider === settings.asrProvider && configuration.configured,
    )?.provider ??
    configurations.find((configuration) => configuration.configured)?.provider ??
    "minimax"
  );
}

/// Values mirrored into the recording view when settings are loaded.
export function viewSettings(settings: AppSettings) {
  return {
    language: settings.language,
    translation: settings.translation,
    retainAudio: settings.retainAudio,
    liveCaptions: settings.liveCaptions,
    liveStreaming: settings.liveStreaming,
  };
}
