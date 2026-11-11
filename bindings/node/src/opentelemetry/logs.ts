import type { Event } from "../events.js";
import { StoreExporter } from "./export.js";
import type { ExportResult } from "./export.js";
import { jsonText, nameOr, resourceAttributes, scopeAttributes, toAttributeValue, toAttributes, toNanos } from "./values.js";
import type { HrTime, InstrumentationScope, Resource } from "./values.js";

/** The fields of a `ReadableLogRecord` of `@opentelemetry/sdk-logs` 2.x and 3.x that the exporter reads. */
interface ReadableLogRecord {
  readonly hrTime: HrTime;
  readonly hrTimeObserved: HrTime;
  readonly spanContext?: { readonly traceId: string; readonly spanId: string } | undefined;
  readonly severityText?: string | undefined;
  readonly severityNumber?: number | undefined;
  readonly body?: unknown;
  readonly eventName?: string | undefined;
  readonly attributes: Readonly<Record<string, unknown>>;
  readonly droppedAttributesCount: number;
  readonly resource: Resource;
  readonly instrumentationScope: InstrumentationScope;
}

/** Writes log records into a duckmon store, one row each. */
export class DuckmonLogRecordExporter extends StoreExporter {
  export(logs: readonly ReadableLogRecord[], resultCallback: (result: ExportResult) => void): void {
    this.exportRecords(() => logs, (record) => [logRecordEvent(record)], resultCallback);
  }
}

/** The row of one log record. */
export function logRecordEvent(record: ReadableLogRecord): Event {
  const ts = toNanos(record.hrTime);
  const attrs = toAttributes(record.attributes);
  attrs.level = levelFromSeverity(record.severityNumber);
  const severityNumber = toAttributeValue(record.severityNumber);
  if (severityNumber !== undefined) attrs["log.severity_number"] = severityNumber;
  if (typeof record.severityText === "string" && record.severityText !== "") {
    attrs["log.severity_text"] = record.severityText;
  }
  const observed = toNanos(record.hrTimeObserved);
  if (observed !== ts) attrs["log.observed_ns"] = observed;
  if (record.droppedAttributesCount > 0) attrs["log.dropped_attributes"] = record.droppedAttributesCount;
  Object.assign(attrs, scopeAttributes(record.instrumentationScope));

  const row: Event = {
    name: nameOr(record.eventName, nameOr(record.instrumentationScope.name, "log")),
    ts,
    message: messageText(record.body),
    attrs,
    resource: resourceAttributes(record.resource),
  };
  if (record.spanContext !== undefined) {
    row.traceId = record.spanContext.traceId;
    row.spanId = record.spanContext.spanId;
  }
  return row;
}

/**
 * The contract's level for an OpenTelemetry severity number. Fatal (21 to 24)
 * gives `error`, because the contract has five levels. A missing or unknown
 * number gives `info`.
 */
export function levelFromSeverity(severityNumber: number | undefined): string {
  if (severityNumber === undefined || !(severityNumber >= 1 && severityNumber <= 24)) return "info";
  if (severityNumber <= 4) return "trace";
  if (severityNumber <= 8) return "debug";
  if (severityNumber <= 12) return "info";
  if (severityNumber <= 16) return "warn";
  return "error";
}

/** A string body as it is, any other body as JSON text, and no body as empty text. */
function messageText(body: unknown): string {
  const value = toAttributeValue(body, 0);
  if (value === undefined) return "";
  if (typeof value === "string") return value;
  return jsonText(value);
}
