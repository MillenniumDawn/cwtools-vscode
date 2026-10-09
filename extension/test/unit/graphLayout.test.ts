import type cytoscape from "cytoscape";
import { afterAll, afterEach, beforeAll, beforeEach, expect, test, vi } from "vitest";
import { installGraph, node, render, resetGraphState, state } from "./support/graphHeadless";

beforeAll(installGraph);
beforeEach(() => { vi.useFakeTimers(); resetGraphState(); });
afterEach(() => { vi.clearAllTimers(); vi.useRealTimers(); });
afterAll(() => { state.cores.forEach((core) => core.destroy()); vi.unstubAllGlobals(); });

test("partitions isolated nodes and connected elements through real Cytoscape", () => {
	const cy = render([node("solo"), node("a", ["b"]), node("b"), node("loop", ["loop"])]);
	expect(cy.nodes().length).toBe(4);
	expect(cy.edges().length).toBe(2);
	const grid = state.layouts.find((l) => l.name === "grid")!;
	const elk = state.layouts.find((l) => l.name === "elk")!;
	expect(grid.ids).toEqual(["solo"]);
	expect(elk.ids).toContain("a");
	expect(elk.ids).toContain("b");
	expect(elk.ids).toContain("loop");
	expect(new Set([...grid.ids, ...elk.ids])).toEqual(new Set(cy.elements().map((e) => e.id())));
	expect(grid.ids.length + elk.ids.length).toBe(cy.elements().length);
});

test("real delegated hover highlights the closed neighborhood and restores classes", () => {
	const cy = render([node("a", ["b"]), node("b"), node("other")]);
	const selected = cy.$id("a");
	selected.emit("mouseover");
	expect(cy.$id("a").hasClass("highlight")).toBe(true);
	expect(cy.$id("b").hasClass("highlight")).toBe(true);
	expect(cy.edges()[0].hasClass("highlight")).toBe(true);
	expect(cy.$id("other").hasClass("semitransp")).toBe(true);
	expect(selected.hasClass("semitransp")).toBe(false);
	selected.emit("mouseout");
	expect(cy.$(".highlight, .semitransp").length).toBe(0);
});

test("drawExtra and the real canvas extension draw and transform a headless graph", () => {
	const cy = render([node("a"), node("b")]);
	cy.zoom(1);
	cy.pan({ x: 3, y: 4 });
	cy.emit("render");
	expect(state.ctx.clearRect).toHaveBeenCalledWith(0, 0, 1600, 1200);
	expect(state.ctx.translate).toHaveBeenCalledWith(6, 8);
	expect(state.ctx.scale).toHaveBeenCalledWith(2, 2);
	expect(state.ctx.arc).toHaveBeenCalledTimes(2);
	expect(state.ctx.fillText).toHaveBeenCalledWith("I", expect.any(Number), expect.any(Number));
	expect(cy.nodes().every((n) => n[0].scratch("_drawLabel") === "I")).toBe(true);
	expect(state.ctx.shadowBlur).toBe(25);
	cy.zoom(0.2);
	cy.emit("render");
	expect(state.ctx.shadowBlur).toBe(0);
});

test("a large graph draws each node once and disables expensive shadows", () => {
	const cy = render(Array.from({ length: 500 }, (_, i) => node(`n${i}`)));
	cy.emit("render");
	expect(state.ctx.arc).toHaveBeenCalledTimes(500);
	expect(state.ctx.shadowBlur).toBe(0);
	expect(state.layouts.find((l) => l.name === "grid")!.ids.length).toBe(500);
});

test("real hover creates tooltips lazily and replacement destroys them", () => {
	const cy = render([node("a"), node("b")]);
	expect(state.tips).toHaveLength(0);
	cy.$id("a").emit("mouseover");
	expect(state.tips).toHaveLength(1);
	expect(state.tips[0].show).toHaveBeenCalledTimes(1);
	vi.advanceTimersByTime(1000);
	cy.$id("a").emit("mouseout");
	const tip = state.tips[0];
	render([node("new")]);
	expect(tip.destroy).toHaveBeenCalledTimes(1);
	expect(cy.destroyed()).toBe(true);
});

test("hover class updates touch at most the graph size for a fixed-degree node", () => {
	const cy = render(Array.from({ length: 100 }, (_, i) => node(`n${i}`, [`n${(i + 1) % 100}`])));
	const prototype = Object.getPrototypeOf(cy.nodes()[0]) as { addClass: cytoscape.CollectionReturnValue["addClass"] };
	const original = prototype.addClass;
	let touched = 0;
	const spy = vi.spyOn(prototype, "addClass").mockImplementation(function (this: cytoscape.CollectionReturnValue, classes) {
		touched += this.length;
		return original.call(this, classes);
	});
	try {
		cy.$id("n0").emit("mouseover");
		expect(cy.$(".highlight").length).toBe(5);
		expect(touched).toBeLessThanOrEqual(cy.elements().length);
		cy.$id("n0").emit("mouseout");
		expect(cy.$(".highlight, .semitransp").length).toBe(0);
	} finally {
		spy.mockRestore();
	}
});

test("canvas resize honors explicit pixel ratio and zero z-index defaults", () => {
	const cy = render([node("a")]);
	const layer = cy.cyCanvas({ pixelRatio: "1.5", zIndex: 0 });
	const canvas = layer.getCanvas();
	expect(canvas.width).toBe(1200);
	expect(canvas.height).toBe(900);
	expect(canvas.style.width).toBe("800px");
	expect(canvas.style.zIndex).toBe("1");
	state.container.offsetWidth = 400;
	try {
		cy.emit("resize");
		expect(canvas.width).toBe(600);
	} finally {
		state.container.offsetWidth = 800;
	}
});
