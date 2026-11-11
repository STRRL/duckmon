# duckmon for Node.js

An embedded telemetry store. Your application writes logs, spans and metric snapshots as events. They land as Parquet files in a directory, and you query them with SQL through an in process DuckDB. Many processes may share one directory. The file format is described in `docs/contract.md` of the duckmon repository.

The package is a Node-API addon (`duckmon.node`) that calls the duckmon C ABI. It runs on Node.js 22 or later and on Bun.

## Build

The package is not published yet. Build it from the repository:

```sh
cd bindings/node
npm ci
npm run build
```

`npm run build` runs `cargo build -p duckmon-node --release`, copies the addon to `duckmon.node` and compiles the TypeScript sources to `dist`. `npm run check` only type checks.

## Use

```ts
import { open, nowNanos } from "duckmon";

const store = await open("/var/lib/api/telemetry", { resource: { "service.name": "api" } });

// A log.
store.write({ name: "app.http", message: "handled request", attrs: { level: "info", "http.status_code": 200 } });

// A span.
const start = nowNanos();
const startClock = process.hrtime.bigint();
// ... work ...
store.write({
  name: "http.request",
  ts: start,
  durationNs: process.hrtime.bigint() - startClock,
  traceId: "4bf92f3577b34da6a3ce929d0e0e4736",
  spanId: "00f067aa0ba902b7",
});

// A metric snapshot.
store.write({ name: "process.stats", attrs: { "heap.bytes": process.memoryUsage().heapUsed } });

const rows = await store.query<{ name: string; ts: bigint; date: string }>(
  "SELECT name, ts, date FROM events ORDER BY ts DESC LIMIT 10",
);
await store.close();
```

### Events

| field | type | meaning |
|---|---|---|
| `name` | `string` | Stable, low cardinality name in lowercase dotted words, such as `http.request`. Required. |
| `ts` | `bigint` | Nanoseconds since 1970 UTC. For a span, the start time. Default `nowNanos()`. |
| `message` | `string` | Log text. Set only for logs. |
| `durationNs` | `bigint` | Span length in nanoseconds. Set only for spans. |
| `traceId`, `spanId`, `parentId` | `string` | Trace ID (32 hex characters) and span IDs (16 hex characters). |
| `attrs` | plain object | Attributes, see below. |

Attribute values, in `attrs` and in the `resource` open option, keep their JSON type. A `number` is stored as the shortest text that reads back as the same `number`, so a whole number above `Number.MAX_SAFE_INTEGER` can show different last digits when SQL reads it as an integer: `2 ** 60` is stored as `1152921504606847000`. Use a `bigint` for whole numbers that large; a `bigint` is stored digit for digit. Accepted values:

- strings, finite numbers, `true`, `false` and `null`,
- `bigint` from `-(2n ** 63n)` to `2n ** 64n - 1n`,
- arrays and plain objects (prototype `Object.prototype` or `null`) of accepted values, nested at most 64 levels.

Anything else throws `TypeError` that names the key path, for example `NaN`, `Infinity`, a larger `bigint`, `undefined`, functions, symbols, `Date`, `Map`, `Set` and class instances. Turn such values into one of the accepted forms first, for example a `Date` into `date.toISOString()`.

`write()` takes one event or an array and returns at once. It throws `TypeError` when a field or attribute cannot be stored, and then none of the given events is written. The events are queued in JavaScript and sent to the native writer on the libuv thread pool, so the event loop does not wait for disk or DuckDB. When `maxPendingEvents` events (default 100000) are already waiting, new events are dropped and counted in `store.droppedEvents`. If the store refuses a batch, all its events are counted there too, and the next `flush()` or `close()` rejects with the error. Each store warns about dropped events once.

Events reach a Parquet file at the next flush, every `flushIntervalMs` (default 5000), at `flush()` or at `close()`. `query()` sees every event written before it, also the ones not flushed yet. `flush()` and `query()` wait only for the events written before them, so they finish while the application keeps writing.

`flushIntervalMs`, `channelCapacity` (at most 1000000) and `maxPendingEvents` must be whole numbers of at least 1, or `open()` throws `TypeError`.

### Query results

`query()` returns plain row objects, one property per column. Values follow the column type, the same way at the top level and inside lists, structs and maps:

| DuckDB type | JavaScript value |
|---|---|
| `BOOLEAN` | `boolean` |
| `TINYINT`, `SMALLINT`, `INTEGER`, `UTINYINT`, `USMALLINT`, `UINTEGER` | `number` |
| `BIGINT`, `UBIGINT` | `bigint` |
| `HUGEINT`, `DECIMAL` with scale 0 | `bigint` |
| `UHUGEINT` | `bigint`, but DuckDB exports it as a signed 128 bit decimal, so values above 2^127 - 1 arrive wrong (the largest comes back as `-1n`); cast to `VARCHAR` |
| `DECIMAL` with a scale above 0 | exact decimal text, such as `"-0.01"` |
| `FLOAT`, `DOUBLE` | `number`, also `NaN` and `Infinity` |
| `VARCHAR`, `UUID`, `JSON`, `ENUM` | `string`; `attrs` and `resource` are JSON text, so read single values in SQL, for example `attrs->>'level'` |
| `BLOB`, `BIT` | `Uint8Array` with DuckDB's bytes; cast `BIT` to `VARCHAR` for its text |
| `DATE` | `"YYYY-MM-DD"`; years after 9999 have more digits, years before 1 have a minus sign (year 0 is 1 BC), and infinite dates give `"infinity"` and `"-infinity"` |
| `TIMESTAMP`, `TIMESTAMP_S`, `TIMESTAMP_MS`, `TIMESTAMP_NS`, `TIMESTAMPTZ` | `bigint` nanoseconds since 1970 UTC; the infinite timestamps give the largest or smallest stored value in nanoseconds, such as `9223372036854775807000n` for `TIMESTAMP` |
| `TIME`, `TIMETZ` | `bigint` nanoseconds since midnight; DuckDB drops the offset of `TIMETZ`, so cast it to `VARCHAR` |
| `LIST`, fixed size `ARRAY` | array |
| `STRUCT` | plain object |
| `MAP` | `Map` |
| NULL | `null` |
| `INTERVAL`, `UNION`, `BIGNUM` and other types | `query()` rejects with `DuckmonError` code `INVALID_ARGUMENT`, which names the column; cast it in SQL, for example `CAST(v AS VARCHAR)` |

A query whose result has two columns with the same name also rejects; give each column its own name with `AS`. A column named `__proto__` is an ordinary property.

Results are decoded on the main thread, so use `LIMIT` for large results.

### Errors

Failures of the store are `DuckmonError` with a `code`: `INVALID_ARGUMENT`, `INVALID_EVENT`, `INVALID_DIRECTORY`, `DATABASE` (for example bad SQL), `FILE`, `CLOSED`, `COMPACT` or `INTERNAL`. `write()` after `close()` throws with code `CLOSED`. If the native writer fails, a warning is printed and the next `flush()` or `close()` rejects with the error.

While `compact` replaces files of a past day, a query can fail with a `DATABASE` IO error. Running it again gives the right answer.

### Process exit

Call `close()` on every store you open, from your own shutdown path. Each open store keeps a writer thread and a DuckDB instance, and a store that is never closed is held until the process exits: garbage collection never frees it. As a safety net, duckmon also closes open stores:

- at a normal end of the event loop (`beforeExit`), asynchronously,
- on `process.exit()`, an uncaught exception or an unhandled rejection (`exit`), synchronously.

In a worker thread, `worker.terminate()` runs no exit handler, so events still queued in that worker are lost. Close the store inside the worker before it is terminated.

duckmon installs no signal handlers. When the process ends by SIGINT or SIGTERM without a handler, or crashes, events not yet in a Parquet file are lost: at most about one flush interval plus the queue. Files on disk stay consistent.

### Compaction

```ts
import { compact } from "duckmon";

const report = await compact("/var/lib/api/telemetry", { keepDays: 7 });
```

`compact()` merges the small files of past days and, with `keepDays`, deletes older days. `dryRun: true` only reports what it would do. The `duckmon compact` command line tool does the same.

### Effect

With Effect, open the store as a scoped resource, and take `ts` from `Clock.currentTimeNanos`, which is already a `bigint`:

```ts
const store = Effect.acquireRelease(
  Effect.promise(() => open(dir)),
  (store) => Effect.promise(() => store.close()),
);
```

## Example

`examples/roundtrip.ts` writes events and reads them back:

```sh
node examples/roundtrip.ts write /tmp/store 3000 0
node examples/roundtrip.ts count /tmp/store 3000
bun examples/roundtrip.ts write /tmp/store 3000 0
```
