import type { Event } from "../events.js";
import { DuckmonError } from "../native.js";
import type { Store } from "../store.js";

/** `ExportResultCode.SUCCESS` of `@opentelemetry/core`. */
export const SUCCESS = 0;
/** `ExportResultCode.FAILED` of `@opentelemetry/core`. The JavaScript SDK does not retry it. */
export const FAILED = 1;

/** The result an exporter hands to the SDK, like `ExportResult` of `@opentelemetry/core`. */
export interface ExportResult {
  code: number;
  error?: Error;
}

/**
 * What the three exporters share: they write into a store that someone else
 * opened, and they never close it.
 *
 * Exports keep working after `shutdown()` while the store is open. The SDK's
 * simple processors call `shutdown()` without waiting for records that still
 * wait for the resource detectors, and those records arrive afterwards.
 */
export class StoreExporter {
  readonly #store: Store;

  constructor(store: Store) {
    this.#store = store;
  }

  /** Waits until every event written so far is in a Parquet file. */
  forceFlush(): Promise<void> {
    return flushStore(this.#store);
  }

  /** Flushes the store. It never closes the store. */
  shutdown(): Promise<void> {
    return flushStore(this.#store);
  }

  /**
   * Converts the records to events, writes them and calls `resultCallback`
   * once, synchronously. Never throws into the SDK.
   */
  protected exportRecords<T>(
    records: () => readonly T[],
    toEvents: (record: T) => Event[],
    resultCallback: (result: ExportResult) => void,
  ): void {
    let result: ExportResult;
    try {
      result = writeRecords(this.#store, records(), toEvents);
    } catch (err) {
      result = { code: FAILED, error: asError(err) };
    }
    resultCallback(result);
  }
}

/**
 * Writes the events of all records. A record that cannot be converted, or an
 * event the store refuses, is left out alone; the others are written.
 */
export function writeRecords<T>(store: Store, records: readonly T[], toEvents: (record: T) => Event[]): ExportResult {
  const events: Event[] = [];
  let skipped = 0;
  let skipError: unknown = null;
  for (const record of records) {
    try {
      events.push(...toEvents(record));
    } catch (err) {
      skipped++;
      skipError ??= err;
    }
  }

  const droppedBefore = store.droppedEvents;
  let refused = 0;
  let refuseError: unknown = null;
  try {
    store.write(events);
  } catch (err) {
    if (err instanceof DuckmonError) return { code: FAILED, error: err };
    // The store refused the whole batch for one event. Write each event alone.
    for (const event of events) {
      try {
        store.write(event);
      } catch (eventError) {
        if (eventError instanceof DuckmonError) return { code: FAILED, error: eventError };
        refused++;
        refuseError ??= eventError;
      }
    }
  }
  const dropped = store.droppedEvents - droppedBefore;

  const problems: string[] = [];
  if (skipped > 0) {
    problems.push(`duckmon could not convert ${skipped} of ${records.length} records: ${asError(skipError).message}`);
  }
  if (refused > 0) {
    problems.push(`duckmon refused ${refused} of ${events.length} events: ${asError(refuseError).message}`);
  }
  if (dropped > 0) {
    problems.push(`duckmon dropped ${dropped} of ${events.length} events because its queue is full`);
  }
  if (problems.length === 0) return { code: SUCCESS };
  return { code: FAILED, error: new Error(problems.join("; ")) };
}

/** Flushes the store. A store that is closed already wrote everything, so that counts as done. */
async function flushStore(store: Store): Promise<void> {
  try {
    await store.flush();
  } catch (err) {
    if (err instanceof DuckmonError && err.code === "CLOSED") return;
    throw err;
  }
}

function asError(value: unknown): Error {
  if (value instanceof Error) return value;
  try {
    return new Error(String(value));
  } catch {
    return new Error("unreadable error");
  }
}
