// Command roundtrip writes events and reads them back, like
// crates/duckmon/examples/roundtrip.rs and bindings/node/examples/roundtrip.ts.
//
//	go run ./examples/roundtrip write <dir> <count> <hold_seconds>
//	go run ./examples/roundtrip count <dir> <expected>
//
// Run cargo build -p duckmon-ffi --release from the repository root first.
package main

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"

	duckmon "github.com/strrl/duckmon/bindings/go"
)

func main() {
	os.Exit(run(os.Args[1:]))
}

func run(args []string) int {
	switch {
	case len(args) == 4 && args[0] == "write":
		count, countErr := strconv.ParseUint(args[2], 10, 31)
		hold, holdErr := strconv.ParseUint(args[3], 10, 31)
		if countErr == nil && holdErr == nil {
			return exitCode(write(args[1], int(count), time.Duration(hold)*time.Second))
		}
	case len(args) == 3 && args[0] == "count":
		expected, err := strconv.ParseInt(args[2], 10, 64)
		if err == nil {
			return exitCode(count(args[1], expected))
		}
	}
	fmt.Fprintln(os.Stderr, "usage: roundtrip write <dir> <count> <hold_seconds>")
	fmt.Fprintln(os.Stderr, "       roundtrip count <dir> <expected>")
	return 2
}

func exitCode(ok bool, err error) int {
	if err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		return 1
	}
	if !ok {
		return 1
	}
	return 0
}

func hexID(size int) string {
	id := make([]byte, size)
	rand.Read(id)
	return hex.EncodeToString(id)
}

func write(dir string, count int, hold time.Duration) (ok bool, err error) {
	store, err := duckmon.Open(dir, duckmon.Options{
		FlushInterval: time.Second,
		Resource:      map[string]any{"service.name": "roundtrip"},
	})
	if err != nil {
		return false, err
	}
	defer func() {
		if closeErr := store.Close(); err == nil {
			err = closeErr
		}
	}()

	runID := hexID(16)
	now := time.Now()
	events := make([]duckmon.Event, 0, count)
	var expectedSpanNs, expectedMetricSum int64
	for i := range count {
		ts := now.Add(-time.Duration(i) * 20 * time.Second)
		switch i % 3 {
		case 0:
			events = append(events, duckmon.Event{
				Name:    "app.http",
				Time:    ts,
				Message: new(fmt.Sprintf("handled request %d", i)),
				Attrs:   map[string]any{"level": "info", "run": runID},
			})
		case 1:
			duration := time.Duration(i) * 1000
			events = append(events, duckmon.Event{
				Name:     "http.request",
				Time:     ts,
				Duration: new(duration),
				TraceID:  hexID(16),
				SpanID:   fmt.Sprintf("%016x", i+1),
				Attrs:    map[string]any{"run": runID},
			})
			expectedSpanNs += int64(duration)
		default:
			events = append(events, duckmon.Event{
				Name:  "process.stats",
				Time:  ts,
				Attrs: map[string]any{"value": i, "cpu.percent": 0.5, "run": runID},
			})
			expectedMetricSum += int64(i)
		}
	}

	half := len(events) / 2
	for start := 0; start < half; start += 100 {
		if err := store.Write(events[start:min(start+100, half)]...); err != nil {
			return false, err
		}
	}
	if err := store.Flush(); err != nil {
		return false, err
	}
	for start := half; start < len(events); start += 100 {
		if err := store.Write(events[start:min(start+100, len(events))]...); err != nil {
			return false, err
		}
	}

	result, err := store.Query(`SELECT count(*) AS rows,
		coalesce(sum(duration_ns) FILTER (WHERE span_id IS NOT NULL), 0)::BIGINT AS span_ns,
		coalesce(sum((attrs->>'value')::BIGINT) FILTER (WHERE name = 'process.stats'), 0)::BIGINT AS metric_sum,
		count(*) FILTER (WHERE date <> CAST(strftime(ts, '%Y-%m-%d') AS DATE)) AS wrong_date
		FROM events WHERE attrs->>'run' = '` + runID + `'`)
	if err != nil {
		return false, err
	}
	buffer, err := store.Query("SELECT count(*) AS rows FROM unflushed_events")
	if err != nil {
		return false, err
	}

	actual := result.Rows[0]
	expected := []any{int64(count), expectedSpanNs, expectedMetricSum, int64(0)}
	for index := range expected {
		if actual[index] != expected[index] {
			fmt.Printf("roundtrip mismatch run=%s\n", runID)
			fmt.Printf("expected rows, span_ns, metric_sum, wrong_date = %v, %v, %v, %v\n", expected...)
			fmt.Printf("actual   rows, span_ns, metric_sum, wrong_date = %v, %v, %v, %v\n", actual...)
			return false, nil
		}
	}
	fmt.Printf("roundtrip ok run=%s rows=%d span_ns=%d metric_sum=%d\n", runID, actual[0], actual[1], actual[2])
	fmt.Printf("buffer rows=%d\n", buffer.Rows[0][0])

	// A signal ends the hold early, and the deferred Close still runs.
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	select {
	case <-ctx.Done():
	case <-time.After(hold):
	}
	return true, nil
}

func count(dir string, expected int64) (ok bool, err error) {
	store, err := duckmon.Open(dir, duckmon.Options{})
	if err != nil {
		return false, err
	}
	defer func() {
		if closeErr := store.Close(); err == nil {
			err = closeErr
		}
	}()
	result, err := store.Query("SELECT count(*) AS rows FROM events")
	if err != nil {
		return false, err
	}
	rows := result.Rows[0][0].(int64)
	fmt.Println(rows)
	return rows == expected, nil
}
