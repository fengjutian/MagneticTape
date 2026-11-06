import { convertFileSrc } from "@tauri-apps/api/core";
import type { PlaybackChunk, PlaybackTracks } from "./types";

/// One recording track: its chunk timeline plus the `<audio>` element that
/// currently holds the chunk under the playhead.
interface Track {
  chunks: PlaybackChunk[];
  audio: HTMLAudioElement;
  index: number;
  /// Bumped on every chunk load so late `loadedmetadata` handlers are ignored.
  generation: number;
  pending: boolean;
}

export interface TimelinePlayerOptions {
  /// Called on every playhead update with the position and playing state.
  onPosition?: (positionMs: number, playing: boolean) => void;
}

function locate(chunks: PlaybackChunk[], positionMs: number): number {
  let low = 0;
  let high = chunks.length - 1;
  while (low <= high) {
    const middle = (low + high) >> 1;
    const chunk = chunks[middle];
    if (positionMs < chunk.startMs) {
      high = middle - 1;
    } else if (positionMs >= chunk.startMs + chunk.durationMs) {
      low = middle + 1;
    } else {
      return middle;
    }
  }
  return -1;
}

function formatTime(milliseconds: number): string {
  const total = Math.max(0, Math.floor(milliseconds / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  const pad = (value: number) => String(value).padStart(2, "0");
  return hours > 0
    ? `${pad(hours)}:${pad(minutes)}:${pad(seconds)}`
    : `${pad(minutes)}:${pad(seconds)}`;
}

/// Plays the microphone and system tracks as one continuous timeline built
/// from 15 second Opus chunks. Seeking maps a timeline position to the chunk
/// that contains it, so the user can scrub the whole session instead of
/// stepping through individual files.
export class TimelinePlayer {
  readonly root: HTMLElement;
  readonly durationMs: number;
  private readonly microphone: Track;
  private readonly system: Track;
  private readonly options: TimelinePlayerOptions;
  private readonly toggle: HTMLButtonElement | null;
  private readonly seekInput: HTMLInputElement | null;
  private readonly rateSelect: HTMLSelectElement | null;
  private readonly currentLabel: HTMLElement | null;
  private readonly totalLabel: HTMLElement | null;
  private readonly hint: HTMLElement | null;
  private positionMs = 0;
  private playing = false;
  private rate = 1;
  private frame: number | undefined;
  private anchorTime = 0;
  private anchorPosition = 0;
  private disposed = false;
  private resumeAfterScrub = false;

  constructor(root: HTMLElement, tracks: PlaybackTracks, options: TimelinePlayerOptions = {}) {
    this.root = root;
    this.options = options;
    this.durationMs = Math.max(0, tracks.durationMs);
    this.microphone = this.createTrack(tracks.microphone);
    this.system = this.createTrack(tracks.system);
    this.toggle = root.querySelector<HTMLButtonElement>(".playback-toggle");
    this.seekInput = root.querySelector<HTMLInputElement>(".playback-seek");
    this.rateSelect = root.querySelector<HTMLSelectElement>(".playback-rate");
    this.currentLabel = root.querySelector<HTMLElement>(".playback-current");
    this.totalLabel = root.querySelector<HTMLElement>(".playback-total");
    this.hint = root.querySelector<HTMLElement>(".playback-hint");
    this.bind();
    this.applyEmptyState();
    this.updateUi();
  }

  play(): void {
    if (this.playing || this.durationMs <= 0 || this.disposed) return;
    if (this.positionMs >= this.durationMs - 20) this.positionMs = 0;
    this.playing = true;
    this.anchorTime = performance.now();
    this.anchorPosition = this.positionMs;
    this.syncTrack(this.microphone, true, true);
    this.syncTrack(this.system, true, true);
    this.updateUi();
    this.emit();
    this.frame = requestAnimationFrame(this.tick);
  }

  pause(): void {
    if (!this.playing) return;
    this.positionMs = Math.min(
      this.durationMs,
      this.anchorPosition + (performance.now() - this.anchorTime) * this.rate,
    );
    this.playing = false;
    this.stopClock();
    this.microphone.audio.pause();
    this.system.audio.pause();
    this.updateUi();
    this.emit();
  }

  seek(positionMs: number): void {
    if (this.durationMs <= 0 || this.disposed) return;
    this.positionMs = Math.min(Math.max(positionMs, 0), this.durationMs);
    if (this.playing) {
      this.anchorTime = performance.now();
      this.anchorPosition = this.positionMs;
    }
    this.syncTrack(this.microphone, this.playing, true);
    this.syncTrack(this.system, this.playing, true);
    this.updateUi();
    this.emit();
  }

  setRate(rate: number): void {
    if (!Number.isFinite(rate) || rate <= 0) return;
    this.rate = rate;
    this.microphone.audio.playbackRate = rate;
    this.system.audio.playbackRate = rate;
    if (this.playing) {
      this.anchorTime = performance.now();
      this.anchorPosition = this.positionMs;
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.stopClock();
    this.playing = false;
    for (const track of [this.microphone, this.system]) {
      track.audio.pause();
      track.audio.removeAttribute("src");
      track.audio.load();
    }
  }

  private createTrack(chunks: PlaybackChunk[]): Track {
    const audio = new Audio();
    audio.preload = "auto";
    return {
      chunks: [...chunks].sort((left, right) => left.startMs - right.startMs),
      audio,
      index: -1,
      generation: 0,
      pending: false,
    };
  }

  private bind(): void {
    this.toggle?.addEventListener("click", () => {
      if (this.playing) {
        this.pause();
      } else {
        this.play();
      }
    });
    this.seekInput?.addEventListener("pointerdown", () => {
      this.resumeAfterScrub = this.playing;
      if (this.playing) this.pause();
    });
    this.seekInput?.addEventListener("input", () => {
      if (!this.seekInput) return;
      this.seek((Number(this.seekInput.value) / 1000) * this.durationMs);
    });
    this.seekInput?.addEventListener("change", () => {
      if (this.resumeAfterScrub) {
        this.resumeAfterScrub = false;
        this.play();
      }
    });
    this.rateSelect?.addEventListener("change", () => {
      this.setRate(Number(this.rateSelect?.value ?? 1));
      this.updateUi();
    });
  }

  private readonly tick = (): void => {
    if (!this.playing || this.disposed) return;
    const position = this.anchorPosition + (performance.now() - this.anchorTime) * this.rate;
    if (position >= this.durationMs) {
      this.positionMs = this.durationMs;
      this.playing = false;
      this.stopClock();
      this.microphone.audio.pause();
      this.system.audio.pause();
      this.updateUi();
      this.emit();
      return;
    }
    this.positionMs = position;
    this.syncTrack(this.microphone, true);
    this.syncTrack(this.system, true);
    this.updateUi();
    this.emit();
    this.frame = requestAnimationFrame(this.tick);
  };

  private stopClock(): void {
    if (this.frame !== undefined) {
      cancelAnimationFrame(this.frame);
      this.frame = undefined;
    }
  }

  /// Makes one track follow the playhead: loads the chunk under it, corrects
  /// drift, and pauses when the timeline has a gap on this track.
  private syncTrack(track: Track, play: boolean, force = false): void {
    if (track.chunks.length === 0) {
      track.pending = false;
      track.audio.pause();
      return;
    }
    if (track.pending && !force) return;
    const index = locate(track.chunks, this.positionMs);
    if (index < 0) {
      track.index = -1;
      track.pending = false;
      track.audio.pause();
      return;
    }
    const chunk = track.chunks[index];
    const offsetSeconds = (this.positionMs - chunk.startMs) / 1000;
    if (force || track.index !== index || track.audio.readyState === 0) {
      this.load(track, index, offsetSeconds, play);
      return;
    }
    const offset = Math.min(
      Math.max(offsetSeconds, 0),
      Math.max(0, chunk.durationMs / 1000 - 0.05),
    );
    if (play) {
      if (Math.abs(track.audio.currentTime - offset) > 0.3) {
        try {
          track.audio.currentTime = offset;
        } catch {
          // The element is still loading; the next frame retries.
        }
      }
      if (track.audio.paused) void track.audio.play().catch(() => {});
    } else if (!track.audio.paused) {
      track.audio.pause();
    }
  }

  private load(track: Track, index: number, offsetSeconds: number, play: boolean): void {
    const chunk = track.chunks[index];
    const url = convertFileSrc(chunk.path);
    const generation = ++track.generation;
    track.pending = true;
    track.index = index;
    if (track.audio.src !== url) track.audio.src = url;

    const apply = () => {
      if (this.disposed || generation !== track.generation) return;
      track.pending = false;
      const offset = Math.min(
        Math.max(offsetSeconds, 0),
        Math.max(0, chunk.durationMs / 1000 - 0.05),
      );
      try {
        track.audio.currentTime = offset;
      } catch {
        // Seeking before metadata is ready is retried on the next frame.
      }
      track.audio.playbackRate = this.rate;
      if (play && this.playing) {
        void track.audio.play().catch(() => {});
      } else {
        track.audio.pause();
      }
    };

    if (track.audio.readyState >= 1) {
      apply();
    } else {
      track.audio.addEventListener("loadedmetadata", apply, { once: true });
    }
  }

  private applyEmptyState(): void {
    const empty = this.durationMs <= 0;
    this.root.classList.toggle("empty", empty);
    if (this.toggle) this.toggle.disabled = empty;
    if (this.seekInput) this.seekInput.disabled = empty;
    if (this.rateSelect) this.rateSelect.disabled = empty;
    if (this.hint) {
      this.hint.textContent = empty
        ? "音频已删除或尚未生成，仅保留转写与笔记。"
        : "麦克风与系统声音已对齐到同一时间轴；点击下方字幕时间戳可跳转。";
    }
  }

  private updateUi(): void {
    if (this.disposed) return;
    if (this.toggle) {
      this.toggle.classList.toggle("playing", this.playing);
      this.toggle.setAttribute("aria-label", this.playing ? "暂停录音" : "播放录音");
      const label = this.toggle.querySelector("span");
      if (label) label.textContent = this.playing ? "暂停" : "播放";
    }
    if (this.currentLabel) this.currentLabel.textContent = formatTime(this.positionMs);
    if (this.totalLabel) this.totalLabel.textContent = formatTime(this.durationMs);
    if (this.seekInput && document.activeElement !== this.seekInput) {
      this.seekInput.value =
        this.durationMs > 0
          ? String(Math.round((this.positionMs / this.durationMs) * 1000))
          : "0";
    }
  }

  private emit(): void {
    this.options.onPosition?.(this.positionMs, this.playing);
  }
}
