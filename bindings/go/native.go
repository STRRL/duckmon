//go:build cgo && (darwin || linux)

package duckmon

/*
#cgo CFLAGS: -I${SRCDIR}/include
#include <stdlib.h>
#include <string.h>
#include "duckmon.h"

// A malloc'd copy of this thread's last error message, or NULL after success.
// It is taken in the same C call as the failing function, because a goroutine
// can move to another OS thread between two cgo calls. Go frees it.
static char *duckmon_go_message(int32_t code) {
	if (code == DUCKMON_OK) {
		return NULL;
	}
	return strdup(duckmon_last_error());
}

static int32_t duckmon_go_open(const char *dir, const char *options_json, duckmon_store **out_store, char **message) {
	int32_t code = duckmon_open(dir, options_json, out_store);
	*message = duckmon_go_message(code);
	return code;
}

static int32_t duckmon_go_write_events(duckmon_store *store, const char *lines, char **message) {
	int32_t code = duckmon_store_write_events(store, lines);
	*message = duckmon_go_message(code);
	return code;
}

static int32_t duckmon_go_flush(duckmon_store *store, char **message) {
	int32_t code = duckmon_store_flush(store);
	*message = duckmon_go_message(code);
	return code;
}

static int32_t duckmon_go_query(duckmon_store *store, const char *sql, duckmon_bytes *out, char **message) {
	int32_t code = duckmon_store_query(store, sql, out);
	*message = duckmon_go_message(code);
	return code;
}

static int32_t duckmon_go_close(duckmon_store *store, char **message) {
	int32_t code = duckmon_store_close(store);
	*message = duckmon_go_message(code);
	return code;
}

static int32_t duckmon_go_compact(const char *dir, const char *options_json, duckmon_bytes *out, char **message) {
	int32_t code = duckmon_compact(dir, options_json, out);
	*message = duckmon_go_message(code);
	return code;
}
*/
import "C"

import (
	"bytes"
	"strings"
	"unsafe"
)

// nativeStore holds an open duckmon_store handle, so that C types stay in
// this file.
type nativeStore struct {
	handle *C.duckmon_store
}

// nativeError turns a code and the message copied by a duckmon_go_* helper
// into an error, and frees the message. It returns nil for DUCKMON_OK.
func nativeError(code C.int32_t, message *C.char) error {
	if code == C.DUCKMON_OK {
		return nil
	}
	text := "no message"
	if message != nil {
		text = C.GoString(message)
		C.free(unsafe.Pointer(message))
	}
	return &Error{Code: Code(code), Message: text}
}

// cText copies text to the C heap. The caller frees the copy with C.free.
// C reads text up to the first NUL character, so a text that holds one is
// refused instead of being cut short.
func cText(text, what string) (*C.char, error) {
	if strings.IndexByte(text, 0) >= 0 {
		return nil, &Error{Code: CodeInvalidArgument, Message: what + " holds a NUL character"}
	}
	return C.CString(text), nil
}

// takeBytes copies bytes that duckmon handed out into Go memory and frees them.
func takeBytes(b *C.duckmon_bytes) []byte {
	data := bytes.Clone(unsafe.Slice((*byte)(unsafe.Pointer(b.data)), int(b.len)))
	C.duckmon_bytes_free(b)
	return data
}

func nativeABIVersion() uint32 {
	return uint32(C.duckmon_abi_version())
}

func nativeOpen(dir, optionsJSON string) (*nativeStore, error) {
	cDir, err := cText(dir, "dir")
	if err != nil {
		return nil, err
	}
	defer C.free(unsafe.Pointer(cDir))
	cOptions, err := cText(optionsJSON, "options")
	if err != nil {
		return nil, err
	}
	defer C.free(unsafe.Pointer(cOptions))
	var handle *C.duckmon_store
	var message *C.char
	code := C.duckmon_go_open(cDir, cOptions, &handle, &message)
	if err := nativeError(code, message); err != nil {
		return nil, err
	}
	return &nativeStore{handle: handle}, nil
}

func (s *nativeStore) writeEvents(lines string) error {
	cLines, err := cText(lines, "events")
	if err != nil {
		return err
	}
	defer C.free(unsafe.Pointer(cLines))
	var message *C.char
	code := C.duckmon_go_write_events(s.handle, cLines, &message)
	return nativeError(code, message)
}

func (s *nativeStore) flush() error {
	var message *C.char
	code := C.duckmon_go_flush(s.handle, &message)
	return nativeError(code, message)
}

func (s *nativeStore) query(sql string) ([]byte, error) {
	cSQL, err := cText(sql, "sql")
	if err != nil {
		return nil, err
	}
	defer C.free(unsafe.Pointer(cSQL))
	var result C.duckmon_bytes
	var message *C.char
	code := C.duckmon_go_query(s.handle, cSQL, &result, &message)
	if err := nativeError(code, message); err != nil {
		return nil, err
	}
	return takeBytes(&result), nil
}

func (s *nativeStore) close() error {
	var message *C.char
	code := C.duckmon_go_close(s.handle, &message)
	return nativeError(code, message)
}

// free releases the handle. No other call may use it at the same time or
// afterwards.
func (s *nativeStore) free() {
	C.duckmon_store_free(s.handle)
	s.handle = nil
}

func nativeCompact(dir, optionsJSON string) ([]byte, error) {
	cDir, err := cText(dir, "dir")
	if err != nil {
		return nil, err
	}
	defer C.free(unsafe.Pointer(cDir))
	cOptions, err := cText(optionsJSON, "options")
	if err != nil {
		return nil, err
	}
	defer C.free(unsafe.Pointer(cOptions))
	var report C.duckmon_bytes
	var message *C.char
	code := C.duckmon_go_compact(cDir, cOptions, &report, &message)
	if err := nativeError(code, message); err != nil {
		return nil, err
	}
	return takeBytes(&report), nil
}
