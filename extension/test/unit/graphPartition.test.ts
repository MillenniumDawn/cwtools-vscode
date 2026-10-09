import type cytoscape from "cytoscape";
import { writeFileSync } from "node:fs";
import { performance } from "node:perf_hooks";
import { afterAll, beforeAll, expect, test, vi } from "vitest";
import { installGraph, node, render, state } from "./support/graphHeadless";

beforeAll(installGraph);
afterAll(() => { state.cores.forEach((core) => core.destroy()); vi.unstubAllGlobals(); });

function measuredRender(size: number) {
	const corePrototype = Object.getPrototypeOf(state.cores[0]) as { fit: cytoscape.Core["fit"] };
	const collectionPrototype = Object.getPrototypeOf(state.cores[0].collection()) as {
		union: cytoscape.CollectionReturnValue["union"];
		layout: cytoscape.CollectionReturnValue["layout"];
		difference: cytoscape.CollectionReturnValue["difference"];
		degree: cytoscape.NodeSingular["degree"];
	};
	const originalFit = corePrototype.fit;
	const originalUnion = collectionPrototype.union;
	const originalLayout = collectionPrototype.layout;
	const originalDifference = collectionPrototype.difference;
	const originalDegree = collectionPrototype.degree;
	let active = false;
	let started = 0;
	let elapsed = 0;
	let unions = 0;
	let copiedElements = 0;
	let differenceInputs = 0;
	let degreeCalls = 0;
	const fit = vi.spyOn(corePrototype, "fit").mockImplementation(function (this: cytoscape.Core, ...args) {
		const result = originalFit.apply(this, args);
		started = performance.now();
		active = true;
		return result;
	});
	const union = vi.spyOn(collectionPrototype, "union").mockImplementation(function (this: cytoscape.CollectionReturnValue, other) {
		if (active) {
			unions++;
			copiedElements += this.length + (typeof other === "string" ? this.cy().$(other).length : other.length);
		}
		return originalUnion.call(this, other);
	});
	const difference = vi.spyOn(collectionPrototype, "difference").mockImplementation(function (this: cytoscape.CollectionReturnValue, other) {
		if (active) differenceInputs += this.length + (typeof other === "string" ? this.cy().$(other).length : other.length);
		return originalDifference.call(this, other);
	});
	const degree = vi.spyOn(collectionPrototype, "degree").mockImplementation(function (this: cytoscape.NodeSingular, ...args) {
		if (active) degreeCalls++;
		return originalDegree.apply(this, args);
	});
	const layout = vi.spyOn(collectionPrototype, "layout").mockImplementation(function (this: cytoscape.CollectionReturnValue, options) {
		if (options.name === "elk") {
			elapsed = performance.now() - started;
			active = false;
		}
		return originalLayout.call(this, options);
	});
	try {
		// Identical mostly-disconnected fixture: 490 isolated + one 10-node chain
		// at size 500. For smaller sizes, keep the same connected fraction.
		const connected = Math.max(2, Math.floor(size / 50));
		const cy = render(Array.from({ length: size }, (_, i) =>
			node(`n${i}`, i < connected - 1 ? [`n${i + 1}`] : [])));
		return { cy, unions, copiedElements, differenceInputs, degreeCalls, elapsed };
	} finally {
		fit.mockRestore(); union.mockRestore(); layout.mockRestore();
		difference.mockRestore(); degree.mockRestore();
	}
}

test.each([100, 200, 500])("partition copying stays linear for %i mostly isolated nodes", (size) => {
	const measurement = measuredRender(size);
	expect(measurement.copiedElements).toBeLessThanOrEqual(4 * measurement.cy.elements().length);
	expect(measurement.unions).toBe(0);
	expect(measurement.degreeCalls).toBe(size);
	expect(measurement.differenceInputs).toBeLessThanOrEqual(2 * measurement.cy.elements().length);
});

test("partition preserves all edges, self-loops and connected components", () => {
	state.layouts.length = 0;
	const cy = render([
		node("isolated"), node("a", ["b", "c"]), node("b"), node("c"),
		node("loop", ["loop"]), node("otherA", ["otherB"]), node("otherB"),
	]);
	const singles = state.layouts.find((l) => l.name === "grid")!.ids;
	const connected = state.layouts.find((l) => l.name === "elk")!.ids;
	expect(singles).toEqual(["isolated"]);
	expect(new Set(connected)).toEqual(new Set(cy.elements().map((e) => e.id()).filter((id) => id !== "isolated")));
	expect(connected.length + singles.length).toBe(cy.elements().length);
});

test.skipIf(process.env.CWTOOLS_GRAPH_BENCH !== "1")("benchmark 500-node pre-layout partition", () => {
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
