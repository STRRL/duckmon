import { DataType, TimeUnit, makeVector, tableFromIPC } from "apache-arrow";
import type { Data, Field } from "apache-arrow";

import { DuckmonError } from "./native.js";

/** One query result row: column name to value. */
export type Row = Record<string, unknown>;

/** Reads the value at one index of a column. */
type Reader = (index: number) => unknown;

const NANOS_PER_UNIT: Record<TimeUnit, bigint> = {
  [TimeUnit.SECOND]: 1_000_000_000n,
  [TimeUnit.MILLISECOND]: 1_000_000n,
  [TimeUnit.MICROSECOND]: 1_000n,
  [TimeUnit.NANOSECOND]: 1n,
};

/** DuckDB stores the dates infinity and -infinity as these day numbers. */
const INFINITY_DAYS = 2147483647;

const MILLISECONDS_PER_DAY = 86_400_000n;

/**
 * Reads an Arrow IPC stream into plain rows. The same rules apply at the top
 * level and inside lists, structs and maps; the README lists them. Throws
 * `DuckmonError` for a column type with no exact JavaScript value and for two
 * columns with the same name.
 */
export function rowsFromArrowIPC(bytes: Uint8Array): Row[] {
  const table = tableFromIPC(bytes);
  const fields = table.schema.fields;
  const names = new Set<string>();
  for (const field of fields) {
    if (names.has(field.name)) {
      throw new DuckmonError(
        "INVALID_ARGUMENT",
        `the query returns more than one column named "${field.name}"; give each column its own name with AS`,
      );
    }
    names.add(field.name);
  }
  const rows: Row[] = [];
  for (const batch of table.batches) {
    const readers = fields.map((field, index) => reader(field, batch.data.children[index]!, field.name));
    for (let index = 0; index < batch.numRows; index++) {
      const row: Row = {};
      fields.forEach((field, column) => setOwn(row, field.name, readers[column]!(index)));
      rows.push(row);
    }
  }
  return rows;
}

/** Sets a property so that a key named "__proto__" becomes an ordinary own property. */
function setOwn(object: Record<string, unknown>, key: string, value: unknown): void {
  if (key === "__proto__") {
    Object.defineProperty(object, key, { value, enumerable: true, writable: true, configurable: true });
  } else {
    object[key] = value;
  }
}

/** A reader that gives `null` for NULL. `column` is the top level column, named in errors. */
function reader(field: Field, data: Data, column: string): Reader {
  const extension = field.metadata.get("ARROW:extension:name");
  if (extension !== undefined) {
    throw unsupportedType(column, extensionTypeName(extension, field.metadata.get("ARROW:extension:metadata")));
  }
  const read = valueReader(field.type, data, column);
  return (index) => (data.getValid(index) ? read(index) : null);
}

function valueReader(type: DataType, data: Data, column: string): Reader {
  if (DataType.isNull(type)) {
    return () => null;
  }
  if (DataType.isInt(type) && type.bitWidth === 64) {
    const values = data.values as BigInt64Array | BigUint64Array;
    return (index) => values[index];
  }
  if (DataType.isInt(type) || DataType.isFloat(type) || DataType.isBool(type) || isText(type)) {
    const vector = makeVector(data);
    return (index) => vector.get(index);
  }
  if (isBinary(type)) {
    const vector = makeVector(data);
    // Copy the bytes, so a row does not keep the whole IPC buffer alive.
    return (index) => (vector.get(index) as Uint8Array).slice();
  }
  if (DataType.isDecimal(type)) {
    const values = data.values as Uint32Array;
    const words = type.bitWidth / 32;
    const scale = type.scale;
    return (index) => decimalValue(values, index, words, scale);
  }
  if (DataType.isDate(type)) {
    const values = data.values as Int32Array | BigInt64Array;
    return (index) => dateText(dayNumber(values[index]!));
  }
  if (DataType.isTimestamp(type) || DataType.isTime(type)) {
    // `get()` gives float milliseconds and loses nanoseconds, so read the raw values.
    const values = data.values as BigInt64Array | Int32Array;
    const scale = NANOS_PER_UNIT[type.unit];
    return (index) => BigInt(values[index]!) * scale;
  }
  if (DataType.isList(type) || DataType.isLargeList(type)) {
    const offsets = data.valueOffsets as Int32Array | BigInt64Array;
    const item = reader(type.children[0]!, data.children[0]!, column);
    return (index) => readRange(item, Number(offsets[index]), Number(offsets[index + 1]));
  }
  if (DataType.isFixedSizeList(type)) {
    const size = type.listSize;
    const item = reader(type.children[0]!, data.children[0]!, column);
    return (index) => readRange(item, index * size, (index + 1) * size);
  }
  if (DataType.isStruct(type)) {
    const members = type.children.map((field, member) => ({
      name: field.name,
      read: reader(field, data.children[member]!, column),
    }));
    return (index) => {
      const object: Record<string, unknown> = {};
      for (const member of members) setOwn(object, member.name, member.read(index));
      return object;
    };
  }
  if (DataType.isMap(type)) {
    const offsets = data.valueOffsets as Int32Array;
    const entries = data.children[0]!;
    const entryType = type.children[0]!.type as DataType;
    const readKey = reader(entryType.children[0]!, entries.children[0]!, column);
    const readValue = reader(entryType.children[1]!, entries.children[1]!, column);
    return (index) => {
      const map = new Map<unknown, unknown>();
      for (let entry = offsets[index]!; entry < offsets[index + 1]!; entry++) {
        map.set(readKey(entry), readValue(entry));
      }
      return map;
    };
  }
  if (DataType.isDictionary(type) && data.dictionary?.data.length === 1) {
    const keys = data.values as Int32Array;
    const word = valueReader(type.dictionary, data.dictionary.data[0]!, column);
    return (index) => word(Number(keys[index]));
  }
  throw unsupportedType(column, String(type));
}

/** DuckDB names its own types, such as BIGNUM, in the extension metadata. */
function extensionTypeName(extension: string, metadata: string | undefined): string {
  try {
    const typeName = (JSON.parse(metadata ?? "") as { type_name?: unknown }).type_name;
    if (typeof typeName === "string") return typeName.toUpperCase();
  } catch {
    // Metadata that is not JSON: name the extension instead.
  }
  return extension;
}

function isText(type: DataType): boolean {
  return DataType.isUtf8(type) || DataType.isLargeUtf8(type) || DataType.isUtf8View(type);
}

function isBinary(type: DataType): boolean {
  return (
    DataType.isBinary(type) ||
    DataType.isLargeBinary(type) ||
    DataType.isBinaryView(type) ||
    DataType.isFixedSizeBinary(type)
  );
}

function readRange(item: Reader, begin: number, end: number): unknown[] {
  const items: unknown[] = [];
  for (let index = begin; index < end; index++) items.push(item(index));
  return items;
}

function unsupportedType(column: string, typeName: string): DuckmonError {
  return new DuckmonError(
    "INVALID_ARGUMENT",
    `column "${column}" has the type ${typeName}, which has no exact JavaScript value; ` +
      `cast it in SQL, for example CAST(${column} AS VARCHAR)`,
  );
}

/** A decimal: a `bigint` when the scale is 0 or less, otherwise exact decimal text such as "-0.01". */
function decimalValue(values: Uint32Array, index: number, words: number, scale: number): bigint | string {
  let raw = 0n;
  for (let word = words - 1; word >= 0; word--) {
    raw = (raw << 32n) | BigInt(values[index * words + word]!);
  }
  const value = BigInt.asIntN(words * 32, raw);
  if (scale <= 0) return value * 10n ** BigInt(-scale);
  const digits = (value < 0n ? -value : value).toString().padStart(scale + 1, "0");
  return `${value < 0n ? "-" : ""}${digits.slice(0, -scale)}.${digits.slice(-scale)}`;
}

/** Days since 1970 from a Date32 day number or a Date64 millisecond count. */
function dayNumber(value: number | bigint): number {
  if (typeof value === "number") return value;
  const days = value / MILLISECONDS_PER_DAY;
  return Number(value % MILLISECONDS_PER_DAY < 0n ? days - 1n : days);
}

/**
 * "YYYY-MM-DD" for days since 1970, in the proleptic Gregorian calendar.
 * Years after 9999 have more digits, years before 1 are written with a minus
 * sign (year 0 is 1 BC), and DuckDB's infinite dates give "infinity" and "-infinity".
 */
function dateText(days: number): string {
  if (days === INFINITY_DAYS) return "infinity";
  if (days === -INFINITY_DAYS) return "-infinity";
  // Civil date from a day number, following Howard Hinnant's civil_from_days.
  const shifted = days + 719468;
  const era = Math.floor(shifted / 146097);
  const dayOfEra = shifted - era * 146097;
  const yearOfEra = Math.floor(
    (dayOfEra - Math.floor(dayOfEra / 1460) + Math.floor(dayOfEra / 36524) - Math.floor(dayOfEra / 146096)) / 365,
  );
  const dayOfYear = dayOfEra - (365 * yearOfEra + Math.floor(yearOfEra / 4) - Math.floor(yearOfEra / 100));
  const monthFromMarch = Math.floor((5 * dayOfYear + 2) / 153);
  const day = dayOfYear - Math.floor((153 * monthFromMarch + 2) / 5) + 1;
  const month = monthFromMarch < 10 ? monthFromMarch + 3 : monthFromMarch - 9;
  const year = yearOfEra + era * 400 + (month <= 2 ? 1 : 0);
  const yearText = `${year < 0 ? "-" : ""}${String(Math.abs(year)).padStart(4, "0")}`;
  return `${yearText}-${String(month).padStart(2, "0")}-${String(day).padStart(2, "0")}`;
}
