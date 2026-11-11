// Sends spans, log records and metrics through a real OpenTelemetry NodeSDK 3.x
// into a store, then prints what landed there.
//
//   node examples/opentelemetry.ts <dir>
//
// Run `npm run build` first. Read the rows afterwards with
// `duckmon --dir <dir> query "SELECT * FROM events"`.

import { createRequire } from "node:module";

import { context, logs, metrics, SpanStatusCode, trace } from "@opentelemetry/api";
import { HttpInstrumentation } from "@opentelemetry/instrumentation-http";
import { SimpleLogRecordProcessor } from "@opentelemetry/sdk-logs";
import { PeriodicExportingMetricReader } from "@opentelemetry/sdk-metrics";
import { NodeSDK } from "@opentelemetry/sdk-node";
import { SimpleSpanProcessor } from "@opentelemetry/sdk-trace";

import { open } from "../dist/index.js";
import { DuckmonLogRecordExporter, DuckmonMetricExporter, DuckmonSpanExporter } from "../dist/opentelemetry/index.js";

const dir = process.argv[2];
if (dir === undefined) {
  console.error("usage: opentelemetry.ts <dir>");
  process.exit(2);
}

const store = await open(dir);
const sdk = new NodeSDK({
  serviceName: "opentelemetry-example",
  spanProcessors: [new SimpleSpanProcessor({ exporter: new DuckmonSpanExporter(store) })],
  logRecordProcessors: [new SimpleLogRecordProcessor({ exporter: new DuckmonLogRecordExporter(store) })],
  metricReaders: [
    new PeriodicExportingMetricReader({ exporter: new DuckmonMetricExporter(store), exportIntervalMillis: 10_000 }),
  ],
  instrumentations: [new HttpInstrumentation()],
});
sdk.start();

// The HTTP instrumentation patches modules loaded with require(). An ES module
// import is patched only with the loader hook, see the README.
const http = createRequire(import.meta.url)("node:http") as typeof import("node:http");

const tracer = trace.getTracer("example.checkout", "1.0.0");
const logger = logs.getLogger("example.checkout");
const meter = metrics.getMeter("example.checkout");
const orders = meter.createCounter("example.orders", { description: "Orders placed" });
const orderTime = meter.createHistogram("example.order.duration", { unit: "ms" });
const queueLength = meter.createGauge("example.queue.length");

logger.emit({ body: "checkout service started", severityNumber: 9, severityText: "INFO" });

const background = tracer.startSpan("refresh prices");
background.end();

await tracer.startActiveSpan("checkout", async (checkout) => {
  logger.emit({ body: { order: 42, items: ["book", "pen"] }, severityNumber: 9, attributes: { "user.id": "u7" } });
  const payment = tracer.startSpan("charge card", {
    kind: 2,
    links: [{ context: background.spanContext(), attributes: { reason: "prices used" } }],
  });
  payment.recordException(new Error("card declined"));
  payment.setStatus({ code: SpanStatusCode.ERROR, message: "card declined" });
  context.with(trace.setSpan(context.active(), payment), () => {
    logger.emit({ body: "payment failed", severityNumber: 17, severityText: "ERROR" });
  });
  payment.end();

  await getOverHTTP();
  orders.add(1, { "payment.method": "card" });
  orderTime.record(37, { "payment.method": "card" });
  queueLength.record(3);
  checkout.end();
});

// One request from a client to a server in this process: one trace with a client and a server span.
async function getOverHTTP(): Promise<void> {
  const server = http.createServer((_request, response) => response.end("ok"));
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as { port: number };
  await new Promise<void>((resolve, reject) => {
    http.get(`http://127.0.0.1:${port}/users/7`, (response) => {
      response.resume();
      response.on("end", resolve);
    }).on("error", reject);
  });
  await new Promise<void>((resolve) => server.close(() => resolve()));
}

// The SDK writes the last metric snapshot at shutdown. The exporters never close the store.
await sdk.shutdown();
const rows = await store.query<{ kind: string; rows: bigint }>(
  `SELECT CASE WHEN duration_ns IS NOT NULL THEN 'span'
               WHEN message IS NOT NULL THEN 'log'
               WHEN attrs->>'metric.type' IS NOT NULL THEN 'metric'
               ELSE 'span event' END AS kind,
          count(*) AS rows
   FROM events WHERE resource->>'service.name' = 'opentelemetry-example'
   GROUP BY kind ORDER BY kind`,
);
for (const row of rows) console.log(`${row.kind}: ${row.rows}`);
await store.close();
