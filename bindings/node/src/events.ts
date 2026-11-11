/**
 * An attribute value that the store keeps exactly: a string, a finite number,
 * a boolean, null, a `bigint` from -(2^63) to 2^64 - 1, or an array or plain
 * object of these, nested at most 64 levels.
 */
export type AttributeValue =
  | string
  | number
  | boolean
  | bigint
  | null
  | AttributeValue[]
  | { [key: string]: AttributeValue };

export type Attributes = { [key: string]: AttributeValue };

/** One telemetry event: a log, a span, a metric snapshot or a plain event. */
export interface Event {
  /** Stable, low cardinality name in lowercase dotted words, such as "http.request". */
  name: string;
  /** Nanoseconds since 1970 UTC. For a span, the start time. Default: `nowNanos()` at write time. */
  ts?: bigint;
  /** Human readable log text. Set only for logs. */
  message?: string;
  /** Span length in nanoseconds. Set only for spans. */
  durationNs?: bigint;
  /** 32 lowercase hex characters. */
  traceId?: string;
  /** 16 lowercase hex characters. */
  spanId?: string;
  /** Parent span ID of a span. Leave it out for a root span. */
  parentId?: string;
  attrs?: Attributes;
}

/** The largest time or length the store can keep: 2^63 - 1 nanoseconds. */
const LARGEST_NANOS = 2n ** 63n - 1n;

/** The native JSON reader keeps a `bigint` exactly when it fits in 64 bits, signed or not. */
const SMALLEST_ATTRIBUTE_BIGINT = -(2n ** 63n);
const LARGEST_ATTRIBUTE_BIGINT = 2n ** 64n - 1n;

/**
 * Deepest nesting of arrays and objects inside `attrs` or `resource`. The
 * native JSON reader accepts 128 levels for a whole line, and the line itself
 * takes two of them.
 */
const MAX_ATTRIBUTE_DEPTH = 64;

/** `JSON.rawJSON` exists since Node 21, but the TypeScript library does not declare it yet. */
const rawJSON = (JSON as unknown as { rawJSON(text: string): unknown }).rawJSON;

/** The current time as nanoseconds since 1970 UTC, with millisecond precision. */
export function nowNanos(): bigint {
  return BigInt(Date.now()) * 1_000_000n;
}

/**
 * Turns an event into one line of the JSON Lines text the C ABI reads.
 * Throws `TypeError` when a field or attribute cannot be stored exactly.
 */
export function eventLine(event: Event): string {
  if (typeof event !== "object" || event === null) {
    throw new TypeError("an event must be an object");
  }
  if (typeof event.name !== "string" || event.name === "") {
    throw new TypeError("event name must be a string that is not empty");
  }
  const line: Record<string, unknown> = {
    ts: rawJSON(nanos(event.ts ?? nowNanos(), "ts").toString()),
    name: event.name.toWellFormed(),
  };
  if (event.message !== undefined) line.message = text(event.message, "message");
  if (event.durationNs !== undefined) line.duration_ns = rawJSON(nanos(event.durationNs, "durationNs").toString());
  if (event.traceId !== undefined) line.trace_id = text(event.traceId, "traceId");
  if (event.spanId !== undefined) line.span_id = text(event.spanId, "spanId");
  if (event.parentId !== undefined) line.parent_id = text(event.parentId, "parentId");
  if (event.attrs !== undefined) line.attrs = storableAttributes(event.attrs, "event attrs");
  return JSON.stringify(line);
}

/**
 * Checks attributes and returns a copy that `JSON.stringify` writes exactly:
 * `bigint` values become raw JSON numbers, and lone surrogates in strings and
 * keys are replaced, so the native JSON reader never refuses the text.
 * Throws `TypeError` that names the key path of the first value it cannot store.
 */
export function storableAttributes(value: unknown, path: string): object {
  if (typeof value !== "object" || value === null || Array.isArray(value) || !isPlainObject(value)) {
    throw new TypeError(`${path} must be a plain object`);
  }
  return storableValue(value, path, 0) as object;
}

function storableValue(value: unknown, path: string, depth: number): unknown {
  switch (typeof value) {
    case "string":
      return value.toWellFormed();
    case "boolean":
      return value;
    case "number":
      if (!Number.isFinite(value)) throw new TypeError(`${path} is ${value}; only finite numbers can be stored`);
      return value;
    case "bigint":
      if (value < SMALLEST_ATTRIBUTE_BIGINT || value > LARGEST_ATTRIBUTE_BIGINT) {
        throw new TypeError(`${path} is a bigint outside -(2^63) to 2^64 - 1, so it cannot be stored exactly`);
      }
      return rawJSON(value.toString());
    case "object": {
      if (value === null) return null;
      if (depth >= MAX_ATTRIBUTE_DEPTH) {
        throw new TypeError(`${path} is nested deeper than ${MAX_ATTRIBUTE_DEPTH} levels`);
      }
      if (Array.isArray(value)) {
        const items: unknown[] = [];
        for (let index = 0; index < value.length; index++) {
          items.push(storableValue(value[index], `${path}[${index}]`, depth + 1));
        }
        return items;
      }
      if (!isPlainObject(value)) {
        const kind = (value as { constructor?: { name?: string } }).constructor?.name ?? "object";
        throw new TypeError(`${path} is a ${kind}; only plain objects and arrays can be stored`);
      }
      // A prototype of null makes a key named "__proto__" an ordinary key.
      const entries: Record<string, unknown> = Object.create(null);
      for (const [key, item] of Object.entries(value)) {
        entries[key.toWellFormed()] = storableValue(item, `${path}[${JSON.stringify(key)}]`, depth + 1);
      }
      return entries;
    }
    default:
      throw new TypeError(`${path} is ${typeof value === "undefined" ? "undefined" : `a ${typeof value}`}, which cannot be stored`);
  }
}

function isPlainObject(value: object): boolean {
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function nanos(value: unknown, field: string): bigint {
  if (typeof value !== "bigint" || value < 0n || value > LARGEST_NANOS) {
    throw new TypeError(`event ${field} must be a bigint from 0 to 2^63 - 1 nanoseconds`);
  }
  return value;
}

function text(value: unknown, field: string): string {
  if (typeof value !== "string") {
    throw new TypeError(`event ${field} must be a string`);
  }
  return value.toWellFormed();
}
