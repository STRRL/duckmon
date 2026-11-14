//go:build cgo && linux && arm64

// Package lib carries the prebuilt duckmon static library for linux/arm64.
// It holds no Go code. Importing it links the library and the system
// libraries it needs into the final binary. The library is split into
// parts so that every file stays under the GitHub file size limit, and
// one group links the parts in any order.
package lib

/*
#cgo LDFLAGS: -Wl,--start-group ${SRCDIR}/libduckmon_ffi_1.a ${SRCDIR}/libduckmon_ffi_2.a ${SRCDIR}/libduckmon_ffi_3.a -Wl,--end-group -lstdc++ -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc
*/
import "C"
