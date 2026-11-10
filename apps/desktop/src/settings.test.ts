import { describe, expect, it } from "vitest";
import {
  mergeSettings,
  resolveLiveProvider,
  settingsFromUi,
  viewSettings,
  withCloudProfile,
  withLocalWhisper,
} from "./settings";
import type { AppSettings, AsrConfiguration } from "./types";

const baseSettings: AppSettings = {
  microphoneId: "mic-1",
  systemOutputId: "sys-1",
  language: "zh-CN",
  translation: "none",
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

const localConfiguration: AsrConfiguration = {
  provider: "local_whisper",
  endpoint: "local://whisper.cpp",
  model: "small",
  language: "auto",
  configured: true,
};

describe("mergeSettings", () => {
  it("falls back to safe defaults for unknown values", () => {
    const merged = mergeSettings(baseSettings, {
      language: "fr-FR",
      translation: "ja-JP",
      asrProvider: "nope" as never,
      autoCleanupDays: 7,
      storageQuotaMb: 12_345,
      microphoneId: "",
      whisperHotwords: `  ${"热".repeat(600)}  `,
    });
    expect(merged.language).toBe("auto");
    expect(merged.translation).toBe("auto");
    expect(merged.asrProvider).toBe("minimax");
    expect(merged.autoCleanupDays).toBe(0);
    expect(merged.storageQuotaMb).toBe(0);
    expect(merged.microphoneId).toBeNull();
    expect(merged.whisperHotwords).toHaveLength(512);
    expect(merged.whisperHotwords.startsWith("热")).toBe(true);
  });

  it("keeps valid values and unrelated fields", () => {
    const merged = mergeSettings(baseSettings, {
      language: "en-US",
      retainAudio: false,
      liveCaptions: true,
      localWhisperModel: "tiny",
      autoCleanupDays: 90,
      storageQuotaMb: 10_240,
    });
    expect(merged.language).toBe("en-US");
    expect(merged.retainAudio).toBe(false);
    expect(merged.liveCaptions).toBe(true);
    expect(merged.localWhisperModel).toBe("tiny");
    expect(merged.autoCleanupDays).toBe(90);
    expect(merged.storageQuotaMb).toBe(10_240);
    expect(merged.asrProfiles).toEqual({});
  });

  it("handles a missing payload", () => {
    expect(mergeSettings(baseSettings, null)).toEqual(baseSettings);
    expect(mergeSettings(baseSettings, undefined)).toEqual(baseSettings);
  });
});

describe("settingsFromUi", () => {
  it("applies UI values without dropping provider configuration", () => {
    const withProfile = withCloudProfile(baseSettings, {
      provider: "silicon_flow",
      endpoint: "https://example.test/asr",
      model: "sensevoice",
      language: "zh-CN",
      configured: true,
    });
    const next = settingsFromUi(withProfile, {
      microphoneId: null,
      systemOutputId: "sys-2",
      language: "auto",
      translation: "en-US",
      retainAudio: false,
      liveCaptions: true,
      liveStreaming: false,
      autoCleanupDays: 30,
      storageQuotaMb: 5_120,
    });
    expect(next.microphoneId).toBeNull();
    expect(next.systemOutputId).toBe("sys-2");
    expect(next.translation).toBe("en-US");
    expect(next.retainAudio).toBe(false);
    expect(next.liveCaptions).toBe(true);
    expect(next.liveStreaming).toBe(false);
    expect(next.autoCleanupDays).toBe(30);
    expect(next.storageQuotaMb).toBe(5_120);
    expect(next.asrProvider).toBe("silicon_flow");
    expect(next.asrProfiles.silicon_flow.model).toBe("sensevoice");
  });
});

describe("provider helpers", () => {
  it("switches to local Whisper and stores model plus hotwords", () => {
    const next = withLocalWhisper(baseSettings, "tiny", "  张三，周会  ");
    expect(next.asrProvider).toBe("local_whisper");
    expect(next.localWhisperModel).toBe("tiny");
    expect(next.whisperHotwords).toBe("张三，周会");
  });

  it("keeps the current model when the selector is empty", () => {
    const next = withLocalWhisper(baseSettings, "", "");
    expect(next.localWhisperModel).toBe("small");
  });

  it("records cloud profiles", () => {
    const next = withCloudProfile(baseSettings, {
      provider: "minimax",
      endpoint: "https://api.minimax.cn/v1/speech_to_text",
      model: "asr-1.0",
      language: "zh-CN",
      configured: true,
    });
    expect(next.asrProvider).toBe("minimax");
    expect(next.asrProfiles.minimax).toEqual({
      endpoint: "https://api.minimax.cn/v1/speech_to_text",
      model: "asr-1.0",
      language: "zh-CN",
    });
  });

  it("resolves the live provider", () => {
    const minimax: AsrConfiguration = {
      provider: "minimax",
      endpoint: "x",
      model: "asr",
      language: "auto",
      configured: true,
    };
    const silicon: AsrConfiguration = { ...minimax, provider: "silicon_flow", configured: false };
    expect(resolveLiveProvider({ ...baseSettings, asrProvider: "local_whisper" }, [minimax])).toBe(
      "local_whisper",
    );
    expect(resolveLiveProvider(baseSettings, [silicon, minimax])).toBe("minimax");
    expect(
      resolveLiveProvider({ ...baseSettings, asrProvider: "silicon_flow" }, [silicon, minimax]),
    ).toBe("minimax");
    expect(resolveLiveProvider(baseSettings, [])).toBe("minimax");
    expect(resolveLiveProvider(baseSettings, [silicon])).toBe("minimax");
  });
});

describe("viewSettings", () => {
  it("mirrors recording options into the view", () => {
    const view = viewSettings({ ...baseSettings, language: "en-US", retainAudio: false });
    expect(view).toEqual({
      language: "en-US",
      translation: "none",
      retainAudio: false,
      liveCaptions: false,
      liveStreaming: true,
    });
  });
});
