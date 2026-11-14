package duckmon

import (
	"encoding/json"
	"fmt"
	"math"
	"time"
)

// CompactOptions configures Compact. The zero value merges and deletes
// nothing old.
type CompactOptions struct {
	// KeepDays keeps the last N UTC days, today included, and deletes older
	// days. Nil deletes nothing; new(0) deletes every day.
	KeepDays *int
	// SettleTime: a day is merged only once it ended at least this long ago,
	// and a file only once it is this old. Nil means 1 hour.
	SettleTime *time.Duration
	// DryRun reports the actions without changing anything and takes no lock.
	DryRun bool
}

// CompactReport lists what Compact did, in order.
type CompactReport struct {
	Actions []CompactAction `json:"actions"`
}

// CompactAction is one step of Compact. Action is one of remove_merged_input,
// remove_deleted_day, delete_day, remove_temp_file, merge_files and skip_day;
// only the fields of that action are set.
type CompactAction struct {
	Action     string `json:"action"`
	Path       string `json:"path"`
	MergedInto string `json:"merged_into"`
	Day        string `json:"day"`
	Files      int64  `json:"files"`
	Bytes      int64  `json:"bytes"`
	Inputs     int64  `json:"inputs"`
	InputBytes int64  `json:"input_bytes"`
	Rows       int64  `json:"rows"`
	Output     string `json:"output"`
	Reason     string `json:"reason"`
}

// Compact merges the small files of past days in dir and, with KeepDays,
// deletes old days. It blocks while it waits for the compaction lock and
// merges files. The duckmon compact command does the same.
func Compact(dir string, options CompactOptions) (*CompactReport, error) {
	if err := checkABIVersion(); err != nil {
		return nil, err
	}
	optionsJSON, err := compactOptionsJSON(options)
	if err != nil {
		return nil, err
	}
	reportJSON, err := nativeCompact(dir, optionsJSON)
	if err != nil {
		return nil, err
	}
	return compactReportFromJSON(reportJSON)
}

// compactOptionsFields is the options JSON of duckmon_compact. Unset keys
// are left out, so the library uses its defaults.
type compactOptionsFields struct {
	KeepDays     *int   `json:"keep_days,omitempty"`
	SettleTimeMS *int64 `json:"settle_time_ms,omitempty"`
	DryRun       bool   `json:"dry_run,omitempty"`
}

// compactOptionsJSON checks options and returns the options JSON of
// duckmon_compact.
func compactOptionsJSON(options CompactOptions) (string, error) {
	fields := compactOptionsFields{KeepDays: options.KeepDays, DryRun: options.DryRun}
	if options.KeepDays != nil && (*options.KeepDays < 0 || int64(*options.KeepDays) > math.MaxUint32) {
		return "", &Error{Code: CodeInvalidArgument, Message: fmt.Sprintf("options: KeepDays must be from 0 to %d, got %d", uint32(math.MaxUint32), *options.KeepDays)}
	}
	if options.SettleTime != nil {
		if *options.SettleTime < 0 {
			return "", &Error{Code: CodeInvalidArgument, Message: fmt.Sprintf("options: SettleTime must not be negative, got %v", *options.SettleTime)}
		}
		fields.SettleTimeMS = new(int64(*options.SettleTime / time.Millisecond))
	}
	text, err := json.Marshal(fields)
	if err != nil {
		return "", &Error{Code: CodeInvalidArgument, Message: "options: " + err.Error()}
	}
	return string(text), nil
}

// compactReportFromJSON reads the report JSON of duckmon_compact.
func compactReportFromJSON(reportJSON []byte) (*CompactReport, error) {
	report := &CompactReport{}
	if err := json.Unmarshal(reportJSON, report); err != nil {
		return nil, &Error{Code: CodeInternal, Message: "cannot read the compact report: " + err.Error()}
	}
	return report, nil
}
