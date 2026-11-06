import { describe, expect, it } from "vitest";
import { formatPlaybackTime, locateChunk } from "./timeline";
import type { PlaybackChunk } from "./types";

const chunks: PlaybackChunk[] = [
  { path: "mic-0.ogg", startMs: 0, durationMs: 5_000 },
  { path: "mic-1.ogg", startMs: 5_000, durationMs: 5_000 },
  { path: "mic-2.ogg", startMs: 12_000, durationMs: 3_000 },
];

describe("locateChunk", () => {
  it("finds the chunk containing the playhead", () => {
    expect(locateChunk(chunks, 0)).toBe(0);
    expect(locateChunk(chunks, 4_999)).toBe(0);
    expect(locateChunk(chunks, 5_000)).toBe(1);
    expect(locateChunk(chunks, 9_999)).toBe(1);
    expect(locateChunk(chunks, 12_000)).toBe(2);
    expect(locateChunk(chunks, 14_999)).toBe(2);
  });

  it("returns -1 in gaps and outside the timeline", () => {
    expect(locateChunk(chunks, -1)).toBe(-1);
    expect(locateChunk(chunks, 10_500)).toBe(-1);
    expect(locateChunk(chunks, 15_000)).toBe(-1);
    expect(locateChunk([], 0)).toBe(-1);
  });
});

describe("formatPlaybackTime", () => {
  it("formats MM:SS and HH:MM:SS", () => {
    expect(formatPlaybackTime(0)).toBe("00:00");
    expect(formatPlaybackTime(61_000)).toBe("01:01");
    expect(formatPlaybackTime(3_600_000)).toBe("01:00:00");
    expect(formatPlaybackTime(3_661_000)).toBe("01:01:01");
    expect(formatPlaybackTime(-5_000)).toBe("00:00");
  });
});
