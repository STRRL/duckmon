package duckmon

import (
	"encoding/json"
	"fmt"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// abiVersion is the DUCKMON_ABI_VERSION this package was written for.
const abiVersion = 1

const (
	defaultMaxPendingEvents = 100_000
	maxChannelCapacity      = 1_000_000
	// maxLinesPerSend is the most event lines sent in one native write.
	maxLinesPerSend = 10_000
	// maxBytesPerSend is the most text sent in one native write. Large events
	// fill a send before the line limit does.
	maxBytesPerSend = 8 << 20
)

// Options configures Open. The zero value gives the defaults.
type Options struct {
	// FlushInterval is how often buffered events are written to a new Parquet
	// file, in whole milliseconds. Zero means 5 seconds.
	FlushInterval time.Duration
	// ChannelCapacity is how many batches may wait for the native writer
	// thread, from 1 to 1000000. Zero means 1024.
	ChannelCapacity int
	// MaxPendingEvents is how many events Write keeps in Go while the native
	// writer is busy. Later events are dropped. Zero means 100000.
	MaxPendingEvents int
	// Resource is stored in the resource column of every event that sets no
	// Resource of its own, such as {"service.name": "api"}. Values follow the
	// attribute rules of Event.Attrs.
	Resource map[string]any
}

// Store is a telemetry store bound to one directory. Its methods are safe
// for concurrent use.
//
// Write only queues events. One goroutine per store sends the queue to the
// native writer in batches. Flush and Query first wait until every event
// queued before them is sent; Close waits for all of them.
type Store struct {
	maxPendingEvents int

	// mu guards the fields below it, up to dropped.
	mu sync.Mutex
	// sent uses mu as its lock. It is signaled when finished grows.
	sent sync.Cond
	// pending holds encoded event lines waiting for the sender.
	pending []string
	// queued counts the events ever put in pending.
	queued uint64
	// finished counts the events whose native write returned.
	finished uint64
	closed   bool
	// writeErr is the first native write failure since the last Flush or Close.
	writeErr error

	dropped atomic.Uint64
	// wake has room for one signal. It tells the sender there is work or that
	// Close began.
	wake chan struct{}
	// done is closed when the sender goroutine has ended.
	done chan struct{}

	// nativeMu serializes Flush, Query and the final close, so that at most
	// one of them holds an OS thread in native code. It guards native, which
	// is nil after Close.
	nativeMu sync.Mutex
	native   *nativeStore
}

// Open opens the store in dir and creates dir when it is missing. It blocks
// while DuckDB starts. Close the store when done.
func Open(dir string, options Options) (*Store, error) {
	if err := checkABIVersion(); err != nil {
		return nil, err
	}
	optionsJSON, maxPendingEvents, err := openOptionsJSON(options)
	if err != nil {
		return nil, err
	}
	native, err := nativeOpen(dir, optionsJSON)
	if err != nil {
		return nil, err
	}
	s := &Store{
		maxPendingEvents: maxPendingEvents,
		wake:             make(chan struct{}, 1),
		done:             make(chan struct{}),
		native:           native,
	}
	s.sent.L = &s.mu
	go s.sendPending(native)
	return s, nil
}

func checkABIVersion() error {
	if version := nativeABIVersion(); version != abiVersion {
		return &Error{
			Code:    CodeInternal,
			Message: fmt.Sprintf("the duckmon library has ABI version %d, but this package needs version %d; build the library and this package from the same commit", version, abiVersion),
		}
	}
	return nil
}

// openOptionsFields is the options JSON of duckmon_open. Unset keys are left
// out, so the library uses its defaults.
type openOptionsFields struct {
	FlushIntervalMS int64           `json:"flush_interval_ms,omitempty"`
	ChannelCapacity int             `json:"channel_capacity,omitempty"`
	Resource        json.RawMessage `json:"resource,omitempty"`
}

// openOptionsJSON checks options and returns the options JSON of
// duckmon_open, and the MaxPendingEvents to use.
func openOptionsJSON(options Options) (string, int, error) {
	invalid := func(format string, args ...any) (string, int, error) {
		return "", 0, &Error{Code: CodeInvalidArgument, Message: "options: " + fmt.Sprintf(format, args...)}
	}
	if options.FlushInterval < 0 || (options.FlushInterval > 0 && options.FlushInterval < time.Millisecond) {
		return invalid("FlushInterval must be zero or at least 1 millisecond, got %v", options.FlushInterval)
	}
	if options.ChannelCapacity < 0 || options.ChannelCapacity > maxChannelCapacity {
		return invalid("ChannelCapacity must be from 0 to %d, got %d", maxChannelCapacity, options.ChannelCapacity)
	}
	if options.MaxPendingEvents < 0 {
		return invalid("MaxPendingEvents must not be negative, got %d", options.MaxPendingEvents)
	}
	fields := openOptionsFields{
		FlushIntervalMS: int64(options.FlushInterval / time.Millisecond),
		ChannelCapacity: options.ChannelCapacity,
	}
	if options.Resource != nil {
		resource, err := appendAttributes(nil, options.Resource, "Resource")
		if err != nil {
			return invalid("%v", err)
		}
		fields.Resource = resource
	}
	text, err := json.Marshal(fields)
	if err != nil {
		return invalid("%v", err)
	}
	maxPendingEvents := options.MaxPendingEvents
	if maxPendingEvents == 0 {
		maxPendingEvents = defaultMaxPendingEvents
	}
	return string(text), maxPendingEvents, nil
}

// Write checks the events and queues them for the store's writer goroutine.
// It never waits for disk or DuckDB. It returns an *Error with
// CodeInvalidEvent when a field or attribute cannot be stored, and then
// queues none of the events; CodeClosed after Close; and an error wrapping
// ErrQueueFull when MaxPendingEvents events already wait, after queuing the
// events that fit. Events reach a Parquet file at the next flush.
func (s *Store) Write(events ...Event) error {
	now := time.Now()
	lines := make([]string, len(events))
	var line []byte
	for index := range events {
		var err error
		line, err = appendEventLine(line[:0], &events[index], now)
		if err != nil {
			return &Error{Code: CodeInvalidEvent, Message: fmt.Sprintf("event %d: %v", index+1, err)}
		}
		lines[index] = string(line)
	}

	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return closedError()
	}
	fitting := min(max(s.maxPendingEvents-len(s.pending), 0), len(lines))
	s.pending = append(s.pending, lines[:fitting]...)
	s.queued += uint64(fitting)
	s.mu.Unlock()

	if fitting > 0 {
		s.wakeSender()
	}
	if dropped := len(lines) - fitting; dropped > 0 {
		s.dropped.Add(uint64(dropped))
		return fmt.Errorf("%w: dropped %d of %d events", ErrQueueFull, dropped, len(lines))
	}
	return nil
}

// DroppedEvents returns how many events were dropped because the queue was
// full or because the store refused their batch.
func (s *Store) DroppedEvents() uint64 {
	return s.dropped.Load()
}

// Flush waits until every event written before this call is in a finished
// Parquet file. It also returns a failure of the writer since the last Flush.
func (s *Store) Flush() error {
	if err := s.waitForSender(); err != nil {
		return err
	}
	// The write error is taken before the native flush, so a Close that runs
	// during this Flush still reports failures that happen from here on.
	writeErr := s.takeWriteError()
	s.nativeMu.Lock()
	flushErr := closedError()
	if s.native != nil {
		flushErr = s.native.flush()
	}
	s.nativeMu.Unlock()
	if writeErr != nil {
		return writeErr
	}
	return flushErr
}

// Query runs SQL against the view events and returns the result of the last
// statement. It sees every event written before this call, also events not
// flushed yet. While compaction replaces files of a past day, a query can fail
// with CodeDatabase and an IO error; running it again gives the right answer.
// The whole result is held in memory, so use LIMIT for large results.
func (s *Store) Query(sql string) (*Result, error) {
	if err := s.waitForSender(); err != nil {
		return nil, err
	}
	s.nativeMu.Lock()
	var arrowIPC []byte
	queryErr := closedError()
	if s.native != nil {
		arrowIPC, queryErr = s.native.query(sql)
	}
	s.nativeMu.Unlock()
	if queryErr != nil {
		return nil, queryErr
	}
	return resultFromArrowIPC(arrowIPC)
}

// Close writes every queued event, flushes and stops the store. A second
// call returns an error with CodeClosed.
func (s *Store) Close() error {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return closedError()
	}
	s.closed = true
	s.mu.Unlock()
	s.wakeSender()
	<-s.done

	s.nativeMu.Lock()
	closeErr := s.native.close()
	s.native.free()
	s.native = nil
	s.nativeMu.Unlock()
	if writeErr := s.takeWriteError(); writeErr != nil {
		return writeErr
	}
	return closeErr
}

// waitForSender waits until the sender has handled every event queued
// before the call. It returns CodeClosed when Close began.
func (s *Store) waitForSender() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return closedError()
	}
	target := s.queued
	for s.finished < target {
		s.sent.Wait()
	}
	return nil
}

func (s *Store) takeWriteError() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	err := s.writeErr
	s.writeErr = nil
	return err
}

func (s *Store) wakeSender() {
	select {
	case s.wake <- struct{}{}:
	default:
	}
}

// sendPending is the sender goroutine. It is the only caller of the native
// write, so at most one OS thread per store waits in it. It ends once Close
// began and the queue is empty.
func (s *Store) sendPending(native *nativeStore) {
	for {
		s.mu.Lock()
		if len(s.pending) == 0 {
			closed := s.closed
			s.mu.Unlock()
			if closed {
				close(s.done)
				return
			}
			<-s.wake
			continue
		}
		batch := s.takeLinesForOneSend()
		s.mu.Unlock()

		err := native.writeEvents(strings.Join(batch, "\n"))
		// Nothing else uses these slots, so drop the lines now instead of
		// when the queue's array is replaced.
		clear(batch)

		s.mu.Lock()
		s.finished += uint64(len(batch))
		if err != nil {
			if s.writeErr == nil {
				s.writeErr = err
			}
			s.dropped.Add(uint64(len(batch)))
		}
		s.sent.Broadcast()
		s.mu.Unlock()
	}
}

// takeLinesForOneSend takes the next lines from the queue, at least one,
// within the line and byte limits of one send. mu must be held.
func (s *Store) takeLinesForOneSend() []string {
	count := 0
	size := 0
	for count < len(s.pending) && count < maxLinesPerSend {
		size += len(s.pending[count]) + 1
		count++
		if size >= maxBytesPerSend {
			break
		}
	}
	batch := s.pending[:count:count]
	s.pending = s.pending[count:]
	if len(s.pending) == 0 {
		s.pending = nil
	}
	return batch
}
