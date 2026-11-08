import type { TranscriptSegment } from "./types";

export interface PointItem {
  text: string;
  startMs?: number;
}

export interface TaskItem {
  text: string;
  done: boolean;
}

export interface DerivedInsights {
  points: PointItem[];
  highlights: string[];
  decisions: string[];
  tasks: TaskItem[];
  questions: string[];
  disputes: string[];
}

const DECISION_PATTERN = /(决策|决定|结论)/;
const DISPUTE_PATTERN = /(分歧|争议)/;
const QUESTION_PATTERN = /[?？]\s*$/;

export function noteBullets(notes: string): string[] {
  return notes
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && !/^#{1,6}\s/.test(line))
    .map((line) => line.replace(/^\s*[-*]\s*(\[[ xX]\]\s*)?/, "").trim())
    .filter((line) => line.length > 0);
}

export function parseTasks(notes: string): TaskItem[] {
  return notes
    .split(/\r?\n/)
    .map((line) => /^\s*[-*]\s*\[([ xX])\]\s+(.+)$/.exec(line))
    .filter((match): match is RegExpExecArray => match !== null)
    .map((match) => ({ done: match[1].toLowerCase() === "x", text: match[2].trim() }))
    .filter((task) => task.text.length > 0);
}

function clip(value: string, max: number): string {
  return value.length > max ? `${value.slice(0, max)}…` : value;
}

/**
 * Single source of truth for the lightweight, local insight extraction.
 * AI-generated notes are parsed here; richer analysis can replace this later
 * without changing the review or insight panel contracts.
 */
export function deriveInsights(notes: string, segments: TranscriptSegment[]): DerivedInsights {
  const bullets = noteBullets(notes);
  const tasks = parseTasks(notes);
  const decisions = bullets.filter((line) => DECISION_PATTERN.test(line)).map((line) => clip(line, 120));
  const disputes = bullets.filter((line) => DISPUTE_PATTERN.test(line)).map((line) => clip(line, 120));
  const questions = bullets.filter((line) => QUESTION_PATTERN.test(line)).map((line) => clip(line, 120));
  const special = (line: string): boolean =>
    DECISION_PATTERN.test(line) || DISPUTE_PATTERN.test(line) || QUESTION_PATTERN.test(line);
  const taskTexts = new Set(tasks.map((task) => task.text));
  const highlights = bullets
    .filter((line) => !special(line) && !taskTexts.has(line))
    .map((line) => clip(line, 160));

  const points: PointItem[] = segments.slice(-3).map((segment) => ({
    text: clip(segment.text.replace(/\s+/g, " ").trim(), 52),
    startMs: segment.startMs,
  }));

  return { points, highlights, decisions, tasks, questions, disputes };
}
