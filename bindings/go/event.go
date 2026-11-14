package duckmon

import (
	"encoding/json"
	"errors"
	"math"
	"reflect"
	"strconv"
	"time"
)

// Event is one telemetry event: a log, a span, a metric snapshot or a plain
// event. docs/contract.md describes how each kind is stored.
type Event struct {
	// Name is a stable, low cardinality name in lowercase dotted words, such
	// as "http.request". Required.
	Name string
	// Time is when the event happened, stored in the ts column with
	// nanosecond precision. For a span, the start time. The zero value means
	// time.Now() at Write. It must be from 1970 to 2262-04-11T23:47:16.854775807Z.
	Time time.Time
	// Message is the log text. Set it only for logs; nil stores NULL.
	Message *string
	// Duration is the span length, stored as duration_ns. Set it only for
	// spans; nil stores NULL. It must not be negative.
	Duration *time.Duration
	// TraceID is 32 lowercase hex characters. Empty stores NULL.
	TraceID string
	// SpanID is 16 lowercase hex characters. Empty stores NULL.
	SpanID string
	// ParentID is the parent span ID of a span. Empty stores NULL, as for a
	// root span.
	ParentID string
	// Attrs are the event attributes. Nil or empty stores NULL. The README
	// lists the values that can be stored.
	Attrs map[string]any
	// Resource replaces the store's Resource for this event when not nil. An
	// empty, non nil map stores NULL.
	Resource map[string]any
}

// maxAttributeDepth is the deepest nesting of maps, slices and arrays inside
// attributes, counting the attribute map itself as level 0. The native JSON
// reader accepts 128 levels for a whole line.
const maxAttributeDepth = 64

var (
	earliestTime = time.Unix(0, 0)
	latestTime   = time.Unix(0, math.MaxInt64)
)

// appendEventLine appends one line of the JSON Lines text that the C ABI
// reads. A zero Time becomes now. The error names the field or the key path
// of the first value that cannot be stored.
func appendEventLine(dst []byte, event *Event, now time.Time) ([]byte, error) {
	if event.Name == "" {
		return dst, errors.New("name must not be empty")
	}
	ts := event.Time
	if ts.IsZero() {
		ts = now
	}
	if ts.Before(earliestTime) || ts.After(latestTime) {
		return dst, errors.New("time must be from 1970 to 2262-04-11T23:47:16.854775807Z, got " + ts.Format(time.RFC3339Nano))
	}
	dst = append(dst, `{"ts":`...)
	dst = strconv.AppendInt(dst, ts.UnixNano(), 10)
	dst = append(dst, `,"name":`...)
	dst = appendString(dst, event.Name)
	if event.Message != nil {
		dst = append(dst, `,"message":`...)
		dst = appendString(dst, *event.Message)
	}
	if event.Duration != nil {
		if *event.Duration < 0 {
			return dst, errors.New("duration must not be negative, got " + event.Duration.String())
		}
		dst = append(dst, `,"duration_ns":`...)
		dst = strconv.AppendInt(dst, int64(*event.Duration), 10)
	}
	dst = appendOptionalString(dst, "trace_id", event.TraceID)
	dst = appendOptionalString(dst, "span_id", event.SpanID)
	dst = appendOptionalString(dst, "parent_id", event.ParentID)
	if len(event.Attrs) > 0 {
		dst = append(dst, `,"attrs":`...)
		var err error
		dst, err = appendAttributes(dst, event.Attrs, "attrs")
		if err != nil {
			return dst, err
		}
	}
	if event.Resource != nil {
		dst = append(dst, `,"resource":`...)
		var err error
		dst, err = appendAttributes(dst, event.Resource, "resource")
		if err != nil {
			return dst, err
		}
	}
	return append(dst, '}'), nil
}

func appendOptionalString(dst []byte, key, value string) []byte {
	if value == "" {
		return dst
	}
	dst = append(dst, ',', '"')
	dst = append(dst, key...)
	dst = append(dst, '"', ':')
	return appendString(dst, value)
}

// appendString appends text as a JSON string. encoding/json replaces invalid
// UTF 8 with U+FFFD and writes NUL as \u0000, so the C ABI always gets valid
// text without NUL characters.
func appendString(dst []byte, text string) []byte {
	// Marshaling a string never fails.
	encoded, _ := json.Marshal(text)
	return append(dst, encoded...)
}

// appendFloat appends a finite float as encoding/json writes it, with the
// shortest text that reads back as the same float32 or float64.
func appendFloat(dst []byte, value any) []byte {
	// Marshaling a finite float never fails.
	encoded, _ := json.Marshal(value)
	return append(dst, encoded...)
}

// attributeError is an attribute value that cannot be stored. path is the key
// path below the attribute map, built from the inside out while the walk
// returns, so that a value that can be stored costs no path text.
type attributeError struct {
	path   string
	reason string
}

func (e *attributeError) Error() string {
	return e.path + " " + e.reason
}

// withPathStep returns err with one more key path step in front, when err is an
// attributeError.
func withPathStep(step string, err error) error {
	if refused, ok := err.(*attributeError); ok {
		refused.path = step + refused.path
	}
	return err
}

// appendAttributes appends an attribute map as a JSON object. name is the
// start of the key path in errors, such as "attrs".
func appendAttributes(dst []byte, attributes map[string]any, name string) ([]byte, error) {
	dst, err := appendStringMap(dst, attributes, 0)
	if err != nil {
		return dst, withPathStep(name, err)
	}
	return dst, nil
}

func keyStep(key string) string {
	encoded, _ := json.Marshal(key)
	return "[" + string(encoded) + "]"
}

func indexStep(index int) string {
	return "[" + strconv.Itoa(index) + "]"
}

func appendStringMap(dst []byte, values map[string]any, depth int) ([]byte, error) {
	if depth >= maxAttributeDepth {
		return dst, tooDeep()
	}
	dst = append(dst, '{')
	first := true
	for key, item := range values {
		if !first {
			dst = append(dst, ',')
		}
		first = false
		dst = appendString(dst, key)
		dst = append(dst, ':')
		var err error
		dst, err = appendValue(dst, item, depth+1)
		if err != nil {
			return dst, withPathStep(keyStep(key), err)
		}
	}
	return append(dst, '}'), nil
}

func tooDeep() error {
	return &attributeError{reason: "is nested deeper than " + strconv.Itoa(maxAttributeDepth) + " levels"}
}

// appendValue appends one attribute value as JSON. Values are checked by Go
// kind; methods such as String, MarshalJSON or MarshalText are never called.
// depth is the nesting level of value.
func appendValue(dst []byte, value any, depth int) ([]byte, error) {
	switch v := value.(type) {
	case nil:
		return append(dst, "null"...), nil
	case string:
		return appendString(dst, v), nil
	case bool:
		return strconv.AppendBool(dst, v), nil
	case int:
		return strconv.AppendInt(dst, int64(v), 10), nil
	case int64:
		return strconv.AppendInt(dst, v, 10), nil
	case uint64:
		return strconv.AppendUint(dst, v, 10), nil
	case float64:
		return appendCheckedFloat(dst, v, v)
	case map[string]any:
		if v == nil {
			return append(dst, "null"...), nil
		}
		return appendStringMap(dst, v, depth)
	case []any:
		if v == nil {
			return append(dst, "null"...), nil
		}
		if depth >= maxAttributeDepth {
			return dst, tooDeep()
		}
		dst = append(dst, '[')
		for index, item := range v {
			if index > 0 {
				dst = append(dst, ',')
			}
			var err error
			dst, err = appendValue(dst, item, depth+1)
			if err != nil {
				return dst, withPathStep(indexStep(index), err)
			}
		}
		return append(dst, ']'), nil
	}
	return appendReflectedValue(dst, reflect.ValueOf(value), depth)
}

func appendCheckedFloat(dst []byte, number float64, value any) ([]byte, error) {
	if math.IsNaN(number) || math.IsInf(number, 0) {
		return dst, &attributeError{reason: "is " + strconv.FormatFloat(number, 'g', -1, 64) + "; only finite numbers can be stored"}
	}
	return appendFloat(dst, value), nil
}

var timeType = reflect.TypeFor[time.Time]()

// appendReflectedValue handles every value the fast path of appendValue does
// not, such as named kinds, typed slices and maps, and the refused values.
func appendReflectedValue(dst []byte, v reflect.Value, depth int) ([]byte, error) {
	switch v.Kind() {
	case reflect.Bool:
		return strconv.AppendBool(dst, v.Bool()), nil
	case reflect.String:
		return appendString(dst, v.String()), nil
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return strconv.AppendInt(dst, v.Int(), 10), nil
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
		return strconv.AppendUint(dst, v.Uint(), 10), nil
	case reflect.Float32:
		return appendCheckedFloat(dst, v.Float(), float32(v.Float()))
	case reflect.Float64:
		return appendCheckedFloat(dst, v.Float(), v.Float())
	case reflect.Slice, reflect.Array:
		if v.Type().Elem().Kind() == reflect.Uint8 {
			return dst, refused(v, "encode bytes as text first, for example base64 or hex")
		}
		if v.Kind() == reflect.Slice && v.IsNil() {
			return append(dst, "null"...), nil
		}
		if depth >= maxAttributeDepth {
			return dst, tooDeep()
		}
		dst = append(dst, '[')
		for index := range v.Len() {
			if index > 0 {
				dst = append(dst, ',')
			}
			var err error
			dst, err = appendValue(dst, v.Index(index).Interface(), depth+1)
			if err != nil {
				return dst, withPathStep(indexStep(index), err)
			}
		}
		return append(dst, ']'), nil
	case reflect.Map:
		if v.Type().Key().Kind() != reflect.String {
			return dst, refused(v, "keys must be strings")
		}
		if v.IsNil() {
			return append(dst, "null"...), nil
		}
		if depth >= maxAttributeDepth {
			return dst, tooDeep()
		}
		dst = append(dst, '{')
		first := true
		for key, item := range v.Seq2() {
			if !first {
				dst = append(dst, ',')
			}
			first = false
			dst = appendString(dst, key.String())
			dst = append(dst, ':')
			var err error
			dst, err = appendValue(dst, item.Interface(), depth+1)
			if err != nil {
				return dst, withPathStep(keyStep(key.String()), err)
			}
		}
		return append(dst, '}'), nil
	case reflect.Struct:
		if v.Type() == timeType {
			return dst, refused(v, "store t.UnixNano() or t.Format(time.RFC3339Nano)")
		}
		return dst, refused(v, "store a map[string]any of its values, or its text")
	case reflect.Pointer:
		return dst, refused(v, "dereference it first")
	}
	return dst, refused(v, "values of this type cannot be stored")
}

func refused(v reflect.Value, advice string) error {
	return &attributeError{reason: "has the type " + v.Type().String() + "; " + advice}
}
