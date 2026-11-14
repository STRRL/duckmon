package duckmon

import (
	"bytes"
	"encoding/json"
	"math/big"
	"strings"
	"time"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/ipc"
)

// Result is the outcome of Query: the column names and the rows, both in
// result order. Two columns may share a name.
type Result struct {
	Columns []string
	// Rows holds one slice per row, with one value per column. The README
	// lists the Go type of each DuckDB type.
	Rows [][]any
}

// MapEntry is one entry of a DuckDB MAP value, which Query returns as
// []MapEntry in key order. Keys may be lists or other values Go maps cannot hold.
type MapEntry struct {
	Key   any
	Value any
}

// readValue reads the value at one index of an array, nil for NULL.
type readValue func(index int) any

// resultFromArrowIPC decodes an Arrow IPC stream. The same rules apply at the
// top level and inside lists, structs and maps. It returns an *Error with
// CodeInvalidArgument for a column type with no exact Go value.
func resultFromArrowIPC(arrowIPC []byte) (*Result, error) {
	reader, err := ipc.NewReader(bytes.NewReader(arrowIPC))
	if err != nil {
		return nil, unreadableResult(err)
	}
	defer reader.Release()
	fields := reader.Schema().Fields()
	result := &Result{Columns: make([]string, len(fields)), Rows: [][]any{}}
	for index, field := range fields {
		result.Columns[index] = field.Name
	}
	for reader.Next() {
		batch := reader.RecordBatch()
		readers := make([]readValue, len(fields))
		for index, field := range fields {
			readers[index], err = valueReader(field, batch.Column(index), field.Name)
			if err != nil {
				return nil, err
			}
		}
		for row := range int(batch.NumRows()) {
			values := make([]any, len(readers))
			for column, read := range readers {
				values[column] = read(row)
			}
			result.Rows = append(result.Rows, values)
		}
	}
	if err := reader.Err(); err != nil {
		return nil, unreadableResult(err)
	}
	return result, nil
}

func unreadableResult(err error) error {
	return &Error{Code: CodeInternal, Message: "cannot read the query result: " + err.Error()}
}

// valueReader returns a reader for one array, which gives nil for NULL.
// column is the top level column, named in errors.
func valueReader(field arrow.Field, values arrow.Array, column string) (readValue, error) {
	if name := extensionTypeName(field); name != "" {
		return nil, unsupportedType(column, name)
	}
	read, err := nonNullReader(values, column)
	if err != nil {
		return nil, err
	}
	return func(index int) any {
		if values.IsNull(index) {
			return nil
		}
		return read(index)
	}, nil
}

// nonNullReader returns a reader for the values of an array that are not NULL.
func nonNullReader(values arrow.Array, column string) (readValue, error) {
	switch values := values.(type) {
	case *array.Null:
		return func(int) any { return nil }, nil
	case *array.Boolean:
		return func(index int) any { return values.Value(index) }, nil
	case *array.Int8:
		return func(index int) any { return values.Value(index) }, nil
	case *array.Int16:
		return func(index int) any { return values.Value(index) }, nil
	case *array.Int32:
		return func(index int) any { return values.Value(index) }, nil
	case *array.Int64:
		return func(index int) any { return values.Value(index) }, nil
	case *array.Uint8:
		return func(index int) any { return values.Value(index) }, nil
	case *array.Uint16:
		return func(index int) any { return values.Value(index) }, nil
	case *array.Uint32:
		return func(index int) any { return values.Value(index) }, nil
	case *array.Uint64:
		return func(index int) any { return values.Value(index) }, nil
	case *array.Float32:
		return func(index int) any { return values.Value(index) }, nil
	case *array.Float64:
		return func(index int) any { return values.Value(index) }, nil
	case *array.Decimal128:
		scale := values.DataType().(*arrow.Decimal128Type).Scale
		return func(index int) any { return decimalValue(values.Value(index).BigInt(), scale) }, nil
	// The text and bytes readers copy, so that a kept value does not hold the
	// whole query result in memory.
	case *array.String:
		return func(index int) any { return strings.Clone(values.Value(index)) }, nil
	case *array.LargeString:
		return func(index int) any { return strings.Clone(values.Value(index)) }, nil
	case *array.StringView:
		return func(index int) any { return strings.Clone(values.Value(index)) }, nil
	case *array.Binary:
		return func(index int) any { return bytes.Clone(values.Value(index)) }, nil
	case *array.LargeBinary:
		return func(index int) any { return bytes.Clone(values.Value(index)) }, nil
	case *array.BinaryView:
		return func(index int) any { return bytes.Clone(values.Value(index)) }, nil
	case *array.FixedSizeBinary:
		return func(index int) any { return bytes.Clone(values.Value(index)) }, nil
	case *array.Date32:
		return func(index int) any { return dateFromDays(int64(values.Value(index))) }, nil
	case *array.Date64:
		return func(index int) any { return time.UnixMilli(int64(values.Value(index))).UTC() }, nil
	case *array.Timestamp:
		unit := values.DataType().(*arrow.TimestampType).Unit
		return func(index int) any { return timeFromTimestamp(int64(values.Value(index)), unit) }, nil
	case *array.Time32:
		unit := values.DataType().(*arrow.Time32Type).Unit
		return func(index int) any { return time.Duration(values.Value(index)) * unit.Multiplier() }, nil
	case *array.Time64:
		unit := values.DataType().(*arrow.Time64Type).Unit
		return func(index int) any { return time.Duration(values.Value(index)) * unit.Multiplier() }, nil
	case *array.Map:
		mapType := values.DataType().(*arrow.MapType)
		readKey, err := valueReader(mapType.KeyField(), values.Keys(), column)
		if err != nil {
			return nil, err
		}
		readItem, err := valueReader(mapType.ItemField(), values.Items(), column)
		if err != nil {
			return nil, err
		}
		return func(index int) any {
			start, end := values.ValueOffsets(index)
			entries := make([]MapEntry, 0, end-start)
			for entry := int(start); entry < int(end); entry++ {
				entries = append(entries, MapEntry{Key: readKey(entry), Value: readItem(entry)})
			}
			return entries
		}, nil
	case array.ListLike:
		listType := values.DataType().(arrow.ListLikeType)
		readItem, err := valueReader(listType.ElemField(), values.ListValues(), column)
		if err != nil {
			return nil, err
		}
		return func(index int) any {
			start, end := values.ValueOffsets(index)
			items := make([]any, 0, end-start)
			for item := int(start); item < int(end); item++ {
				items = append(items, readItem(item))
			}
			return items
		}, nil
	case *array.Struct:
		structType := values.DataType().(*arrow.StructType)
		names := make([]string, values.NumField())
		readers := make([]readValue, values.NumField())
		for member := range readers {
			field := structType.Field(member)
			names[member] = field.Name
			var err error
			readers[member], err = valueReader(field, values.Field(member), column)
			if err != nil {
				return nil, err
			}
		}
		return func(index int) any {
			object := make(map[string]any, len(readers))
			for member, read := range readers {
				object[names[member]] = read(index)
			}
			return object
		}, nil
	case *array.Dictionary:
		readWord, err := nonNullReader(values.Dictionary(), column)
		if err != nil {
			return nil, err
		}
		return func(index int) any { return readWord(values.GetValueIndex(index)) }, nil
	}
	return nil, unsupportedType(column, duckDBTypeName(values.DataType()))
}

// extensionTypeName returns the name of an Arrow extension type, or "" when
// the field has none. DuckDB names its own types, such as BIGNUM, in the
// extension metadata.
func extensionTypeName(field arrow.Field) string {
	if extension, ok := field.Type.(arrow.ExtensionType); ok {
		return extension.ExtensionName()
	}
	name, ok := field.Metadata.GetValue("ARROW:extension:name")
	if !ok {
		return ""
	}
	var metadata struct {
		TypeName string `json:"type_name"`
	}
	text, _ := field.Metadata.GetValue("ARROW:extension:metadata")
	if json.Unmarshal([]byte(text), &metadata) == nil && metadata.TypeName != "" {
		return strings.ToUpper(metadata.TypeName)
	}
	return name
}

// duckDBTypeName names an Arrow type that has no Go value, with the DuckDB
// name where the DuckDB type is clear.
func duckDBTypeName(dataType arrow.DataType) string {
	switch dataType.ID() {
	case arrow.INTERVAL_MONTHS, arrow.INTERVAL_DAY_TIME, arrow.INTERVAL_MONTH_DAY_NANO:
		return "INTERVAL"
	case arrow.SPARSE_UNION, arrow.DENSE_UNION:
		return "UNION"
	}
	return dataType.String()
}

func unsupportedType(column, typeName string) error {
	return &Error{
		Code:    CodeInvalidArgument,
		Message: `column "` + column + `" has the type ` + typeName + ", which has no exact Go value; cast it in SQL, for example CAST(" + column + " AS VARCHAR)",
	}
}

// decimalValue returns a decimal with no digits after the point as a
// *big.Int, and any other decimal as exact text such as "-0.01".
func decimalValue(unscaled *big.Int, scale int32) any {
	if scale <= 0 {
		return unscaled.Mul(unscaled, new(big.Int).Exp(big.NewInt(10), big.NewInt(int64(-scale)), nil))
	}
	return decimalText(unscaled, scale)
}

// decimalText writes an unscaled decimal with scale digits after the point.
func decimalText(unscaled *big.Int, scale int32) string {
	digits := new(big.Int).Abs(unscaled).String()
	if pad := int(scale) + 1 - len(digits); pad > 0 {
		digits = strings.Repeat("0", pad) + digits
	}
	point := len(digits) - int(scale)
	sign := ""
	if unscaled.Sign() < 0 {
		sign = "-"
	}
	return sign + digits[:point] + "." + digits[point:]
}

const secondsPerDay = 24 * 60 * 60

// dateFromDays returns 00:00 UTC of a day counted from 1970-01-01.
func dateFromDays(days int64) time.Time {
	return time.Unix(days*secondsPerDay, 0).UTC()
}

// timeFromTimestamp returns the UTC time of a timestamp in unit since 1970.
// Each unit has its own conversion, so no value overflows.
func timeFromTimestamp(value int64, unit arrow.TimeUnit) time.Time {
	switch unit {
	case arrow.Second:
		return time.Unix(value, 0).UTC()
	case arrow.Millisecond:
		return time.UnixMilli(value).UTC()
	case arrow.Microsecond:
		return time.UnixMicro(value).UTC()
	}
	return time.Unix(0, value).UTC()
}
