import type cytoscape from "cytoscape";
import { afterAll, afterEach, beforeAll, beforeEach, expect, test, vi } from "vitest";
import type { GraphNode } from "../../src/common/graphTypes";
import { completeLayout, installGraph, node, render, resetGraphState, state } from "./support/graphHeadless";

beforeAll(installGraph);
beforeEach(() => { vi.useFakeTimers(); resetGraphState(); });
afterEach(() => { vi.restoreAllMocks(); vi.clearAllTimers(); vi.useRealTimers(); });
afterAll(() => { state.cores.forEach((core) => core.destroy()); vi.unstubAllGlobals(); });

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
	// Place both nodes inside the viewport; the pan shift alone leaves them
	// above the top edge, where the render cull would skip them.
	const extent = cy.extent();
	cy.batch(() => {
		cy.$id("a").position({ x: extent.x1 + 100, y: extent.y1 + 100 });
		cy.$id("b").position({ x: extent.x1 + 140, y: extent.y1 + 100 });
	});
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

test("waits for ELK before laying out isolated nodes and fitting the graph", () => {
	const cy = render([node("isolated"), node("connected", ["target"]), node("target")], { completeLayouts: false });
	const fit = vi.spyOn(Object.getPrototypeOf(cy) as { fit: cytoscape.Core["fit"] }, "fit");

	expect(state.layouts.map((layout) => layout.name)).toEqual(["elk"]);
	expect(fit).not.toHaveBeenCalled();

	completeLayout("elk");

	expect(state.layouts.map((layout) => layout.name)).toEqual(["elk", "grid"]);
	expect(state.layouts.find((layout) => layout.name === "grid")?.ids).toEqual(["isolated"]);
	expect(state.layoutEvents).toEqual(["elk:run", "elk:stop", "grid:run"]);
	expect(fit).toHaveBeenCalledTimes(1);
});

test("reports a synchronous layout failure to the extension host", () => {
	state.layoutFailure = new Error("ELK failed");
	render([node("isolated"), node("connected", ["target"]), node("target")], { completeLayouts: false });

	expect(state.postMessage).toHaveBeenCalledWith({
		command: "showError",
		message: "CWTools: couldn't lay out graph: ELK failed.",
	});
	expect(state.layouts.map((layout) => layout.name)).toEqual(["grid"]);
});

test("reports an ELK layout that does not complete and accepts a late completion", () => {
	const timerSpy = vi.spyOn(globalThis, "setTimeout");
	const cy = render([node("isolated"), node("connected", ["target"]), node("target")], { completeLayouts: false });
	const fit = vi.spyOn(cy, "fit");
	const timeout = timerSpy.mock.calls.find(([, delay]) => delay === 60_000)?.[0];
	expect(timeout).toBeDefined();
	(timeout as () => void)();
	timerSpy.mockRestore();
	vi.clearAllTimers();

	expect(state.postMessage).toHaveBeenCalledWith({
		command: "showError",
		message: "CWTools: couldn't lay out graph: ELK layout did not finish within 60 seconds.",
	});
	expect(state.layouts.map((layout) => layout.name)).toEqual(["elk", "grid"]);
	expect(fit).toHaveBeenCalledTimes(1);

	completeLayout("elk");
	expect(fit).toHaveBeenCalledTimes(2);
});

test("discarded layout callbacks cannot finalize a replacement graph", () => {
	const oldCy = render([node("old", ["oldTarget"]), node("oldTarget")], { completeLayouts: false });
	const oldOwner = state.cores.length - 1;
	const oldFit = vi.spyOn(oldCy, "fit");
	const newCy = render([node("newIsolated"), node("new", ["newTarget"]), node("newTarget")], { completeLayouts: false });
	const newOwner = state.cores.length - 1;
	const newFit = vi.spyOn(newCy, "fit");

	completeLayout("elk", oldOwner);

	expect(state.postMessage).not.toHaveBeenCalled();
	expect(state.layoutCallbacks.some((layout) => layout.owner === oldOwner && layout.active)).toBe(false);
	expect(oldFit).not.toHaveBeenCalled();
	expect(newFit).not.toHaveBeenCalled();

	completeLayout("elk", newOwner);

	expect(state.layouts.filter((layout) => layout.name === "grid").map((layout) => layout.owner)).toEqual([newOwner]);
	expect(newFit).toHaveBeenCalledTimes(1);
});

test("real hover creates tooltips lazily and replacement destroys them", () => {
	const cy = render([{ ...node("a"), details: [{ key: "cost", values: ["10", "20"] }] }, node("b")]);
	expect(state.tips).toHaveLength(0);
	cy.$id("a").emit("mouseover");
	expect(state.tips).toHaveLength(1);
	const tip = state.tips[0];
	expect(tip.show).toHaveBeenCalledTimes(1);
	const simpleContent = tip.props.content;
	const setProps = vi.spyOn(tip, "setProps");
	vi.advanceTimersByTime(1000);
	expect(setProps).toHaveBeenCalledTimes(1);
	expect(setProps).toHaveBeenCalledWith(expect.objectContaining({ interactive: true }));
	expect(tip.props.content).not.toBe(simpleContent);
	const content = (tip.props.content as () => HTMLElement)();
	const table = content.children[0].children[2];
	expect(table.className).toBe("cwtools-table");
	expect(table.children[0].children[0].textContent).toBe("cost");
	expect(table.children[0].children[1].textContent).toBe("10, 20");
	cy.$id("a").emit("mouseout");
	expect(tip.hide).not.toHaveBeenCalled();
	render([node("new")]);
	expect(tip.destroy).toHaveBeenCalledTimes(1);
	expect(cy.destroyed()).toBe(true);
});

test("mouseout before expansion hides the simple tooltip and cancels detail content", () => {
	const cy = render([node("a")]);
	cy.$id("a").emit("mouseover");
	const tip = state.tips[0];
	const setProps = vi.spyOn(tip, "setProps");
	vi.advanceTimersByTime(999);
	cy.$id("a").emit("mouseout");
	expect(tip.hide).toHaveBeenCalledTimes(1);
	vi.advanceTimersByTime(1);
	expect(setProps).not.toHaveBeenCalled();
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

test("render skips nodes outside the viewport but still draws a node crossing its edge", () => {
	const cy = render([node("in"), node("crossing"), node("far")]);
	const extent = cy.extent();
	const middleY = (extent.y1 + extent.y2) / 2;
	const inX = (extent.x1 + extent.x2) / 2;
	const crossingX = extent.x2 - 10;
	const farX = extent.x2 + 40;
	cy.batch(() => {
		cy.$id("in").position({ x: inX, y: middleY });
		cy.$id("crossing").position({ x: crossingX, y: middleY });
		cy.$id("far").position({ x: farX, y: middleY });
	});
	// A node body is 30 units wide (radius 15), so "crossing" pokes past the
	// right viewport edge and "far" clears it entirely.
	state.ctx.arc.mockClear();
	cy.emit("render");
	const drawnX: number[] = state.ctx.arc.mock.calls.map((call) => call[0] as number);
	expect(drawnX).toHaveLength(2);
	expect(drawnX).toContain(inX);
	expect(drawnX).toContain(crossingX);
	expect(drawnX).not.toContain(farX);
});

test("a node just past the edge is drawn while its shadow shows and skipped once shadows are off", () => {
	const cy = render([node("edge")]);
	const edge = cy.$id("edge");
	const placeJustPastRightEdge = () => {
		const extent = cy.extent();
		const box = edge.boundingBox({ includeLabels: false });
		// The body starts 5 units past the right edge, inside the 12.5-unit
		// shadow reach at devicePixelRatio 2.
		edge.position({ x: extent.x2 + (box.x2 - box.x1) / 2 + 5, y: (extent.y1 + extent.y2) / 2 });
	};
	cy.zoom(1);
	placeJustPastRightEdge();
	state.ctx.arc.mockClear();
	cy.emit("render");
	expect(state.ctx.shadowBlur).toBe(25);
	expect(state.ctx.arc).toHaveBeenCalledTimes(1);

	cy.zoom(0.2);
	placeJustPastRightEdge();
	state.ctx.arc.mockClear();
	cy.emit("render");
	expect(state.ctx.shadowBlur).toBe(0);
	expect(state.ctx.arc).not.toHaveBeenCalled();
});

test("render culling leaves skipped nodes in the graph and undimmed", () => {
	const cy = render([node("in"), node("far")]);
	const extent = cy.extent();
	const middleY = (extent.y1 + extent.y2) / 2;
	cy.batch(() => {
		cy.$id("in").position({ x: (extent.x1 + extent.x2) / 2, y: middleY });
		cy.$id("far").position({ x: extent.x2 + 40, y: middleY });
	});
	state.ctx.arc.mockClear();
	cy.emit("render");
	expect(state.ctx.arc).toHaveBeenCalledTimes(1);
	// exportImage hands drawExtra the unfiltered cy.nodes() collection, so the
	// render pass must cull by drawing alone: nothing removed, nothing dimmed.
	expect(cy.nodes()).toHaveLength(2);
	expect(cy.$id("far").removed()).toBe(false);
	expect(cy.$id("far").hasClass("semitransp")).toBe(false);
});

const ref = (key: string, isOutgoing: boolean, label: string) => ({ key, isOutgoing, label });

test.each([
	["collapses a repeated source, target and label", [ref("b", true, "needs"), ref("b", true, "needs")], [["a", "b", "needs"]]],
	["keeps the same pair when the labels differ", [ref("b", true, "needs"), ref("b", true, "unlocks")], [["a", "b", "needs"], ["a", "b", "unlocks"]]],
	["reverses the endpoints of an incoming reference", [ref("b", false, "needs")], [["b", "a", "needs"]]],
	["treats outgoing and incoming as distinct edges", [ref("b", true, ""), ref("b", false, "")], [["a", "b", ""], ["b", "a", ""]]],
	// A printable separator in the dedup key would collapse these two into one edge.
	["keeps edges distinct when ids and labels contain delimiters", [ref("b", true, "c|d"), ref("b|c", true, "d")], [["a", "b", "c|d"], ["a", "b|c", "d"]]],
	["drops a reference to an id that is not in the graph", [ref("gone", true, "")], []],
	["renders a node whose references the server omitted", undefined, []],
])("edge building %s", (_name, references, expected) => {
	const cy = render([{ ...node("a"), references } as GraphNode, node("b"), node("b|c")]);
	expect(cy.edges().map((edge): unknown[] => [edge.data("source"), edge.data("target"), edge.data("label")])).toEqual(expected);
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
