//go:build cgo && linux && arm64 && !duckmon_local

package duckmon

// The imported module carries the prebuilt static library and the link
// flags for this platform.
import _ "github.com/STRRL/duckmon/bindings/lib/linux-arm64"
