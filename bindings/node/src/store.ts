import { eventLine, storableAttributes } from "./events.js";
import type { Attributes, Event } from "./events.js";
import { DuckmonError, check, checkValue, errorFromNative, native } from "./native.js";
import type { NativeBatch, NativeError, NativeStore } from "./native.js";
import { wholeNumber } from "./options.js";
import { rowsFromArrowIPC } from "./rows.js";
import type { Row } from "./rows.js";

export interface OpenOptions {
  /** How often buffered events are written to a new Parquet file. Default 5000. */
  flushIntervalMs?: number;
  /** How many native write batches may wait for the writer thread, from 1 to 1000000. Default 1024. */
  channelCapacity?: number;
  /** Stored in the resource column of every event, such as `{ "service.name": "api" }`. */
  resource?: Attributes;
  /** Events kept in JavaScript while the native writer is busy. Later events are dropped. Default 100000. */
  maxPendingEvents?: number;
}

/** Most event lines sent in one native write. */
const MAX_LINES_PER_SEND = 10_000;

/**
 * Most characters sent in one native write. Large events fill a send before
 * the line limit does; without this limit the joined text can pass the
 * longest string JavaScript allows.
 */
const MAX_CHARACTERS_PER_SEND = 8_000_000;

const DEFAULT_MAX_PENDING_EVENTS = 100_000;

/** The largest `channelCapacity` the C ABI accepts. */
const MAX_CHANNEL_CAPACITY = 1_000_000;

/** Stores that are not closed yet, each with the function that closes it while the process exits. */
const openStores = new Map<Store, () => void>();

/** Opens the store in `dir` and creates `dir` when it is missing. */
export async function open(dir: string, options: OpenOptions = {}): Promise<Store> {
  const maxPendingEvents = wholeNumber(options.maxPendingEvents, "maxPendingEvents", 1) ?? DEFAULT_MAX_PENDING_EVENTS;
  const optionsJSON = JSON.stringify({
    flush_interval_ms: wholeNumber(options.flushIntervalMs, "flushIntervalMs", 1),
    channel_capacity: wholeNumber(options.channelCapacity, "channelCapacity", 1, MAX_CHANNEL_CAPACITY),
    resource: options.resource === undefined ? undefined : storableAttributes(options.resource, "resource"),
  });
  const nativeStore = checkValue(await native.open(dir, optionsJSON));
  return new Store(nativeStore, maxPendingEvents);
}

/**
 * A telemetry store bound to one directory. Create it with `open()`.
 *
 * `write()` only queues events. The queue is sent to the native writer on the
 * libuv thread pool, one batch at a time. `flush()` and `query()` first wait
 * until every event queued before them is sent; `close()` waits for all.
 */
export class Store {
  #native: NativeStore;
  #maxPendingEvents: number;
  #pending: string[] = [];
  /** The batch whose native write has not finished yet. The exit handler writes it if the pool task has not. */
  #batch: NativeBatch | null = null;
  /** True while a send is scheduled or running. */
  #sending = false;
  /** Events ever put in the queue, and events whose native write has finished. */
  #queuedEvents = 0;
  #finishedEvents = 0;
  #waiters: { events: number; resolve: () => void }[] = [];
  #writeError: DuckmonError | null = null;
  #closing: Promise<void> | null = null;
  #droppedEvents = 0;
  #warnedAboutDrops = false;

  /** Use `open()`. */
  constructor(nativeStore: NativeStore, maxPendingEvents: number) {
    this.#native = nativeStore;
    this.#maxPendingEvents = maxPendingEvents;
    openStores.set(this, () => this.#closeWhileExiting());
    installExitHandlers();
  }

  /** Events dropped because the queue was full or because the store refused their batch. */
  get droppedEvents(): number {
    return this.#droppedEvents;
  }

  /**
   * Queues events and returns at once. Throws `TypeError` for a field or
   * attribute that cannot be stored, in which case none of the given events is
   * queued, and `DuckmonError` with code `CLOSED` once `close()` was called.
   */
  write(events: Event | readonly Event[]): void {
    if (this.#closing !== null) {
      throw new DuckmonError("CLOSED", "store is closed");
    }
    const list = (Array.isArray(events) ? events : [events]) as readonly Event[];
    const lines = list.map(eventLine);
    for (const line of lines) {
      if (this.#pending.length >= this.#maxPendingEvents) {
        this.#drop(1, `duckmon dropped events because ${this.#maxPendingEvents} events wait to be written`);
        continue;
      }
      this.#pending.push(line);
      this.#queuedEvents++;
    }
    if (!this.#sending && this.#pending.length > 0) {
      this.#sending = true;
      setImmediate(() => void this.#sendPending());
    }
  }

  /** Waits until every event written before this call is in a finished Parquet file. */
  async flush(): Promise<void> {
    await this.#sentUpTo(this.#queuedEvents);
    const result = await this.#native.flush();
    this.#throwWriteError();
    check(result);
  }

  /**
   * Runs SQL against the view `events`. Sees every event written before this call.
   * Results are decoded on the main thread, so use `LIMIT` for large results.
   */
  async query<T = Row>(sql: string): Promise<T[]> {
    await this.#sentUpTo(this.#queuedEvents);
    const bytes = checkValue(await this.#native.query(sql));
    return rowsFromArrowIPC(bytes) as T[];
  }

  /** Writes every queued event, flushes and stops the store. Safe to call again. */
  close(): Promise<void> {
    this.#closing ??= this.#close();
    return this.#closing;
  }

  async #close(): Promise<void> {
    await this.#sentUpTo(this.#queuedEvents);
    const result = await this.#native.close();
    openStores.delete(this);
    this.#throwWriteError();
    check(result);
  }

  async #sendPending(): Promise<void> {
    while (this.#pending.length > 0) {
      const lines = this.#takeLinesForOneSend();
      const batch = new native.NativeBatch(lines.join("\n"));
      this.#batch = batch;
      let err: DuckmonError | null;
      try {
        const result = await this.#native.writeEvents(batch);
        err = result === null ? null : errorFromNative(result);
      } catch (thrown) {
        err = new DuckmonError("INTERNAL", String(thrown));
      }
      this.#batch = null;
      if (err !== null) {
        this.#writeError ??= err;
        this.#drop(lines.length, `duckmon dropped events because the store refused them: ${err.message}`);
      }
      this.#finishedEvents += lines.length;
      this.#waiters = this.#waiters.filter((waiter) => {
        if (waiter.events > this.#finishedEvents) return true;
        waiter.resolve();
        return false;
      });
    }
    this.#sending = false;
  }

  /** Resolves once the native write of the first `events` queued events has finished. */
  #sentUpTo(events: number): Promise<void> {
    if (this.#finishedEvents >= events) return Promise.resolve();
    return new Promise((resolve) => this.#waiters.push({ events, resolve }));
  }

  /** Counts dropped events and warns once per store. */
  #drop(events: number, warning: string): void {
    this.#droppedEvents += events;
    if (this.#warnedAboutDrops) return;
    this.#warnedAboutDrops = true;
    process.emitWarning(warning, "DuckmonWarning");
  }

  #throwWriteError(): void {
    const err = this.#writeError;
    this.#writeError = null;
    if (err !== null) throw err;
  }

  /**
   * Writes what is not written yet and closes, on the main thread. Only the
   * `exit` handler calls this. The batch of a running send is written here
   * when its pool task has not started; when it is running, this waits for it.
   */
  #closeWhileExiting(): void {
    if (this.#batch !== null) {
      printExitError(this.#native.writeEventsSync(this.#batch));
    }
    while (this.#pending.length > 0) {
      const lines = this.#takeLinesForOneSend();
      printExitError(this.#native.writeEventsSync(new native.NativeBatch(lines.join("\n"))));
    }
    printExitError(this.#native.closeSync());
  }

  /** Takes the next lines from the queue, at least one, within the line and character limits of one send. */
  #takeLinesForOneSend(): string[] {
    let count = 0;
    let characters = 0;
    while (count < this.#pending.length && count < MAX_LINES_PER_SEND) {
      characters += this.#pending[count]!.length;
      count++;
      if (characters >= MAX_CHARACTERS_PER_SEND) break;
    }
    return this.#pending.splice(0, count);
  }
}

let exitHandlersInstalled = false;

/**
 * At a normal end of the event loop, `beforeExit` closes every open store, and
 * the pending close keeps the process alive until it is done. On
 * `process.exit()`, an uncaught exception or an unhandled rejection only
 * `exit` runs, so it writes and closes synchronously. Signals are not caught.
 */
function installExitHandlers(): void {
  if (exitHandlersInstalled) return;
  exitHandlersInstalled = true;
  process.on("beforeExit", () => {
    for (const store of openStores.keys()) {
      store.close().catch((err: unknown) => process.stderr.write(`duckmon: ${String(err)}\n`));
    }
  });
  process.on("exit", () => {
    for (const closeWhileExiting of openStores.values()) closeWhileExiting();
    openStores.clear();
  });
}

function printExitError(result: NativeError | null): void {
  if (result !== null && errorFromNative(result).code !== "CLOSED") {
    process.stderr.write(`duckmon: ${result.message}\n`);
  }
}
