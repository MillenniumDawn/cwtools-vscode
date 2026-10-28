import { writeFileSync } from "node:fs";
import { afterAll, afterEach, beforeAll, beforeEach, test, vi } from "vitest";
import { installGraph, resetGraphState, state } from "./support/graphHeadless";
import { listenerCounts, measuredRender, scratchCollections, traverseHovers } from "./support/graphMeasurements";

beforeAll(installGraph);
beforeEach(() => { vi.useFakeTimers(); resetGraphState(); });
afterEach(() => { vi.clearAllTimers(); vi.useRealTimers(); });
afterAll(() => { state.cores.forEach((core) => core.destroy()); vi.unstubAllGlobals(); });

test("benchmark 500-node pre-layout partition", () => {
	for (let i = 0; i < 5; i++) measuredRender(500);
	const samples = Array.from({ length: 30 }, () => measuredRender(500));
	const times = samples.map((sample) => sample.elapsed).sort((a, b) => a - b);
	const result = {
		nodes: 500, isolated: 490, samples: times.length,
		medianMs: times[Math.floor(times.length / 2)],
		p90Ms: times[Math.floor(times.length * 0.9)],
		unions: samples[0].unions, copiedElements: samples[0].copiedElements,
		differenceInputs: samples[0].differenceInputs, degreeCalls: samples[0].degreeCalls,
	};
	console.log(JSON.stringify(result));
	if (process.env.CWTOOLS_GRAPH_BENCH_OUTPUT) {
		writeFileSync(process.env.CWTOOLS_GRAPH_BENCH_OUTPUT, JSON.stringify(result, null, 2));
	}
}, 60000);

test("record listener and retained collection counts for 500-node hover traversal", () => {
	const { cy, perNode, core } = listenerCounts(500);
	traverseHovers(cy);
	const retained = scratchCollections(cy);
	const result = {
		nodes: 500, edges: 500, perNodeListeners: perNode, coreListeners: core,
		scratchCollections: retained.length, scratchElements: retained.reduce((sum, entry) => sum + entry.collection.length, 0),
	};
	console.log(JSON.stringify(result));
	if (process.env.CWTOOLS_HOVER_BENCH_OUTPUT) {
		writeFileSync(process.env.CWTOOLS_HOVER_BENCH_OUTPUT, JSON.stringify(result, null, 2));
	}
}, 60000);
