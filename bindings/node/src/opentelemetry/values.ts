import { LARGEST_ATTRIBUTE_BIGINT, LARGEST_NANOS, MAX_ATTRIBUTE_DEPTH, SMALLEST_ATTRIBUTE_BIGINT, rawJSON } from "../events.js";
import type { AttributeValue, Attributes } from "../events.js";

/** OpenTelemetry time: whole seconds and nanoseconds since 1970 UTC. */
export type HrTime = readonly [number, number];

/** The fields of an OpenTelemetry resource that the exporters read. */
export interface Resource {
  readonly attributes: Readonly<Record<string, unknown>>;
  readonly asyncAttributesPending?: boolean | undefined;
}

/** The fields of an OpenTelemetry instrumentation scope that the exporters read. */
export interface InstrumentationScope {
  readonly name: string;
  readonly version?: string | undefined;
}

/** Resource attributes already converted, kept as long as the resource lives. */
const resourceCache = new WeakMap<object, Attributes>();

/** Turns OpenTelemetry time into nanoseconds since 1970, from 0 to 2^63 - 1. Never throws for numbers. */
export function toNanos(time: HrTime): bigint {
  const seconds = Number.isFinite(time[0]) ? Math.trunc(time[0]) : 0;
  const nanos = Number.isFinite(time[1]) ? Math.round(time[1]) : 0;
  const total = BigInt(seconds) * 1_000_000_000n + BigInt(nanos);
  if (total < 0n) return 0n;
  if (total > LARGEST_NANOS) return LARGEST_NANOS;
  return total;
}

/**
 * Turns any JavaScript value into a value the store accepts, and never throws.
 * Values JSON cannot hold become text. `undefined` gives `undefined`, which
 * the caller leaves out. `depth` is the nesting level the value will sit at
 * inside `attrs`, where the attributes object itself is level 0.
 */
export function toAttributeValue(value: unknown, depth: number = 1, ancestors: object[] = []): AttributeValue | undefined {
  try {
    return convert(value, depth, ancestors);
  } catch {
    return "[unreadable]";
  }
}

function convert(value: unknown, depth: number, ancestors: object[]): AttributeValue | undefined {
  switch (typeof value) {
    case "string":
    case "boolean":
      return value;
    case "number":
      return Number.isFinite(value) ? value : String(value);
    case "bigint":
      return value < SMALLEST_ATTRIBUTE_BIGINT || value > LARGEST_ATTRIBUTE_BIGINT ? value.toString() : value;
    case "undefined":
      return undefined;
    case "symbol":
      return value.toString();
    case "function":
      return String(value);
    case "object":
      return convertObject(value, depth, ancestors);
  }
}

function convertObject(value: object | null, depth: number, ancestors: object[]): AttributeValue {
  if (value === null) return null;
  if (ancestors.includes(value)) return "[circular]";
  if (value instanceof Uint8Array) {
    return Buffer.from(value.buffer, value.byteOffset, value.byteLength).toString("base64");
  }
  if (value instanceof Date) {
    return Number.isNaN(value.getTime()) ? "Invalid Date" : value.toISOString();
  }
  const isArray = Array.isArray(value);
  if (!isArray && !isPlainObject(value)) return String(value);
  if (depth >= MAX_ATTRIBUTE_DEPTH) return "[too deep]";
  ancestors.push(value);
  try {
    if (isArray) {
      const items: AttributeValue[] = [];
      for (let index = 0; index < value.length; index++) {
        // An undefined item and a hole become null, so the other items keep their positions.
        items.push(toAttributeValue(value[index], depth + 1, ancestors) ?? null);
      }
      return items;
    }
    return convertEntries(value as Readonly<Record<string, unknown>>, depth, ancestors);
  } finally {
    ancestors.pop();
  }
}

/** Converts each property on its own, so one unreadable property does not hide the others. */
function convertEntries(value: Readonly<Record<string, unknown>>, depth: number, ancestors: object[]): Attributes {
  // A prototype of null makes a key named "__proto__" an ordinary key.
  const entries: Attributes = Object.create(null);
  for (const key of Object.keys(value)) {
    let item: AttributeValue | undefined;
    try {
      item = toAttributeValue(value[key], depth + 1, ancestors);
    } catch {
      item = "[unreadable]";
    }
    if (item !== undefined) entries[key] = item;
  }
  return entries;
}

/**
 * Turns OpenTelemetry attributes into attributes the store accepts. `depth`
 * is the nesting level of the attributes object itself inside `attrs`.
 */
export function toAttributes(attributes: unknown, depth: number = 0): Attributes {
  const value = toAttributeValue(attributes, depth);
  if (typeof value === "object" && value !== null && !Array.isArray(value)) return value;
  return Object.create(null) as Attributes;
}

/** The attributes of a resource. A resource whose attributes are settled is converted only once. */
export function resourceAttributes(resource: Resource): Attributes {
  const cached = resourceCache.get(resource);
  if (cached !== undefined) return cached;
  const attributes = toAttributes(resource.attributes);
  if (resource.asyncAttributesPending !== true) resourceCache.set(resource, attributes);
  return attributes;
}

/** The semantic convention keys that name the instrumentation scope. */
export function scopeAttributes(scope: InstrumentationScope): Attributes {
  const attributes: Attributes = Object.create(null);
  if (typeof scope.name === "string" && scope.name !== "") attributes["otel.scope.name"] = scope.name;
  if (typeof scope.version === "string" && scope.version !== "") attributes["otel.scope.version"] = scope.version;
  return attributes;
}

/** JSON text of a converted value, with each `bigint` written as its exact digits. */
export function jsonText(value: AttributeValue): string {
  return JSON.stringify(value, (_key, item: unknown) => (typeof item === "bigint" ? rawJSON(item.toString()) : item));
}

/** The text when it is a string that is not empty, else the fallback. The store refuses empty names. */
export function nameOr(name: unknown, fallback: string): string {
  return typeof name === "string" && name !== "" ? name : fallback;
}

function isPlainObject(value: object): boolean {
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}
