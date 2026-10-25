import { afterAll, beforeAll, expect, test, vi } from "vitest";
import { installGraph, node, render, resetGraphState, state } from "./support/graphHeadless";
import { measuredRender } from "./support/graphMeasurements";

beforeAll(installGraph);
afterAll(() => { state.cores.forEach((core) => core.destroy()); vi.unstubAllGlobals(); });

test.each([100, 200, 500])("partition copying stays linear for %i mostly isolated nodes", (size) => {
	const measurement = measuredRender(size);
	expect(measurement.unions).toBe(0);
	expect(measurement.degreeCalls).toBe(size);
	expect(measurement.differenceInputs).toBeLessThanOrEqual(2 * measurement.cy.elements().length);
});

test("partition preserves all edges, self-loops and connected components", () => {
	resetGraphState();
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
