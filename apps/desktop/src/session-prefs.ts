import type { TranscriptSegment } from "./types";

export type SpeakerNames = Record<string, string>;

export interface DeletedSegment {
  startMs: number;
  endMs: number;
  text: string;
}

function key(prefix: string, sessionId: string | null): string {
  return `magnetic-tape.${prefix}.${sessionId ?? "draft"}`;
}

function read<T>(storageKey: string, fallback: T): T {
  try {
    const raw = globalThis.localStorage?.getItem(storageKey);
    if (!raw) return fallback;
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

function write(storageKey: string, value: unknown): void {
  try {
    globalThis.localStorage?.setItem(storageKey, JSON.stringify(value));
  } catch {
    // Persistence is best effort; the in-memory state still works.
  }
}

export function speakerKey(segment: TranscriptSegment, index: number): string {
  return segment.speaker ?? `index:${index}`;
}

/// Without diarization there is no speaker at all. Cycling A/B/C/D by row
/// index made consecutive sentences by the same person look like different
/// people, so unnamed rows stay neutral until the backend labels them.
export function defaultSpeakerLabel(speaker: string | undefined): string {
  if (!speaker) return "发言人";
  const match = /^S(\d+)$/i.exec(speaker);
  if (match) return `发言人 ${String.fromCharCode(64 + Number(match[1]))}`;
  return speaker;
}

export function speakerLabel(
  segment: TranscriptSegment,
  index: number,
  names: SpeakerNames | undefined,
): string {
  const mapped = names?.[speakerKey(segment, index)];
  return mapped && mapped.trim() ? mapped : defaultSpeakerLabel(segment.speaker);
}

export function loadSpeakerNames(sessionId: string | null): SpeakerNames {
  const parsed = read<unknown>(key("speakers", sessionId), {});
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
  const result: SpeakerNames = {};
  for (const [speaker, value] of Object.entries(parsed as Record<string, unknown>)) {
    if (typeof value === "string" && value.trim()) result[speaker] = value.trim();
  }
  return result;
}

export function saveSpeakerNames(sessionId: string | null, names: SpeakerNames): void {
  write(key("speakers", sessionId), names);
}

export function loadDeletedSegments(sessionId: string | null): DeletedSegment[] {
  const parsed = read<unknown>(key("deleted", sessionId), []);
  if (!Array.isArray(parsed)) return [];
  return parsed.filter((value): value is DeletedSegment => {
    if (!value || typeof value !== "object") return false;
    const item = value as Partial<DeletedSegment>;
    return typeof item.startMs === "number" && typeof item.endMs === "number" && typeof item.text === "string";
  });
}

export function saveDeletedSegments(sessionId: string | null, deleted: DeletedSegment[]): void {
  write(key("deleted", sessionId), deleted);
}

export function isSegmentDeleted(segment: TranscriptSegment, deleted: DeletedSegment[]): boolean {
  return deleted.some((item) => item.startMs === segment.startMs && item.text === segment.text);
}

export function filterDeletedSegments(segments: TranscriptSegment[], deleted: DeletedSegment[]): TranscriptSegment[] {
  if (deleted.length === 0) return segments;
  return segments.filter((segment) => !isSegmentDeleted(segment, deleted));
}
