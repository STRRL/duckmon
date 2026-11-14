# duckmon for Go

An embedded telemetry store. Your application writes logs, spans and metric snapshots as events. They land as Parquet files in a directory, and you query them with SQL through an in process DuckDB. Many processes may share one directory. The file format is described in `docs/contract.md` of the duckmon repository.

The package `duckmon` (module `github.com/strrl/duckmon/bindings/go`) calls the duckmon C ABI through cgo and links the static library `libduckmon_ffi.a` into your binary.

## Build

You need Go 1.26 or later, a C compiler and Rust. The module is not published yet, and the module alone does not build: it needs the header and the static library of a duckmon clone. From the root of the clone:

```sh
cargo build -p duckmon-ffi --release
```

This writes `target/release/libduckmon_ffi.a`. When `CARGO_TARGET_DIR` is set in your environment, add `--target-dir target`, because the package looks for the library in `target/release` of the clone.

Then run the example from `bindings/go`:

```sh
cd bindings/go
go run ./examples/roundtrip write /tmp/store 3000 0
```

To use the package from your own module, point it at the clone and let Go fill in `go.sum`:

```sh
go mod edit -require=github.com/strrl/duckmon/bindings/go@v0.0.0
go mod edit -replace=github.com/strrl/duckmon/bindings/go=/path/to/duckmon/bindings/go
go mod tidy
```

`go work use /path/to/duckmon/bindings/go` works too. Import it as `duckmon "github.com/strrl/duckmon/bindings/go"`.

When the build fails:

- `no such file` naming `.../target/release/libduckmon_ffi.a`: run the `cargo build` above.
- `'duckmon.h' file not found`: the module came from the module proxy. Use `replace` or `go work` with a local clone.

Supported: macOS and Linux. The macOS link flags are checked on darwin/arm64. The Linux link flags are not checked yet. Not supported yet: Windows, prebuilt libraries and cross compilation.

## Linking

The library is linked statically, so a program is one file with nothing to look up at run time. On darwin/arm64 the example program is 95.1 MB, or 72.4 MB when built with `-ldflags=-s`. Building it takes about 10 s with an empty Go build cache and about 1 s after a change to this package. It starts and counts the rows of an empty store in 0.02 s; `Open` takes 6 to 26 ms. The first start of a new binary on macOS takes about 0.7 s once, which is the system's check of a new program.

With `CGO_ENABLED=0`, or on another system, the build stops with `undefined: duckmon_needs_cgo_on_darwin_or_linux`. Go turns cgo off by itself when it finds no C compiler.

On macOS, `.cargo/config.toml` of the repository builds the library for macOS 11.0, so the Go link prints no warnings. If you build your program with `MACOSX_DEPLOYMENT_TARGET` set below your macOS version, the linker warns about objects built for a newer macOS: the Rust standard library, which Rust ships prebuilt, and C objects from the Go build cache. The program still works.

## Use

```go
package main

import (
	"fmt"
	"log"
	"runtime"
	"time"

	duckmon "github.com/strrl/duckmon/bindings/go"
)

func main() {
	if err := run(); err != nil {
		log.Fatal(err)
	}
}

func run() error {
	store, err := duckmon.Open("telemetry", duckmon.Options{
		Resource: map[string]any{"service.name": "api"},
	})
	if err != nil {
		return err
	}
	defer store.Close()

	// A log.
	_ = store.Write(duckmon.Event{
		Name:    "app.http",
		Message: new("handled request"),
		Attrs:   map[string]any{"level": "info", "http.status_code": 200},
	})

	// A span.
	start := time.Now()
	// ... work ...
	_ = store.Write(duckmon.Event{
		Name:     "http.request",
		Time:     start,
		Duration: new(time.Since(start)),
		TraceID:  "4bf92f3577b34da6a3ce929d0e0e4736",
		SpanID:   "00f067aa0ba902b7",
	})

	// A metric snapshot.
	var memory runtime.MemStats
	runtime.ReadMemStats(&memory)
	_ = store.Write(duckmon.Event{Name: "process.stats", Attrs: map[string]any{"heap.bytes": memory.HeapAlloc}})

	result, err := store.Query("SELECT name, ts, date FROM events ORDER BY ts DESC LIMIT 10")
	if err != nil {
		return err
	}
	for _, row := range result.Rows {
		fmt.Println(row[0].(string), row[1].(time.Time).Format(time.RFC3339Nano), row[2].(time.Time).Format(time.DateOnly))
	}
	return store.Close()
}
```

`Close` is safe to defer and to call once more at the end: the second call only returns an error with `CodeClosed`, which the deferred call ignores here.

### Options

| field | meaning |
|---|---|
| `FlushInterval` | How often buffered events are written to a new Parquet file, in whole milliseconds. Zero means 5 seconds. Above zero it must be at least 1 millisecond. |
| `ChannelCapacity` | How many batches may wait for the native writer thread, at most 1000000. Zero means 1024. |
| `MaxPendingEvents` | How many events `Write` keeps in Go while the native writer is busy. Zero means 100000. |
| `Resource` | Stored in the `resource` column of every event that sets no `Resource` of its own, such as `{"service.name": "api"}`. |

A negative value, or a value outside these ranges, makes `Open` return an error with `CodeInvalidArgument`.

## Events

| field | type | meaning |
|---|---|---|
| `Name` | `string` | Stable, low cardinality name in lowercase dotted words, such as `http.request`. Required. |
| `Time` | `time.Time` | When the event happened, stored in `ts` with nanosecond precision. For a span, the start time. The zero value means `time.Now()` at `Write`. |
| `Message` | `*string` | Log text. Set only for logs. |
| `Duration` | `*time.Duration` | Span length, stored in `duration_ns`. Set only for spans. Must not be negative. |
| `TraceID`, `SpanID`, `ParentID` | `string` | Trace ID (32 lowercase hex characters) and span IDs (16 lowercase hex characters). Empty stores NULL. |
| `Attrs` | `map[string]any` | Attributes, see below. Nil or empty stores NULL. |
| `Resource` | `map[string]any` | Replaces the store's `Resource` for this event when not nil. An empty, non nil map stores NULL. |

A pointer field means "set": nil stores NULL, so an empty log message (`new("")`) and a zero length span (`new(time.Duration(0))`) stay what they are. Write them with `new("text")` or `new(d)`, which needs Go 1.26; with an older `go` line in your `go.mod`, take the address of a variable.

`Time` must be from 1970 to 2262-04-11T23:47:16.854775807Z. A `time.Time` from `time.Now()` carries a monotonic clock reading as well, so `Time: start, Duration: new(time.Since(start))` measures the span with the monotonic clock and stores the start with the wall clock. The very last instant of that range is the value DuckDB shows as `infinity`, so `epoch_ns(ts)` is NULL for it in SQL; Go reads it back exactly.

Attribute values, in `Attrs`, in an event's `Resource` and in `Options.Resource`, are checked by their Go kind. Methods such as `String`, `MarshalJSON` or `MarshalText` are never called. Accepted values:

| Go value | stored as |
|---|---|
| `nil` | `null` |
| `bool` | `true` or `false` |
| `string` and named string types, `json.Number` included | JSON string; invalid UTF 8 becomes U+FFFD, so two keys that differ only in invalid bytes become one key and one value is lost |
| `int`, `int8` to `int64` and named integer types | exact integer; `time.Duration` stores nanoseconds; `math.MinInt64` and `math.MaxInt64` read back exactly |
| `uint`, `uint8` to `uint64` and named unsigned types | exact integer; `math.MaxUint64` reads back as `UBIGINT` 18446744073709551615 |
| finite `float64` and `float32` | a JSON number, as `encoding/json` writes it: `0.1` stays `0.1`, `1e300` is stored as `1e+300`, and a whole float such as `3.0` becomes `3`. The native JSON reader can change the last digit of a `float64`, so about 3 in 10 arbitrary values read back one step away from the value written; `float32` values and short decimals such as `0.1` are not affected. Store the text of a float when every digit matters |
| slices and arrays of accepted values, such as `[]string`, `[]any` or `[3]int` | JSON array; a nil slice stores `null` |
| maps with string keys and accepted values, such as `map[string]any` or `map[string]int` | JSON object; a nil map stores `null` |

Maps, slices and arrays may be nested at most 64 levels below the attribute map. Anything else returns an error with `CodeInvalidEvent` (`CodeInvalidArgument` for `Options.Resource`) that names the key path, and nothing of that `Write` call is queued:

| Go value | the error says |
|---|---|
| NaN, +Inf, -Inf | only finite numbers can be stored |
| `[]byte` and `[N]byte`, also `net.IP` | encode bytes as text first, for example base64 or hex |
| `time.Time` | store `t.UnixNano()` or `t.Format(time.RFC3339Nano)` |
| other structs | store its fields in a `map[string]any` |
| any pointer, a nil pointer included, such as `*big.Int` | dereference it first; integers wider than 64 bits cannot be stored exactly, so store their text |
| `uintptr`, `complex64`, `complex128`, channels, functions, `unsafe.Pointer` | values of this type cannot be stored |
| a map whose keys are not strings | keys must be strings |
| nesting deeper than 64 levels, also a map that contains itself | nested deeper than 64 levels |

For example: `duckmon: event 2: attrs["latency"][3] is NaN; only finite numbers can be stored`.

## Writing

`Write` never waits for disk or DuckDB. It checks and encodes the events in the calling goroutine, about 1 µs for a small event, and puts them on a queue in Go. One goroutine per store sends the queue to the native writer in batches of up to 10000 events or 8 MiB of text.

When `MaxPendingEvents` events already wait, the events that do not fit are dropped. `Write` then returns an error that wraps `ErrQueueFull` (`errors.Is(err, duckmon.ErrQueueFull)`), after queuing the events that fit. `DroppedEvents` counts the events dropped because the queue was full and the events of a batch the store refused. Events lost because a later flush to disk failed are not counted; `Flush` and `Close` return that error. Events wait in two places: up to `MaxPendingEvents` events in Go, and up to `ChannelCapacity` batches of up to 10000 events each in the native writer.

When the native writer fails, the next `Flush` or `Close` returns the error.

Events reach a Parquet file at the next flush: every `FlushInterval`, at `Flush` or at `Close`. `Query` sees every event written before it, also the ones not flushed yet. `Flush` and `Query` wait only for the events written before them, so they finish while other goroutines keep writing.

## Query results

`Query` returns a `*Result` with `Columns []string` and `Rows [][]any`. Two columns may share a name. Values follow the column type, the same way at the top level and inside lists, structs and maps. NULL is `nil` everywhere.

| DuckDB type | Go value |
|---|---|
| `BOOLEAN` | `bool` |
| `TINYINT`, `SMALLINT`, `INTEGER`, `BIGINT` | `int8`, `int16`, `int32`, `int64` |
| `UTINYINT`, `USMALLINT`, `UINTEGER`, `UBIGINT` | `uint8`, `uint16`, `uint32`, `uint64` |
| `HUGEINT`, `DECIMAL` with scale 0 | `*big.Int` |
| `UHUGEINT` | `*big.Int`, but DuckDB exports it as a signed 128 bit decimal, so values above 2^127 minus 1 arrive wrong (the largest arrives as `-1`); cast to `VARCHAR` |
| `DECIMAL` with a scale above 0 | exact decimal text, such as `"-0.01"` |
| `FLOAT`, `DOUBLE` | `float32`, `float64`, also NaN and infinities |
| `VARCHAR`, `UUID`, `JSON`, `ENUM` | `string`; `attrs` and `resource` are JSON text |
| `BLOB`, `BIT` | `[]byte` copy of DuckDB's bytes; cast `BIT` to `VARCHAR` for its text |
| `DATE` | `time.Time` at 00:00 UTC; years before 1 are exact (year 0 is 1 BC); the infinite date arrives as year 5881580, so test it in SQL |
| `TIMESTAMP_NS` (the `ts` column) | `time.Time` in UTC, nanoseconds exact |
| `TIMESTAMP`, `TIMESTAMP_MS`, `TIMESTAMP_S`, `TIMESTAMPTZ` | `time.Time` in UTC; the infinite timestamp arrives as its largest stored value, such as year 294247 for `TIMESTAMP` |
| `TIME`, `TIMETZ` | `time.Duration` since midnight; DuckDB drops the offset of `TIMETZ`, so cast it to `VARCHAR` |
| `LIST`, fixed size `ARRAY` | `[]any` |
| `STRUCT` | `map[string]any` |
| `MAP` | `[]duckmon.MapEntry` with `Key` and `Value`, in DuckDB's order; keys may be lists |
| `INTERVAL`, `UNION`, `BIGNUM` and other types | `Query` returns an error with `CodeInvalidArgument` that names the column; cast it in SQL, for example `CAST(i AS VARCHAR)` |

`attrs` holds 64 bit integers. To decode it in Go without rounding them to `float64`, use a `json.Decoder` with `UseNumber()`, or read single values in SQL, for example `attrs->>'level'`.

The whole result is held in memory, first as Arrow data and then as Go values, so use `LIMIT` for large results.

While compaction replaces files of a past day, a query can fail with `CodeDatabase` and an IO error. Running it again gives the right answer.

The DuckDB built into duckmon has no ICU, so `now() - INTERVAL 1 HOUR` does not work. Compare `ts` with `make_timestamp_ns(epoch_ns(now()))`:

```sql
SELECT name, duration_ns / 1e6 AS ms FROM events
WHERE duration_ns IS NOT NULL AND ts >= make_timestamp_ns(epoch_ns(now())) - INTERVAL 1 HOUR
ORDER BY duration_ns DESC LIMIT 20;
```

The queries of the Node README (`bindings/node/README.md`) work the same way from Go.

## Errors

Failures are `*duckmon.Error` with a `Code` and a `Message`. The codes are `CodeInvalidArgument`, `CodeInvalidEvent`, `CodeInvalidDirectory`, `CodeDatabase` (for example bad SQL), `CodeFile`, `CodeClosed`, `CodeCompact` and `CodeInternal`. A `Code` is an error itself, so both forms work:

```go
if errors.Is(err, duckmon.CodeClosed) {
	// The store was closed.
}
var e *duckmon.Error
if errors.As(err, &e) {
	fmt.Println(e.Code.String(), e.Message)
}
```

Every call after `Close` returns `CodeClosed`, a second `Close` included. `Open` and `Compact` return `CodeInternal` when the library was built from another ABI version than the package.

## Concurrency and threads

A `Store` is safe for concurrent use. A native call holds its OS thread while it waits, so the store bounds them: only the sender goroutine writes, and `Flush`, `Query` and the end of `Close` take turns, so at most two threads per store wait in native code. Other callers wait as goroutines. DuckDB already runs one query at a time per store, so the turns cost nothing. `Compact` holds one thread per call while it waits for the compaction lock and merges.

There is no `context.Context` parameter. The library cannot stop a query, a flush or a compaction once it started, so a context could only stop the waiting while the work goes on. `Write` never blocks, so it needs no deadline.

## Process exit

Go runs no exit hooks, and duckmon installs no signal handler. Call `Close` on every store you open. A store that is never closed is held until the process exits: the garbage collector never frees it, because its sender goroutine refers to it.

| how the process ends | what runs | what is lost |
|---|---|---|
| `main` returns, or `run()` returns and `os.Exit(run())` follows | the deferred `Close` | nothing |
| `os.Exit`, `log.Fatal`, `flag` errors | no deferred calls | events in the queue in Go, in the native writer, and in its buffer since the last flush: at most `MaxPendingEvents` events plus about one `FlushInterval` |
| a panic in the main goroutine that nobody recovers | the deferred calls of that goroutine, so the deferred `Close`, then the crash | nothing written before the panic |
| a panic in another goroutine that nobody recovers, or a fatal runtime error | no deferred calls of `main` | as for `os.Exit` |
| SIGINT or SIGTERM with no handler, SIGKILL, a crash | nothing | as for `os.Exit` |

Files on disk stay consistent in every case. A writer stopped in the middle of a flush leaves `.tmp` files, which readers ignore and compaction removes.

End the program by returning from a `run` function, and turn SIGINT and SIGTERM into a normal return:

```go
package main

import (
	"context"
	"log"
	"os"
	"os/signal"
	"syscall"

	duckmon "github.com/strrl/duckmon/bindings/go"
)

func main() { os.Exit(run()) }

func run() int {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	store, err := duckmon.Open("telemetry", duckmon.Options{})
	if err != nil {
		log.Print(err)
		return 1
	}
	defer func() {
		if err := store.Close(); err != nil {
			log.Print(err)
		}
	}()
	// Serve until ctx is done; return instead of calling os.Exit or log.Fatal.
	<-ctx.Done()
	return 0
}
```

Recover panics in goroutines you start, or accept that such a crash loses the queue. Using duckmon across `fork()` is not supported; `os/exec` is fine, because the child calls exec right away.

## Compaction

```go
report, err := duckmon.Compact("telemetry", duckmon.CompactOptions{KeepDays: new(7)})
if err != nil {
	return err
}
for _, action := range report.Actions {
	fmt.Println(action.Action, action.Day, action.Path)
}
```

`Compact` merges the small files of past days and, with `KeepDays`, deletes older days. `KeepDays: new(0)` deletes every day, and nil deletes nothing. `SettleTime` (default 1 hour) is how long ago a day must have ended before it is merged. `DryRun: true` only reports what it would do. The `duckmon compact` command line tool does the same.

## Example

`examples/roundtrip` writes events and reads them back, like the Rust and Node examples:

```sh
go run ./examples/roundtrip write /tmp/store 3000 0
go run ./examples/roundtrip count /tmp/store 3000
```

`write <dir> <count> <hold_seconds>` prints `roundtrip ok ...`, then keeps the store open for `hold_seconds`; SIGINT or SIGTERM ends the hold early and still closes the store. Check the result with the command line tool or DuckDB itself:

```sh
../../target/release/duckmon --dir /tmp/store query "SELECT count(*), sum(duration_ns) FILTER (WHERE span_id IS NOT NULL) FROM events"
duckdb -c "SELECT count(*) FROM read_parquet('/tmp/store/date=*/*.parquet', hive_partitioning = true, union_by_name = true, hive_types = {'date': 'DATE'})"
```

## OpenTelemetry

Exporters for the OpenTelemetry Go SDK are planned as the module `github.com/strrl/duckmon/bindings/go/opentelemetry`, so this module stays free of OpenTelemetry dependencies.
