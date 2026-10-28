import type cytoscape from "cytoscape";
import { afterAll, afterEach, beforeAll, beforeEach, expect, test, vi } from "vitest";
import { installGraph, node, render, resetGraphState, state } from "./support/graphHeadless";
import { listenerCounts, scratchCollections, traverseHovers } from "./support/graphMeasurements";

beforeAll(installGraph);
beforeEach(() => { vi.useFakeTimers(); resetGraphState(); });
afterEach(() => { vi.clearAllTimers(); vi.useRealTimers(); });
afterAll(() => { state.cores.forEach((core) => core.destroy()); vi.unstubAllGlobals(); });

test("tooltip listeners stay delegated and constant across graph sizes", () => {
	const counts = [10, 100, 500].map(listenerCounts);
	for (const count of counts) {
		expect(count.perNode, `${count.cy.nodes().length} nodes`).toBe(0);
		expect(count.core, `${count.cy.nodes().length} nodes`).toBeLessThanOrEqual(10);
		expect(count.core + count.perNode).toBe(counts[0].core + counts[0].perNode);
	}
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
