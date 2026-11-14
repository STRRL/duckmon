//go:build cgo && darwin && arm64

// Package lib carries the prebuilt duckmon static library for darwin/arm64.
// It holds no Go code. Importing it links the library and the system
// libraries it needs into the final binary. The library is split into
// parts so that every file stays under the GitHub file size limit. The
// macOS linker resolves symbols across all of them without a group.
package lib

/*
#cgo LDFLAGS: ${SRCDIR}/libduckmon_ffi_1.a ${SRCDIR}/libduckmon_ffi_2.a ${SRCDIR}/libduckmon_ffi_3.a -lc++ -liconv -framework CoreFoundation
*/
import "C"
