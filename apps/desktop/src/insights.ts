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
// Keep this deliberately stricter than a generic future-tense matcher. These
// phrases express an agreement, choice, approval, or concrete commitment and
// are useful before the post-recording AI notes have been generated.
const LIVE_DECISION_PATTERN = /(?:达成一致|一致(?:同意|通过)|(?:已经|现已|最终)?决定|(?:已经|现已|最终)?确定|(?:予以|一致)?通过|(?:已经|表示)?同意|批准|就按|按.+(?:执行|推进|处理)|这就安排|(?:我|我们|公司|团队)?(?:会|将|要)安排|保证(?:以后|后续|下次)|(?:以后|后续|下次)(?:不再|改为|由|安排))/;
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

function transcriptSentences(segments: TranscriptSegment[]): string[] {
  return segments.flatMap((segment) =>
    segment.text
      .replace(/\s+/g, " ")
      .match(/[^。！？!?；;]+[。！？!?；;]?/g)
      ?.map((line) => line.trim())
      .filter((line) => line.length > 0) ?? [],
  );
}

function uniqueLines(lines: string[]): string[] {
  const seen = new Set<string>();
  return lines.filter((line) => {
    const key = line.replace(/[\s。！？!?；;：:，,]/g, "");
    if (!key || seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/**
 * Single source of truth for the lightweight, local insight extraction.
 * AI-generated notes are parsed here; richer analysis can replace this later
 * without changing the review or insight panel contracts.
 */
export function deriveInsights(notes: string, segments: TranscriptSegment[]): DerivedInsights {
  const bullets = noteBullets(notes);
  const tasks = parseTasks(notes);
  const noteDecisions = bullets.filter((line) => DECISION_PATTERN.test(line));
  const liveDecisions = transcriptSentences(segments).filter((line) => LIVE_DECISION_PATTERN.test(line));
  const decisions = uniqueLines([...noteDecisions, ...liveDecisions]).map((line) => clip(line, 120));
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
