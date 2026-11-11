import { checkValue, native } from "./native.js";
import { wholeNumber } from "./options.js";

export interface CompactOptions {
  /** Keep the last N UTC days including today and delete older days. Without it nothing is deleted; 0 deletes every day. */
  keepDays?: number;
  /** A day is merged only once it ended at least this long ago, and a file only once it is this old. Default 3600000. */
  settleTimeMs?: number;
  /** Report the actions without changing anything. */
  dryRun?: boolean;
}

export type CompactAction =
  | { action: "remove_merged_input"; path: string; mergedInto: string }
  | { action: "remove_deleted_day"; path: string }
  | { action: "delete_day"; day: string; files: number; bytes: number }
  | { action: "remove_temp_file"; path: string }
  | { action: "merge_files"; day: string; inputs: number; inputBytes: number; rows: number; output: string }
  | { action: "skip_day"; day: string; reason: string };

export interface CompactReport {
  actions: CompactAction[];
}

/** Merges the small files of past days in `dir` and deletes old days. Runs on the libuv thread pool. */
export async function compact(dir: string, options: CompactOptions = {}): Promise<CompactReport> {
  if (options.dryRun !== undefined && typeof options.dryRun !== "boolean") {
    throw new TypeError("dryRun must be true or false");
  }
  const optionsJSON = JSON.stringify({
    keep_days: wholeNumber(options.keepDays, "keepDays", 0),
    settle_time_ms: wholeNumber(options.settleTimeMs, "settleTimeMs", 0),
    dry_run: options.dryRun,
  });
  const bytes = checkValue(await native.compact(dir, optionsJSON));
  return reportFromJSON(new TextDecoder().decode(bytes));
}

/** Reads the report JSON of the C ABI. Keys such as `merged_into` become `mergedInto`. */
export function reportFromJSON(text: string): CompactReport {
  const report = JSON.parse(text) as { actions: Record<string, unknown>[] };
  const actions = report.actions.map(
    (action) =>
      Object.fromEntries(
        Object.entries(action).map(([key, value]) => [key.replace(/_([a-z])/g, (_, letter: string) => letter.toUpperCase()), value]),
      ) as CompactAction,
  );
  return { actions };
}
