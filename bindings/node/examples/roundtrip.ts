// Writes events and reads them back, like crates/duckmon/examples/roundtrip.rs.
//
//   node examples/roundtrip.ts write <dir> <count> <holdSeconds>
//   node examples/roundtrip.ts count <dir> <expected>
//
// Run `npm run build` first. Bun runs this file the same way.

import { randomUUID } from "node:crypto";
import { setTimeout as sleep } from "node:timers/promises";

import { nowNanos, open } from "../dist/index.js";
import type { Event, Store } from "../dist/index.js";

const NANOS_PER_SECOND = 1_000_000_000n;

async function main(args: string[]): Promise<number> {
  const [command, dir, number, holdSeconds] = args;
  if (command === "write" && dir !== undefined && number !== undefined && holdSeconds !== undefined) {
    return (await write(dir, parse(number), parse(holdSeconds))) ? 0 : 1;
  }
  if (command === "count" && dir !== undefined && number !== undefined && holdSeconds === undefined) {
    return (await count(dir, parse(number))) ? 0 : 1;
  }
  console.error("usage: roundtrip.ts write <dir> <count> <holdSeconds>");
  console.error("       roundtrip.ts count <dir> <expected>");
  return 2;
}

function parse(text: string): number {
  const value = Number(text);
  if (!Number.isInteger(value) || value < 0) throw new Error(`not a number: ${text}`);
  return value;
}

function hexID(): string {
  return randomUUID().replaceAll("-", "");
}

async function write(dir: string, count: number, holdSeconds: number): Promise<boolean> {
  const store = await open(dir, { flushIntervalMs: 1000, resource: { "service.name": "roundtrip" } });

  const run = hexID();
  const now = nowNanos();
  const events: Event[] = [];
  let expectedSpanNs = 0n;
  let expectedMetricSum = 0n;
  for (let i = 0; i < count; i++) {
    const ts = now - BigInt(i) * 20n * NANOS_PER_SECOND;
    if (i % 3 === 0) {
      events.push({ name: "app.http", ts, message: `handled request ${i}`, attrs: { level: "info", run } });
    } else if (i % 3 === 1) {
      const durationNs = BigInt(i) * 1000n;
      events.push({
        name: "http.request",
        ts,
        durationNs,
        traceId: hexID(),
        spanId: (i + 1).toString(16).padStart(16, "0"),
        attrs: { run },
      });
      expectedSpanNs += durationNs;
    } else {
      events.push({ name: "process.stats", ts, attrs: { value: i, "cpu.percent": 0.5, run } });
      expectedMetricSum += BigInt(i);
    }
  }

  const half = Math.floor(events.length / 2);
  for (let start = 0; start < half; start += 100) {
    store.write(events.slice(start, Math.min(start + 100, half)));
  }
  await store.flush();
  for (let start = half; start < events.length; start += 100) {
    store.write(events.slice(start, start + 100));
  }

  const [row] = await store.query<{ rows: bigint; span_ns: bigint | null; metric_sum: bigint | null; wrong_date: bigint }>(
    `SELECT count(*) AS rows,
            sum(duration_ns) FILTER (WHERE span_id IS NOT NULL) AS span_ns,
            sum((attrs->>'value')::BIGINT) FILTER (WHERE name = 'process.stats') AS metric_sum,
            count(*) FILTER (WHERE date <> CAST(strftime(ts, '%Y-%m-%d') AS DATE)) AS wrong_date
     FROM events WHERE attrs->>'run' = '${run}'`,
  );
  const [buffer] = await store.query<{ rows: bigint }>("SELECT count(*) AS rows FROM unflushed_events");

  const actual = [row!.rows, row!.span_ns ?? 0n, row!.metric_sum ?? 0n, row!.wrong_date];
  const expected = [BigInt(count), expectedSpanNs, expectedMetricSum, 0n];
  if (actual.some((value, index) => value !== expected[index])) {
    console.log(`roundtrip mismatch run=${run}`);
    console.log(`expected rows, span_ns, metric_sum, wrong_date = ${expected.join(", ")}`);
    console.log(`actual   rows, span_ns, metric_sum, wrong_date = ${actual.join(", ")}`);
    await store.close();
    return false;
  }
  console.log(`roundtrip ok run=${run} rows=${actual[0]} span_ns=${actual[1]} metric_sum=${actual[2]}`);
  console.log(`buffer rows=${buffer!.rows}`);

  await sleep(holdSeconds * 1000);
  await store.close();
  return true;
}

async function count(dir: string, expected: number): Promise<boolean> {
  const store: Store = await open(dir);
  const [row] = await store.query<{ rows: bigint }>("SELECT count(*) AS rows FROM events");
  console.log(String(row!.rows));
  await store.close();
  return row!.rows === BigInt(expected);
}

process.exitCode = await main(process.argv.slice(2));
