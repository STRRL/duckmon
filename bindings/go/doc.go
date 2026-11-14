// Package duckmon is an embedded telemetry store. An application writes logs,
// spans and metric snapshots as events. They land as Parquet files in a
// directory, and SQL run by an in process DuckDB reads them. Many processes
// may share one directory. The file format is described in docs/contract.md
// of the duckmon repository.
//
// The package calls the duckmon C ABI through cgo and links the static
// library libduckmon_ffi.a. By default it links a prebuilt library that a
// platform module of this repository carries, so a plain go get is enough.
// With the build tag duckmon_local it links the library built by
//
//	cargo build -p duckmon-ffi --release
//
// from the root of a duckmon clone. The README of this directory describes
// the install, the value types and the exit behavior.
package duckmon
