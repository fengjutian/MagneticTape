export type ConfirmationKind = "decision" | "task" | "question";
export type ConfirmationState = "confirmed" | "ignored";

export interface ConfirmationEntry {
  state: ConfirmationState;
  /** User-edited text. The original AI text remains the key source. */
  text?: string;
  at: number;
}

export type ConfirmationMap = Record<string, ConfirmationEntry>;

const STORAGE_PREFIX = "magnetic-tape.confirmations.";

function normalize(value: string): string {
  return value.replace(/\s+/g, " ").trim().toLocaleLowerCase("zh-CN");
}

/**
 * Stable identity for an AI-derived item. Keys stay stable when the user edits
 * the text, so the edit can be stored as an override instead of a new item.
 */
export function confirmationKey(kind: ConfirmationKind, text: string): string {
  const normalized = normalize(text);
  let hash = 0x811c9dc5;
  for (let index = 0; index < normalized.length; index += 1) {
    hash ^= normalized.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return `${kind}:${(hash >>> 0).toString(36)}:${normalized.length}`;
}

function storageKey(sessionId: string | null): string {
  return `${STORAGE_PREFIX}${sessionId ?? "draft"}`;
}

function readStorage(): Storage | null {
  try {
    return globalThis.localStorage;
  } catch {
    return null;
  }
}

export function loadConfirmations(sessionId: string | null): ConfirmationMap {
  const storage = readStorage();
  if (!storage) return {};
  try {
    const raw = storage.getItem(storageKey(sessionId));
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const result: ConfirmationMap = {};
    for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
      if (!value || typeof value !== "object") continue;
      const entry = value as Partial<ConfirmationEntry>;
      if (entry.state !== "confirmed" && entry.state !== "ignored") continue;
      result[key] = {
        state: entry.state,
        text: typeof entry.text === "string" ? entry.text : undefined,
        at: typeof entry.at === "number" && Number.isFinite(entry.at) ? entry.at : Date.now(),
      };
    }
    return result;
  } catch {
    return {};
  }
}

export function saveConfirmations(sessionId: string | null, map: ConfirmationMap): void {
  const storage = readStorage();
  if (!storage) return;
  storage.setItem(storageKey(sessionId), JSON.stringify(map));
}

export function setConfirmation(
  map: ConfirmationMap,
  kind: ConfirmationKind,
  text: string,
  state: ConfirmationState,
  editedText?: string,
): ConfirmationMap {
  const key = confirmationKey(kind, text);
  return {
    ...map,
    [key]: {
      state,
      text: editedText?.trim() || undefined,
      at: Date.now(),
    },
  };
}

export function clearConfirmation(map: ConfirmationMap, kind: ConfirmationKind, text: string): ConfirmationMap {
  const key = confirmationKey(kind, text);
  if (!(key in map)) return map;
  const next = { ...map };
  delete next[key];
  return next;
}

export function confirmationEntry(
  map: ConfirmationMap,
  kind: ConfirmationKind,
  text: string,
): ConfirmationEntry | undefined {
  return map[confirmationKey(kind, text)];
}

export function effectiveText(entry: ConfirmationEntry | undefined, fallback: string): string {
  return entry?.text?.trim() || fallback;
}

export function isPending(entry: ConfirmationEntry | undefined): boolean {
  return entry === undefined;
}
