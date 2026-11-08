import { beforeEach, describe, expect, it } from "vitest";
import {
  filterDeletedSegments,
  isSegmentDeleted,
  loadDeletedSegments,
  loadSpeakerNames,
  saveDeletedSegments,
  saveSpeakerNames,
  speakerKey,
  speakerLabel,
} from "./session-prefs";

const segment = { startMs: 1_000, endMs: 2_000, speaker: "S1", text: "你好" };

beforeEach(() => localStorage.clear());

describe("speaker names", () => {
  it("maps a speaker key to a custom label and falls back safely", () => {
    expect(speakerKey(segment, 0)).toBe("S1");
    expect(speakerKey({ startMs: 0, endMs: 1, text: "x" }, 2)).toBe("index:2");
    expect(speakerLabel(segment, 0, { S1: "张三" })).toBe("张三");
    expect(speakerLabel(segment, 0, {})).toBe("发言人 A");
  });

  it("round-trips per session and ignores invalid values", () => {
    saveSpeakerNames("s1", { S1: "张三" });
    expect(loadSpeakerNames("s1")).toEqual({ S1: "张三" });
    expect(loadSpeakerNames("s2")).toEqual({});
    localStorage.setItem("magnetic-tape.speakers.s1", JSON.stringify({ S1: 42, S2: " 李四 " }));
    expect(loadSpeakerNames("s1")).toEqual({ S2: "李四" });
  });
});

describe("deleted segments", () => {
  it("filters only the deleted text at the same timestamp", () => {
    const deleted = [{ startMs: 1_000, endMs: 2_000, text: "你好" }];
    expect(isSegmentDeleted(segment, deleted)).toBe(true);
    expect(filterDeletedSegments([segment], deleted)).toEqual([]);
    expect(isSegmentDeleted({ ...segment, text: "再见" }, deleted)).toBe(false);
  });

  it("round-trips and falls back when corrupt", () => {
    const deleted = [{ startMs: 1_000, endMs: 2_000, text: "你好" }];
    saveDeletedSegments("s1", deleted);
    expect(loadDeletedSegments("s1")).toEqual(deleted);
    localStorage.setItem("magnetic-tape.deleted.s1", "{bad");
    expect(loadDeletedSegments("s1")).toEqual([]);
  });
});
