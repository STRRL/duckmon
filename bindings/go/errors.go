package duckmon

import (
	"errors"
	"strconv"
)

// Code is a DUCKMON_ERROR_* code of duckmon.h. It is an error itself, so
// errors.Is(err, duckmon.CodeClosed) reports whether err carries that code.
type Code int32

const (
	// CodeInvalidArgument means bad options, a NUL character in a text, or a
	// query column type with no exact Go value.
	CodeInvalidArgument Code = 1
	// CodeInvalidEvent means a field or attribute of an event cannot be stored.
	CodeInvalidEvent Code = 2
	// CodeInvalidDirectory means the store directory path cannot be used.
	CodeInvalidDirectory Code = 3
	// CodeDatabase means DuckDB reported an error, for example for bad SQL.
	CodeDatabase Code = 4
	// CodeFile means a file or directory operation failed.
	CodeFile Code = 5
	// CodeClosed means the store is closed or its writer has stopped.
	CodeClosed Code = 6
	// CodeCompact means a merged file did not hold the same rows as its inputs.
	CodeCompact Code = 7
	// CodeInternal means duckmon failed in a way it did not expect, or the
	// native library has a different ABI version than this package.
	CodeInternal Code = 8
)

var codeNames = map[Code]string{
	CodeInvalidArgument:  "INVALID_ARGUMENT",
	CodeInvalidEvent:     "INVALID_EVENT",
	CodeInvalidDirectory: "INVALID_DIRECTORY",
	CodeDatabase:         "DATABASE",
	CodeFile:             "FILE",
	CodeClosed:           "CLOSED",
	CodeCompact:          "COMPACT",
	CodeInternal:         "INTERNAL",
}

// String returns the name of the code, such as "CLOSED", or "CODE(9)" for a
// code this package does not know.
func (c Code) String() string {
	if name, ok := codeNames[c]; ok {
		return name
	}
	return "CODE(" + strconv.Itoa(int(c)) + ")"
}

// Error returns "duckmon: " followed by the name of the code.
func (c Code) Error() string {
	return "duckmon: " + c.String()
}

// Error is a failure reported by duckmon, or found by this package before
// calling it.
type Error struct {
	Code    Code
	Message string
}

// Error returns "duckmon: " followed by Message.
func (e *Error) Error() string {
	return "duckmon: " + e.Message
}

// Unwrap returns e.Code, so errors.Is matches the code.
func (e *Error) Unwrap() error {
	return e.Code
}

// ErrQueueFull is wrapped by the error Write returns when it dropped events
// because MaxPendingEvents events were already waiting.
var ErrQueueFull = errors.New("duckmon: queue is full")

// closedError is the error of every call after Close, with the message the
// native library uses for the same case.
func closedError() error {
	return &Error{Code: CodeClosed, Message: "store is closed"}
}
