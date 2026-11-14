//go:build cgo && duckmon_local && (darwin || linux)

package duckmon

// Building with -tags duckmon_local links the library that a local
// `cargo build -p duckmon-ffi --release` wrote to target/release of this
// clone, instead of a prebuilt library module.

/*
#cgo darwin LDFLAGS: ${SRCDIR}/../../target/release/libduckmon_ffi.a -lc++ -liconv -framework CoreFoundation
#cgo linux LDFLAGS: ${SRCDIR}/../../target/release/libduckmon_ffi.a -lstdc++ -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc
*/
import "C"
