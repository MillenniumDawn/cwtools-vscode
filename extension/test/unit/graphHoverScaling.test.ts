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

function retainedComplements(cy: cytoscape.Core) {
	const original = Object.getOwnPropertyDescriptor(Map.prototype, "set")!.value as
		(this: Map<unknown, unknown>, key: unknown, value: unknown) => Map<unknown, unknown>;
	let collections = 0;
	let elements = 0;
	const spy = vi.spyOn(Map.prototype, "set").mockImplementation(function (this: Map<unknown, unknown>, key: unknown, value: unknown) {
		if (typeof value === "object" && value !== null && "hood" in value && "rest" in value) {
			const rest = (value as { rest: cytoscape.CollectionReturnValue }).rest;
			collections++;
			elements += rest.length;
		}
		return original.call(this, key, value);
	});
	try {
		cy.nodes().forEach((n) => { n.emit("mouseover"); n.emit("mouseout"); });
		return { collections, elements };
	} finally { spy.mockRestore(); }
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

test("hovering every node retains no complement collections and only one active hood", () => {
	const { cy } = listenerCounts(100);
	const complements = retainedComplements(cy);
	expect(complements).toEqual({ collections: 0, elements: 0 });
	expect(cy.scratch("_hoverHood")).toBeUndefined();
	cy.$id("n0").emit("mouseover");
	const first: cytoscape.CollectionReturnValue = cy.scratch("_hoverHood") as cytoscape.CollectionReturnValue;
	expect(first.length).toBe(5);
	cy.$id("n1").emit("mouseover");
	const second: cytoscape.CollectionReturnValue = cy.scratch("_hoverHood") as cytoscape.CollectionReturnValue;
	expect(second.length).toBe(5);
	expect(second).not.toBe(first);
	cy.$id("n0").emit("mouseout");
	expect(cy.$id("n1").hasClass("highlight")).toBe(true);
	cy.$id("n1").emit("mouseout");
	expect(cy.scratch("_hoverHood")).toBeUndefined();
	expect(cy.$(".highlight, .semitransp").length).toBe(0);
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
	const complements = retainedComplements(cy);
	const result = { nodes: 500, edges: 500, perNodeListeners: perNode, coreListeners: core, ...complements };
	if (process.env.CWTOOLS_HOVER_BENCH_OUTPUT) {
		writeFileSync(process.env.CWTOOLS_HOVER_BENCH_OUTPUT, JSON.stringify(result, null, 2));
	}
}, 60000);
