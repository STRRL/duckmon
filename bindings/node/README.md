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
| `resource` | plain object | Replaces the store's `resource` for this event. `{}` stores NULL. The OpenTelemetry exporters set it. |

Attribute values, in `attrs`, in an event's `resource` and in the `resource` open option, keep their JSON type. A `number` is stored as the shortest text that reads back as the same `number`, so a whole number above `Number.MAX_SAFE_INTEGER` can show different last digits when SQL reads it as an integer: `2 ** 60` is stored as `1152921504606847000`. Use a `bigint` for whole numbers that large; a `bigint` is stored digit for digit. Accepted values:

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

## OpenTelemetry

`duckmon/opentelemetry` has exporters for the OpenTelemetry JavaScript SDK, 2.x and 3.x. Existing instrumentation then writes spans, log records and metrics into a store, with no collector. The exporters import nothing from OpenTelemetry, so they add no dependency.

Supported: SDK 3.x (`@opentelemetry/sdk-trace`, `sdk-logs` and `sdk-metrics` 3.x, which need Node.js 22.15 or later) and SDK 2.x (`@opentelemetry/core` 2.0.0 or later, `@opentelemetry/sdk-logs` 0.203.0 or later).

### NodeSDK

Put the setup in its own file and load it before the application. An ES module application also needs the loader hook of OpenTelemetry, or the instrumentations do not patch modules it imports:

```sh
node --experimental-loader=@opentelemetry/instrumentation/hook.mjs --import ./telemetry.ts app.ts
```

```ts
// telemetry.ts
import { getNodeAutoInstrumentations } from "@opentelemetry/auto-instrumentations-node";
import { SimpleLogRecordProcessor } from "@opentelemetry/sdk-logs";
import { PeriodicExportingMetricReader } from "@opentelemetry/sdk-metrics";
import { NodeSDK } from "@opentelemetry/sdk-node";
import { SimpleSpanProcessor } from "@opentelemetry/sdk-trace";
import { open } from "duckmon";
import { DuckmonLogRecordExporter, DuckmonMetricExporter, DuckmonSpanExporter } from "duckmon/opentelemetry";

export const store = await open("/var/lib/api/telemetry");
export const sdk = new NodeSDK({
  serviceName: "api",
  spanProcessors: [new SimpleSpanProcessor({ exporter: new DuckmonSpanExporter(store) })],
  logRecordProcessors: [new SimpleLogRecordProcessor({ exporter: new DuckmonLogRecordExporter(store) })],
  metricReaders: [
    new PeriodicExportingMetricReader({ exporter: new DuckmonMetricExporter(store), exportIntervalMillis: 10_000 }),
  ],
  instrumentations: [getNodeAutoInstrumentations()],
});
sdk.start();
```

In the application's shutdown path:

```ts
import { sdk, store } from "./telemetry.ts";

try {
  await sdk.shutdown();
} finally {
  await store.close();
}
```

`sdk.shutdown()` rejects when the store could not write, so the `finally` makes sure the store is still closed.

With SDK 2.x, import `SimpleSpanProcessor` from `@opentelemetry/sdk-trace-base` and create it with `new SimpleSpanProcessor(new DuckmonSpanExporter(store))`. The rest stays the same; use `@opentelemetry/auto-instrumentations-node` 0.81 or earlier.

### Effect setup

`@effect/opentelemetry` 4.1 needs SDK 2.x. Open the store in the layer, so it is closed after the SDK has flushed and shut down:

```ts
import * as NodeSdk from "@effect/opentelemetry/NodeSdk";
import { SimpleLogRecordProcessor } from "@opentelemetry/sdk-logs";
import { PeriodicExportingMetricReader } from "@opentelemetry/sdk-metrics";
import { SimpleSpanProcessor } from "@opentelemetry/sdk-trace-base";
import { open } from "duckmon";
import { DuckmonLogRecordExporter, DuckmonMetricExporter, DuckmonSpanExporter } from "duckmon/opentelemetry";
import { Effect, Layer } from "effect";

export const TelemetryLive = Layer.unwrap(
  Effect.gen(function* () {
    const store = yield* Effect.acquireRelease(
      Effect.promise(() => open("/var/lib/api/telemetry")),
      (store) => Effect.promise(() => store.close()),
    );
    return NodeSdk.layer(() => ({
      resource: { serviceName: "api" },
      spanProcessor: new SimpleSpanProcessor(new DuckmonSpanExporter(store)),
      logRecordProcessor: new SimpleLogRecordProcessor({ exporter: new DuckmonLogRecordExporter(store) }),
      metricReader: new PeriodicExportingMetricReader({
        exporter: new DuckmonMetricExporter(store),
        exportIntervalMillis: 10_000,
      }),
    }));
  }),
);
```

Provide it to the program, for example `NodeRuntime.runMain(program.pipe(Effect.provide(TelemetryLive)))`. Effect also adds each log written inside a span to that span as an event, so such a log gives a log row and a span event row. The `name` of that span event row is the log text, so filter span events by name with care in Effect programs.

### Rules

- Use the simple processors. duckmon already queues and batches off the event loop; a batch processor adds a second queue, a delay, and loses its queue on `process.exit()`.
- Export metrics every 10 seconds, so a 5 minute window holds about 30 snapshots per series.
- Keep cumulative temporality. The metric exporter asks for it, and with Effect leave `metricTemporality` unset. The queries below expect it.
- The exporters never close the store. Shut down the SDK first, then close the store. An export that arrives after `shutdown()` is still written while the store is open.
- Each exporter event carries the resource of its export, so the store's `resource` option applies only to events the application writes itself.
- An export reports `FAILED` when the queue is full, when the store is closed, or when a record could not be converted. The other records of the export are still written. The SDK does not retry, and reports the error through its `diag` logger.
- The SDK holds back spans and log records until its asynchronous resource attributes, such as `host.id`, are ready. That takes about 20 milliseconds after `sdk.start()`, and the SDK only looks when a span or log record ends. `process.exit()` before that loses the records that ended so far. To close this gap, build the resource yourself and wait for it before the application starts:

```ts
import {
  detectResources,
  envDetector,
  hostDetector,
  osDetector,
  processDetector,
  resourceFromAttributes,
  serviceInstanceIdDetector,
} from "@opentelemetry/resources";

const resource = resourceFromAttributes({ "service.name": "api" }).merge(
  detectResources({ detectors: [envDetector, processDetector, hostDetector, osDetector, serviceInstanceIdDetector] }),
);
await resource.waitForAsyncAttributes?.();
const sdk = new NodeSDK({
  resource,
  resourceDetectors: [],
  // The processors, readers and instrumentations of the setup above.
});
```

  Effect resources have no asynchronous attributes, so Effect has no such gap.

### Mapping

`docs/contract.md` describes the mapping in full. In short:

- `name` keeps the OpenTelemetry name, such as the span name `GET /users/:id`.
- `resource` holds the resource attributes, `attrs` the OpenTelemetry attributes. Other fields use keys that start with `span.`, `log.` or `metric.`, and `otel.scope.name` and `otel.scope.version` name the instrumentation scope.
- A span sets `attrs["span.kind"]`, and `attrs.status` and `attrs.error` from its status. Each span event, such as an exception, is a row with the span's `trace_id` and `span_id` and no `duration_ns`.
- A log record has `message` (the body, JSON text for a structured body) and `attrs.level` from its severity.
- A metric gives one row per data point every export. All measured values sit in `attrs["metric.value"]`, so `json_merge_patch(attrs, '{"metric.value":null}')` with `resource` names one series.

### Queries

```sql
-- Slowest spans in the last hour
SELECT name, duration_ns / 1e6 AS ms, trace_id FROM events
WHERE duration_ns IS NOT NULL AND ts >= make_timestamp_ns(epoch_ns(now())) - INTERVAL 1 HOUR
ORDER BY duration_ns DESC LIMIT 20;
```

```sql
-- Error rate per route
SELECT attrs->>'http.route' AS route, count(*) AS requests,
       avg(CASE WHEN attrs->>'status' = 'error' THEN 1 ELSE 0 END) AS error_rate
FROM events
WHERE duration_ns IS NOT NULL AND attrs->>'span.kind' = 'server'
  AND ts >= make_timestamp_ns(epoch_ns(now())) - INTERVAL 1 HOUR
GROUP BY route ORDER BY requests DESC;
```

```sql
-- One trace as a tree (a span whose parent is in another process counts as a root)
WITH RECURSIVE spans AS (
  SELECT * FROM events WHERE trace_id = '4bf92f3577b34da6a3ce929d0e0e4736' AND duration_ns IS NOT NULL
), tree AS (
  SELECT span_id, name, duration_ns, 0 AS depth, [ts] AS path FROM spans
  WHERE parent_id IS NULL OR parent_id NOT IN (SELECT span_id FROM spans)
  UNION ALL
  SELECT s.span_id, s.name, s.duration_ns, t.depth + 1, list_append(t.path, s.ts)
  FROM spans s JOIN tree t ON s.parent_id = t.span_id
)
SELECT repeat('  ', depth) || name AS span, duration_ns / 1e6 AS ms FROM tree ORDER BY path;
```

```sql
-- Logs with the span they were written in
SELECT l.ts, l.attrs->>'level' AS level, l.message, s.name AS span
FROM events l JOIN events s
  ON s.trace_id = l.trace_id AND s.span_id = l.span_id AND s.duration_ns IS NOT NULL
WHERE l.message IS NOT NULL
ORDER BY l.ts DESC LIMIT 50;
```

The metric queries below take the increase of each series within the window. A series that started inside the window, after a restart for example, counts from zero.

```sql
-- Requests per second per route over the last 5 minutes, from the count of a cumulative histogram
WITH series AS (
  SELECT any_value(attrs) AS attrs,
         max((attrs->'metric.value'->>'count')::DOUBLE)
           - CASE WHEN make_timestamp_ns(any_value(attrs->>'metric.start_ns')::BIGINT)
                       >= make_timestamp_ns(epoch_ns(now())) - INTERVAL 5 MINUTE
                  THEN 0 ELSE min((attrs->'metric.value'->>'count')::DOUBLE) END AS increase
  FROM events
  WHERE name = 'http.server.request.duration'
    AND ts >= make_timestamp_ns(epoch_ns(now())) - INTERVAL 5 MINUTE
  GROUP BY resource, json_merge_patch(attrs, '{"metric.value":null}')
)
SELECT attrs->>'http.route' AS route, sum(increase) / 300 AS per_second
FROM series GROUP BY route ORDER BY per_second DESC;
```

```sql
-- Increase of a counter per attribute value over the last 5 minutes
WITH series AS (
  SELECT any_value(attrs) AS attrs,
         max((attrs->>'metric.value')::DOUBLE)
           - CASE WHEN make_timestamp_ns(any_value(attrs->>'metric.start_ns')::BIGINT)
                       >= make_timestamp_ns(epoch_ns(now())) - INTERVAL 5 MINUTE
                  THEN 0 ELSE min((attrs->>'metric.value')::DOUBLE) END AS increase
  FROM events
  WHERE name = 'app.orders'
    AND ts >= make_timestamp_ns(epoch_ns(now())) - INTERVAL 5 MINUTE
  GROUP BY resource, json_merge_patch(attrs, '{"metric.value":null}')
)
SELECT attrs->>'payment.method' AS method, sum(increase) AS orders
FROM series GROUP BY method ORDER BY orders DESC;
```

```sql
-- p95 of a histogram over the last 5 minutes: the upper bound of the bucket that holds it
WITH series AS (
  SELECT json_transform(any_value(attrs)->'metric.value'->'explicit_bounds', '["DOUBLE"]') AS bounds,
         json_transform(arg_max(attrs, ts)->'metric.value'->'bucket_counts', '["DOUBLE"]') AS last_counts,
         CASE WHEN make_timestamp_ns(any_value(attrs->>'metric.start_ns')::BIGINT)
                   >= make_timestamp_ns(epoch_ns(now())) - INTERVAL 5 MINUTE
              THEN NULL
              ELSE json_transform(arg_min(attrs, ts)->'metric.value'->'bucket_counts', '["DOUBLE"]') END AS first_counts
  FROM events
  WHERE name = 'http.server.request.duration' AND attrs->>'http.route' = '/users/:id'
    AND ts >= make_timestamp_ns(epoch_ns(now())) - INTERVAL 5 MINUTE
  GROUP BY resource, json_merge_patch(attrs, '{"metric.value":null}')
),
buckets AS (
  SELECT i, any_value(bounds)[i] AS upper, sum(last_counts[i] - coalesce(first_counts[i], 0)) AS n
  FROM series, unnest(generate_series(1, len(last_counts))) AS t(i)
  GROUP BY i
)
SELECT upper AS p95_at_most FROM buckets
QUALIFY sum(n) OVER (ORDER BY i) >= 0.95 * sum(n) OVER ()
ORDER BY i LIMIT 1;
```

The result is NULL when the p95 falls in the last bucket, which has no upper bound.

```sql
-- Current value of a gauge, per process
SELECT resource->>'service.instance.id' AS instance,
       arg_max((attrs->>'metric.value')::DOUBLE, ts) AS value
FROM events
WHERE name = 'app.queue.length'
  AND ts >= make_timestamp_ns(epoch_ns(now())) - INTERVAL 5 MINUTE
GROUP BY ALL;
```

Compare `ts` with `make_timestamp_ns(epoch_ns(now()))`: `ts` is `TIMESTAMP_NS`, and `now()` minus an interval does not work in the DuckDB built into duckmon. With `duckmon query`, put `--` before SQL that starts with a comment.

## Example

`examples/roundtrip.ts` writes events and reads them back:

```sh
node examples/roundtrip.ts write /tmp/store 3000 0
node examples/roundtrip.ts count /tmp/store 3000
bun examples/roundtrip.ts write /tmp/store 3000 0
```

`examples/opentelemetry.ts` sends spans, log records, metrics and one HTTP request through a real NodeSDK 3.x into a store and prints how many rows of each kind landed:

```sh
node examples/opentelemetry.ts /tmp/otel-store
duckmon --dir /tmp/otel-store query "SELECT name, message, duration_ns, attrs FROM events ORDER BY ts"
```
