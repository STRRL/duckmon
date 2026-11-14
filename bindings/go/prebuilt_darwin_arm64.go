//go:build cgo && darwin && arm64 && !duckmon_local

package duckmon

// The imported module carries the prebuilt static library and the link
// flags for this platform.
import _ "github.com/STRRL/duckmon/bindings/lib/darwin-arm64"
