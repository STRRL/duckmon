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
- Files never change after they are renamed. Each one carries the Parquet key value metadata `duckmon_contract = '1'`. A finished file can later be deleted by compaction or retention.
- Many processes may write to one directory at the same time. Each one only adds its own files. Writers never lock.

### Internal names

Any name under `<dir>` that starts with `.` is internal, and readers ignore it:

- `.compact.lock` in `<dir>`.
- `.deleting-date=YYYY-MM-DD-<run ID>` directories in `<dir>`, made while retention deletes a day.
- `.<writer ID>-<sequence>.parquet.tmp` files in day directories.

duckmon never creates a dot file ending in `.parquet` in a day directory, because the DuckDB `*` pattern also matches names that start with `.`. The pattern `date=*/*.parquet` never matches the internal directories.

## Compaction

`compact` replaces a group of files of one day with one file. It only works on days that ended at least the settle time ago (default 1 hour), and only on files at least that old. The new file follows the normal naming rule with a writer ID of its own, and carries a second key value metadata entry, `duckmon_compacted_from`: a comma separated list of the file names, in the same directory, whose rows it holds. If a listed file still exists, it is a leftover duplicate, and the next `compact` deletes it.

For each group, compaction writes the new file under a `.tmp` name, renames it into place, and then deletes the input files right away. What readers see:

- A reader never sees a partly written file.
- Once `compact` returns success, every row is in exactly one data file. Rows are never lost.
- While one group is being replaced, from the rename of the merged file to the unlink of its last input, any read of the directory can meet that group. This window grows with the number of inputs: deleting 1500 inputs takes about 50 ms. A filter on `date` does not avoid it, because `union_by_name = true` opens the footer of every file the pattern matches.
- A read that meets the window either fails with an IO error, because a file it listed was deleted before it opened it, or counts that group's rows twice.
- Running the read again gives the right answer.
- After a crash between rename and delete, duplicates stay visible until the next `compact` finishes.

Run a read again when it fails with an IO error. Only days that have ended are compacted, so the directory of the current UTC day is never compacted. Only a pattern limited to that one directory, such as `<dir>/date=<today>/*.parquet`, is sure to avoid the window. Any pattern that includes a past day can meet it.

`compact --dry-run` prints what it would do, changes nothing and takes no lock.

Retention (`compact --keep-days N`) deletes whole `date=` directories. A day disappears in one rename, so a reader that listed its files beforehand gets an IO error, never a partial day. A writer that later writes an event for a deleted day creates that day directory again, and the next retention run deletes it again.

Only `compact` takes a lock: an exclusive `flock` on `.compact.lock`. A second run waits for the first. The lock may not work across hosts on a network file system, so compact one directory from one host only.

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

When `<dir>` contains a glob character, write it inside its own brackets so it matches literally: `[` as `[[]`, `]` as `[]]`, `*` as `[*]`, `?` as `[?]`. For example, `/data/logs[1]` becomes `/data/logs[[]1[]]/date=*/*.parquet`. This applies to both the DuckDB and the polars pattern. Without it the pattern can match a different directory, such as `/data/logs1`, and return its rows with no error.

`<dir>` must not contain a backslash, except as a path separator on Windows. DuckDB cannot list files below such a directory, so `duckmon::open` refuses it.

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
