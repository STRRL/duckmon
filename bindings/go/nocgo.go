//go:build !cgo || !(darwin || linux)

package duckmon

// This package calls the duckmon library through cgo, so it builds only with
// CGO_ENABLED=1, a C compiler, and macOS or Linux. This name is undefined on
// purpose, so that the first compiler error names the problem. It is a type,
// because the compiler checks types before everything else.
type _ duckmon_needs_cgo_on_darwin_or_linux
