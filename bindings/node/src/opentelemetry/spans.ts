import type { AttributeValue, Attributes, Event } from "../events.js";
import { StoreExporter } from "./export.js";
import type { ExportResult } from "./export.js";
import { nameOr, resourceAttributes, scopeAttributes, toAttributes, toNanos } from "./values.js";
import type { HrTime, InstrumentationScope, Resource } from "./values.js";

/** The fields of an OpenTelemetry span context that the exporter reads. */
interface SpanContext {
  readonly traceId: string;
  readonly spanId: string;
  readonly traceState?: { serialize(): string } | undefined;
}

/** The fields of a `ReadableSpan` of `@opentelemetry/sdk-trace` 2.x and 3.x that the exporter reads. */
interface ReadableSpan {
  readonly name: string;
  readonly kind: number;
  spanContext(): SpanContext;
  readonly parentSpanContext?: SpanContext | undefined;
  readonly startTime: HrTime;
  readonly endTime: HrTime;
  readonly status: { readonly code: number; readonly message?: string | undefined };
  readonly attributes: Readonly<Record<string, unknown>>;
  readonly links: readonly {
    readonly context: SpanContext;
    readonly attributes?: Readonly<Record<string, unknown>> | undefined;
  }[];
  readonly events: readonly {
    readonly name: string;
    readonly time: HrTime;
    readonly attributes?: Readonly<Record<string, unknown>> | undefined;
  }[];
  readonly resource: Resource;
  readonly instrumentationScope: InstrumentationScope;
  readonly droppedAttributesCount: number;
  readonly droppedEventsCount: number;
  readonly droppedLinksCount: number;
}

/** `SpanKind` of `@opentelemetry/api`, by value. */
const SPAN_KINDS = ["internal", "server", "client", "producer", "consumer"];

/** `SpanStatusCode` of `@opentelemetry/api`. */
const STATUS_OK = 1;
const STATUS_ERROR = 2;

/**
 * Writes ended spans into a duckmon store: one row for each span, and one row
 * for each event of a span, such as an exception.
 */
export class DuckmonSpanExporter extends StoreExporter {
  export(spans: readonly ReadableSpan[], resultCallback: (result: ExportResult) => void): void {
    this.exportRecords(() => spans, spanEvents, resultCallback);
  }
}

/** The rows of one span: the span itself first, then its events. */
export function spanEvents(span: ReadableSpan): Event[] {
  const context = span.spanContext();
  const resource = resourceAttributes(span.resource);
  const scope = scopeAttributes(span.instrumentationScope);
  const start = toNanos(span.startTime);
  const end = toNanos(span.endTime);

  const attrs = toAttributes(span.attributes);
  attrs["span.kind"] = SPAN_KINDS[span.kind] ?? String(span.kind);
  if (span.status.code === STATUS_OK) attrs.status = "ok";
  if (span.status.code === STATUS_ERROR) {
    attrs.status = "error";
    if (typeof span.status.message === "string" && span.status.message !== "") attrs.error = span.status.message;
  }
  const traceState = context.traceState?.serialize();
  if (traceState !== undefined && traceState !== "") attrs["span.trace_state"] = traceState;
  if (span.links.length > 0) attrs["span.links"] = span.links.map(linkValue);
  if (span.droppedAttributesCount > 0) attrs["span.dropped_attributes"] = span.droppedAttributesCount;
  if (span.droppedEventsCount > 0) attrs["span.dropped_events"] = span.droppedEventsCount;
  if (span.droppedLinksCount > 0) attrs["span.dropped_links"] = span.droppedLinksCount;
  Object.assign(attrs, scope);

  const spanRow: Event = {
    name: nameOr(span.name, "span"),
    ts: start,
    durationNs: end > start ? end - start : 0n,
    traceId: context.traceId,
    spanId: context.spanId,
    attrs,
    resource,
  };
  const parentId = span.parentSpanContext?.spanId;
  if (parentId !== undefined) spanRow.parentId = parentId;

  const rows = [spanRow];
  for (const event of span.events) {
    rows.push({
      name: nameOr(event.name, "event"),
      ts: toNanos(event.time),
      traceId: context.traceId,
      spanId: context.spanId,
      attrs: Object.assign(toAttributes(event.attributes ?? {}), scope),
      resource,
    });
  }
  return rows;
}

/** A link as stored in `span.links`: the attributes sit three levels deep in `attrs`. */
function linkValue(link: ReadableSpan["links"][number]): AttributeValue {
  const value: Attributes = { trace_id: link.context.traceId, span_id: link.context.spanId };
  const traceState = link.context.traceState?.serialize();
  if (traceState !== undefined && traceState !== "") value.trace_state = traceState;
  const attributes = toAttributes(link.attributes ?? {}, 3);
  if (Object.keys(attributes).length > 0) value.attributes = attributes;
  return value;
}
