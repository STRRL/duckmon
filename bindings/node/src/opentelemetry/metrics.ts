import type { AttributeValue, Event } from "../events.js";
import { StoreExporter } from "./export.js";
import type { ExportResult } from "./export.js";
import { nameOr, resourceAttributes, scopeAttributes, toAttributeValue, toAttributes, toNanos } from "./values.js";
import type { HrTime, InstrumentationScope, Resource } from "./values.js";

/** The value of a histogram data point of `@opentelemetry/sdk-metrics`. */
interface Histogram {
  readonly buckets: { readonly boundaries: readonly number[]; readonly counts: readonly number[] };
  readonly count: number;
  readonly sum?: number | undefined;
  readonly min?: number | undefined;
  readonly max?: number | undefined;
}

/** The value of an exponential histogram data point of `@opentelemetry/sdk-metrics`. */
interface ExponentialHistogram {
  readonly count: number;
  readonly sum?: number | undefined;
  readonly min?: number | undefined;
  readonly max?: number | undefined;
  readonly scale: number;
  readonly zeroCount: number;
  readonly positive: { readonly offset: number; readonly bucketCounts: readonly number[] };
  readonly negative: { readonly offset: number; readonly bucketCounts: readonly number[] };
}

interface DataPoint {
  readonly startTime: HrTime;
  readonly endTime: HrTime;
  readonly attributes: Readonly<Record<string, unknown>>;
  readonly value: number | Histogram | ExponentialHistogram;
}

/** One `MetricData` of `@opentelemetry/sdk-metrics` 2.x and 3.x: a sum, a gauge or a histogram. */
interface MetricData {
  readonly descriptor: { readonly name: string; readonly description: string; readonly unit: string };
  readonly aggregationTemporality: number;
  readonly dataPointType: number;
  readonly isMonotonic?: boolean | undefined;
  readonly dataPoints: readonly DataPoint[];
}

/** The fields of a `ResourceMetrics` that the exporter reads. */
interface ResourceMetrics {
  readonly resource: Resource;
  readonly scopeMetrics: readonly {
    readonly scope: InstrumentationScope;
    readonly metrics: readonly MetricData[];
  }[];
}

/** One metric with what it was collected for. */
interface ScopedMetric {
  readonly resource: Resource;
  readonly scope: InstrumentationScope;
  readonly metric: MetricData;
}

/** `AggregationTemporality.CUMULATIVE` of `@opentelemetry/sdk-metrics`. `DELTA` is 0. */
const CUMULATIVE = 1;
const TEMPORALITY_NAMES = ["delta", "cumulative"];

/** `DataPointType` of `@opentelemetry/sdk-metrics`, by value: 0 histogram, 1 exponential histogram, 2 gauge, 3 sum. */
const HISTOGRAM = 0;
const EXPONENTIAL_HISTOGRAM = 1;
const SUM = 3;
const TYPE_NAMES = ["histogram", "exponential_histogram", "gauge", "sum"];

/**
 * Writes metric snapshots into a duckmon store: one row for each data point.
 * Use it with a `PeriodicExportingMetricReader`.
 */
export class DuckmonMetricExporter extends StoreExporter {
  export(metrics: ResourceMetrics, resultCallback: (result: ExportResult) => void): void {
    const scopedMetrics = (): ScopedMetric[] =>
      metrics.scopeMetrics.flatMap((scopeMetrics) =>
        scopeMetrics.metrics.map((metric) => ({ resource: metrics.resource, scope: scopeMetrics.scope, metric })),
      );
    this.exportRecords(scopedMetrics, metricEvents, resultCallback);
  }

  /** Always cumulative, the contract's rule for counters. */
  selectAggregationTemporality(): number {
    return CUMULATIVE;
  }
}

/** The rows of one metric: one for each data point. */
export function metricEvents({ resource, scope, metric }: ScopedMetric): Event[] {
  const resourceAttrs = resourceAttributes(resource);
  const scopeAttrs = scopeAttributes(scope);
  const type = metric.dataPointType;
  return metric.dataPoints.map((point) => {
    const attrs = toAttributes(point.attributes);
    attrs["metric.type"] = TYPE_NAMES[type] ?? String(type);
    const { unit, description } = metric.descriptor;
    if (typeof unit === "string" && unit !== "") attrs["metric.unit"] = unit;
    if (typeof description === "string" && description !== "") attrs["metric.description"] = description;
    attrs["metric.temporality"] = TEMPORALITY_NAMES[metric.aggregationTemporality] ?? String(metric.aggregationTemporality);
    if (type === SUM && metric.isMonotonic !== undefined) attrs["metric.monotonic"] = metric.isMonotonic;
    attrs["metric.start_ns"] = toNanos(point.startTime);
    attrs["metric.value"] = pointValue(type, point.value);
    Object.assign(attrs, scopeAttrs);
    return {
      name: nameOr(metric.descriptor.name, "metric"),
      ts: toNanos(point.endTime),
      attrs,
      resource: resourceAttrs,
    };
  });
}

/** The measured value of a data point, with OTLP field names. It sits at level 1 of `attrs`. */
function pointValue(type: number, value: DataPoint["value"]): AttributeValue {
  let stored: unknown = value;
  if (type === HISTOGRAM) {
    const histogram = value as Histogram;
    stored = {
      count: histogram.count,
      sum: histogram.sum,
      min: histogram.min,
      max: histogram.max,
      explicit_bounds: histogram.buckets.boundaries,
      bucket_counts: histogram.buckets.counts,
    };
  } else if (type === EXPONENTIAL_HISTOGRAM) {
    const histogram = value as ExponentialHistogram;
    stored = {
      count: histogram.count,
      sum: histogram.sum,
      min: histogram.min,
      max: histogram.max,
      scale: histogram.scale,
      zero_count: histogram.zeroCount,
      positive: { offset: histogram.positive.offset, bucket_counts: histogram.positive.bucketCounts },
      negative: { offset: histogram.negative.offset, bucket_counts: histogram.negative.bucketCounts },
    };
  }
  return toAttributeValue(stored, 1) ?? null;
}
