import { createRequire } from "node:module";

/** A failed C ABI call: the `DUCKMON_ERROR_*` code and its message. */
export interface NativeError {
  code: number;
  message: string;
}

/**
 * JSON Lines text that the addon writes exactly once: by the thread pool task
 * or by the exit handler, whichever takes it first.
 */
export type NativeBatch = object;

/** The store object of the addon. Every promise resolves; a failure is a `NativeError` value. */
export interface NativeStore {
  /** Writes the batch on the thread pool, unless it was written already. */
  writeEvents(batch: NativeBatch): Promise<NativeError | null>;
  /** Writes the batch on the calling thread unless it was written already, and waits while a pool task writes it. Only for the exit handler. */
  writeEventsSync(batch: NativeBatch): NativeError | null;
  flush(): Promise<NativeError | null>;
  /** Resolves with an Arrow IPC stream. */
  query(sql: string): Promise<Buffer | NativeError>;
  close(): Promise<NativeError | null>;
  /** Blocks the calling thread. Only for the exit handler. */
  closeSync(): NativeError | null;
}

interface NativeModule {
  NativeBatch: new (jsonLines: string) => NativeBatch;
  abiVersion(): number;
  open(dir: string, optionsJSON: string): Promise<NativeStore | NativeError>;
  /** Resolves with the report as UTF 8 JSON. */
  compact(dir: string, optionsJSON: string): Promise<Buffer | NativeError>;
}

/** `DUCKMON_ABI_VERSION` of `duckmon.h` that this code was written for. */
const ABI_VERSION = 1;

export const native = createRequire(import.meta.url)("../duckmon.node") as NativeModule;

if (native.abiVersion() !== ABI_VERSION) {
  throw new Error(`duckmon.node has ABI version ${native.abiVersion()}, but this package needs ${ABI_VERSION}`);
}

export type ErrorCode =
  | "INVALID_ARGUMENT"
  | "INVALID_EVENT"
  | "INVALID_DIRECTORY"
  | "DATABASE"
  | "FILE"
  | "CLOSED"
  | "COMPACT"
  | "INTERNAL";

/** The names of the `DUCKMON_ERROR_*` codes in `duckmon.h`. */
const ERROR_CODES: Record<number, ErrorCode> = {
  1: "INVALID_ARGUMENT",
  2: "INVALID_EVENT",
  3: "INVALID_DIRECTORY",
  4: "DATABASE",
  5: "FILE",
  6: "CLOSED",
  7: "COMPACT",
  8: "INTERNAL",
};

/** An error reported by the duckmon store. */
export class DuckmonError extends Error {
  readonly code: ErrorCode;

  constructor(code: ErrorCode, message: string) {
    super(message);
    this.name = "DuckmonError";
    this.code = code;
  }
}

export function errorFromNative(err: NativeError): DuckmonError {
  return new DuckmonError(ERROR_CODES[err.code] ?? "INTERNAL", err.message);
}

/** Throws when a native call that returns nothing failed. */
export function check(result: NativeError | null): void {
  if (result !== null) throw errorFromNative(result);
}

/** Returns the value of a native call, or throws when it failed. */
export function checkValue<T extends object>(result: T | NativeError): T {
  if (isNativeError(result)) throw errorFromNative(result);
  return result;
}

function isNativeError(value: object): value is NativeError {
  return typeof (value as { code?: unknown }).code === "number" && !(value instanceof Uint8Array);
}
