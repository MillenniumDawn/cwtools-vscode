import type cytoscape from "cytoscape";
import { writeFileSync } from "node:fs";
import { afterAll, afterEach, beforeAll, beforeEach, expect, test, vi } from "vitest";
import { installGraph, node, render, resetGraphState, state } from "./support/graphHeadless";

beforeAll(installGraph);
beforeEach(() => { vi.useFakeTimers(); resetGraphState(); });
afterEach(() => { vi.clearAllTimers(); vi.useRealTimers(); });
afterAll(() => { state.cores.forEach((core) => core.destroy()); vi.unstubAllGlobals(); });

function listenerCounts(size: number) {
	const prototype = Object.getPrototypeOf(state.cores[0].collection()) as { on: cytoscape.NodeSingular["on"] };
	const corePrototype = Object.getPrototypeOf(state.cores[0]) as { on: cytoscape.Core["on"] };
	const nodeOn = vi.spyOn(prototype, "on");
	const coreOn = vi.spyOn(corePrototype, "on");
	try {
		const cy = render(Array.from({ length: size }, (_, i) => node(`n${i}`, [`n${(i + 1) % size}`])));
		return { cy, perNode: nodeOn.mock.calls.length, core: coreOn.mock.calls.length };
	} finally { nodeOn.mockRestore(); coreOn.mockRestore(); }
}

function scratchCollections(cy: cytoscape.Core) {
	const collectionPrototype = Object.getPrototypeOf(cy.collection()) as object;
	const collections: { path: string; collection: cytoscape.CollectionReturnValue }[] = [];
	const visited = new Set<object>();
	const visit = (value: unknown, path: string) => {
		if (typeof value !== "object" || value === null) return;
		if (Object.prototype.isPrototypeOf.call(collectionPrototype, value)) {
			collections.push({ path, collection: value as cytoscape.CollectionReturnValue });
			return;
		}
		if (visited.has(value)) return;
		visited.add(value);
		if (value instanceof Map) {
			for (const [key, entry] of value) { visit(key, `${path}.key`); visit(entry, `${path}.value`); }
		} else if (value instanceof Set) {
			for (const entry of value) visit(entry, `${path}.value`);
		} else {
			for (const [key, entry] of Object.entries(value)) visit(entry, `${path}.${key}`);
		}
	};
	visit(cy.scratch(), "core");
	cy.nodes().forEach((n) => { visit(n.scratch(), n.id()); });
	return collections;
}

function traverseHovers(cy: cytoscape.Core) {
	cy.nodes().forEach((n) => { n.emit("mouseover"); n.emit("mouseout"); });
}

test.each([10, 100, 500])("tooltip listeners are delegated for %i nodes", (size) => {
	const counts = listenerCounts(size);
	expect(counts.perNode).toBe(0);
	expect(counts.core).toBeLessThanOrEqual(10);
});

test("listener count remains constant when graph size increases", () => {
	const small = listenerCounts(10);
	const large = listenerCounts(500);
	expect(large.core + large.perNode).toBe(small.core + small.perNode);
});

test("hovering every node leaves scratch collections bounded to the active hood", () => {
	const { cy } = listenerCounts(100);
	traverseHovers(cy);
	expect(scratchCollections(cy).map((entry) => entry.path)).toEqual([]);
	expect(cy.scratch("_hoverHood")).toBeUndefined();
	cy.$id("n0").emit("mouseover");
	const first: cytoscape.CollectionReturnValue = cy.scratch("_hoverHood") as cytoscape.CollectionReturnValue;
	expect(first.length).toBe(5);
	expect(scratchCollections(cy).map((entry) => entry.path)).toEqual(["core._hoverHood"]);
	expect(scratchCollections(cy)[0].collection).toBe(first);
	cy.$id("n1").emit("mouseover");
	const second: cytoscape.CollectionReturnValue = cy.scratch("_hoverHood") as cytoscape.CollectionReturnValue;
	expect(second.length).toBe(5);
	expect(second).not.toBe(first);
	expect(scratchCollections(cy).map((entry) => entry.path)).toEqual(["core._hoverHood"]);
	expect(scratchCollections(cy)[0].collection).toBe(second);
	cy.$id("n0").emit("mouseout");
	expect(cy.$id("n1").hasClass("highlight")).toBe(true);
	cy.$id("n1").emit("mouseout");
	expect(cy.scratch("_hoverHood")).toBeUndefined();
	expect(cy.$(".highlight, .semitransp").length).toBe(0);
	expect(scratchCollections(cy).map((entry) => entry.path)).toEqual([]);
});

test("replacing a graph cancels a pending tooltip expansion and destroys the tooltip", () => {
	const cy = render([node("a")]);
	cy.$id("a").emit("mouseover");
	const tip = state.tips[0];
	expect(tip.props.onHidden).toBeUndefined();
	render([node("replacement")]);
	expect(tip.destroy).toHaveBeenCalledTimes(1);
	expect(cy.scratch("_hoverHood")).toBeUndefined();
	expect(cy.$id("a").scratch("_tooltip")).toBeUndefined();
	vi.advanceTimersByTime(1000);
	expect(tip.props.onHidden).toBeUndefined();
});


test("mouseout before first hover creates no tooltip state", () => {
	const cy = render([node("a")]);
	cy.$id("a").emit("mouseout");
	expect(state.tips).toHaveLength(0);
	expect(cy.$id("a").scratch("_tooltip")).toBeUndefined();
});

test("repeated hover schedules one expansion and preserves simple/expanded behavior", () => {
	const cy = render([node("a")]);
	cy.$id("a").emit("mouseover");
	cy.$id("a").emit("mouseover");
	expect(state.tips).toHaveLength(1);
	vi.advanceTimersByTime(1000);
	const tip = state.tips[0];
	expect(typeof tip.props.onHidden).toBe("function");
	cy.$id("a").emit("mouseout");
	expect(tip.hide).not.toHaveBeenCalled();
	(tip.props.onHidden as (instance: unknown) => void)(tip);
	expect(tip.props.onHidden).toBeUndefined();
	cy.$id("a").emit("mouseover");
	cy.$id("a").emit("mouseout");
	expect(tip.hide).toHaveBeenCalledTimes(1);
});

test.skipIf(process.env.CWTOOLS_HOVER_BENCH !== "1")("record listener and retained collection counts for 500-node hover traversal", () => {
	const { cy, perNode, core } = listenerCounts(500);
	traverseHovers(cy);
	const retained = scratchCollections(cy);
	const result = {
		nodes: 500, edges: 500, perNodeListeners: perNode, coreListeners: core,
		scratchCollections: retained.length, scratchElements: retained.reduce((sum, entry) => sum + entry.collection.length, 0),
	};
	if (process.env.CWTOOLS_HOVER_BENCH_OUTPUT) {
		writeFileSync(process.env.CWTOOLS_HOVER_BENCH_OUTPUT, JSON.stringify(result, null, 2));
	}
}, 60000);
