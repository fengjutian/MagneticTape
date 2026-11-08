import { beforeEach, describe, expect, it } from "vitest";
import {
  clearConfirmation,
  confirmationEntry,
  confirmationKey,
  effectiveText,
  loadConfirmations,
  saveConfirmations,
  setConfirmation,
} from "./confirmations";

beforeEach(() => {
  localStorage.clear();
});

describe("confirmationKey", () => {
  it("is stable across whitespace and case changes", () => {
    const first = confirmationKey("task", "完成  系统稳定性优化");
    const second = confirmationKey("task", "完成 系统稳定性优化 ");
    expect(first).toBe(second);
  });

  it("separates kinds and different texts", () => {
    expect(confirmationKey("task", "同一段文字")).not.toBe(confirmationKey("decision", "同一段文字"));
    expect(confirmationKey("task", "甲")).not.toBe(confirmationKey("task", "乙"));
  });
});

describe("confirmation persistence", () => {
  it("round-trips confirmations per session", () => {
    let map = setConfirmation({}, "task", "完成稳定性方案", "confirmed");
    map = setConfirmation(map, "decision", "先修复稳定性", "ignored");
    saveConfirmations("session-a", map);

    const loaded = loadConfirmations("session-a");
    expect(confirmationEntry(loaded, "task", "完成稳定性方案")?.state).toBe("confirmed");
    expect(confirmationEntry(loaded, "decision", "先修复稳定性")?.state).toBe("ignored");
    expect(confirmationEntry(loadConfirmations("session-b"), "task", "完成稳定性方案")).toBeUndefined();
  });

  it("keeps the original key when the user edits the text", () => {
    const original = "制定稳定性方案";
    let map = setConfirmation({}, "task", original, "confirmed", "制定并评审稳定性方案");
    const entry = confirmationEntry(map, "task", original);
    expect(entry?.state).toBe("confirmed");
    expect(effectiveText(entry, original)).toBe("制定并评审稳定性方案");
    expect(confirmationEntry(map, "task", "制定并评审稳定性方案")).toBeUndefined();
  });

  it("clears an entry without touching the others", () => {
    let map = setConfirmation({}, "task", "任务 A", "confirmed");
    map = setConfirmation(map, "task", "任务 B", "confirmed");
    map = clearConfirmation(map, "task", "任务 A");
    expect(confirmationEntry(map, "task", "任务 A")).toBeUndefined();
    expect(confirmationEntry(map, "task", "任务 B")?.state).toBe("confirmed");
  });

  it("falls back safely when stored data is corrupt", () => {
    localStorage.setItem("magnetic-tape.confirmations.session-x", "{not json");
    expect(loadConfirmations("session-x")).toEqual({});
  });
});
