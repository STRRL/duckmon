# duckmon storage contract

This document describes the files duckmon writes. Any tool that reads Parquet can read them. Contract version: `1`.

## Columns

Each Parquet file stores these columns, in this order:

| column | DuckDB type | null | meaning |
|---|---|---|---|
| ts | TIMESTAMP_NS | no | When the event happened, UTC, nanoseconds. For a span, the start time. |
| name | VARCHAR | no | Stable, low cardinality event name in lowercase dotted words, such as `http.request`. |
| message | VARCHAR | yes | Human readable log text. Set only for logs. |
| duration_ns | BIGINT | yes | Span length in nanoseconds. Set only for spans. |
| trace_id | VARCHAR | yes | 32 lowercase hex characters (W3C / OpenTelemetry trace ID). |
| span_id | VARCHAR | yes | 16 lowercase hex characters. For a span, its own ID. For a log inside a span, the enclosing span. |
| parent_id | VARCHAR | yes | Parent span ID of a span. NULL for a root span. |
| resource | VARCHAR holding JSON object text | yes | Describes the producing process, such as `{"service.name":"api"}`. |
| attrs | VARCHAR holding JSON object text | yes | Event attributes. NULL when there are none. |

Every column marked "yes" in the null column is stored as an `OPTIONAL` Parquet field in every file, even when a file holds no NULL in it.

Readers that use hive partitioning also get `date DATE`. It is the UTC calendar date of `ts`, computed as floor(ts / 1 day). It is stored in the path only.

## Layout

```
<dir>/date=YYYY-MM-DD/<writer ID>-<sequence>.parquet
```

- A file never holds rows from two dates.
- `<writer ID>` is a UUID v7 written as 32 lowercase hex characters. Each `open` creates a new one.
- `<sequence>` is a counter per writer. It starts at 1, is written with 8 digits padded with zeros, and is never reused.
- A file that is still being written is named `.<writer ID>-<sequence>.parquet.tmp` in the same directory, and is renamed when it is complete. Readers ignore these files.
- Files never change after they are renamed. Each one carries the Parquet key value metadata `duckmon_contract = '1'`.
- Many processes may write to one directory at the same time. Each one only adds its own files. There are no locks.

## Change rules

- Columns may only be added, and new columns must allow NULL.
- Columns are never renamed and never change type.
- Readers must use `union_by_name` so files with fewer columns still read.

## How to read

DuckDB:

```sql
SELECT * FROM read_parquet('<dir>/date=*/*.parquet',
    hive_partitioning = true, union_by_name = true, hive_types = {'date': 'DATE'});
```

polars:

```python
pl.scan_parquet('<dir>/date=*/*.parquet', hive_partitioning=True)
```

Use the glob. The bare directory fails while a `.tmp` file exists.

When `<dir>` contains a glob character, write it in the DuckDB pattern inside its own brackets so it matches literally: `[` as `[[]`, `]` as `[]]`, `*` as `[*]`, `?` as `[?]`. For example, `/data/logs[1]` becomes `/data/logs[[]1[]]/date=*/*.parquet`.

pandas or pyarrow:

```python
pd.read_parquet('<dir>')
```

Filter on `date` to skip whole files. Never use `CAST(ts AS DATE)` or `ts::TIMESTAMP`. Casting TIMESTAMP_NS rounds, so `23:59:59.999999999` becomes the next day. Compare `ts` directly.

## Mapping conventions

There is no kind column. Each kind of event fills a different set of columns.

Log:
- `name` is the logger or module target, such as `app.http`.
- `message` holds the text.
- `attrs.level` is one of `error`, `warn`, `info`, `debug`, `trace`.
- `trace_id` and `span_id` are set when the log happens inside a span.

Span:
- Written once, when it ends.
- `ts` is the start. `duration_ns` is end minus start.
- `trace_id` and `span_id` are required. `parent_id` is NULL for a root span.
- `name` is the operation.
- Optional `attrs.status` (`ok` or `error`) and `attrs.error`.

Metric snapshot:
- A periodic event. `name` is the metric group, such as `process.stats`.
- `attrs` holds the numeric values.
- Counters are cumulative since the process started. Gauges hold the current value.

Telling them apart:
- A span has `span_id IS NOT NULL AND duration_ns IS NOT NULL`.
- A log has `message IS NOT NULL`.
- Everything else is a plain event or a metric snapshot.

Attribute keys are lowercase dotted words. OpenTelemetry semantic convention names are recommended. `service.name` is recommended in `resource`.
