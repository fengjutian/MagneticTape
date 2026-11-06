import type { PlaybackChunk } from "./types";

/// Returns the index of the chunk that contains `positionMs`, or -1 when the
/// timeline has a gap there (e.g. a dropped audio block).
export function locateChunk(chunks: PlaybackChunk[], positionMs: number): number {
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

/// Formats milliseconds as `MM:SS`, or `HH:MM:SS` past one hour.
export function formatPlaybackTime(milliseconds: number): string {
  const total = Math.max(0, Math.floor(milliseconds / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  const pad = (value: number) => String(value).padStart(2, "0");
  return hours > 0
    ? `${pad(hours)}:${pad(minutes)}:${pad(seconds)}`
    : `${pad(minutes)}:${pad(seconds)}`;
}
