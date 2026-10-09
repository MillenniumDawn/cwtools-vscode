import { afterAll, afterEach, beforeAll, beforeEach, expect, test, vi } from "vitest";
import { installGraph, node, render, resetGraphState, send, state } from "./support/graphHeadless";

beforeAll(installGraph);
beforeEach(() => { vi.useFakeTimers(); resetGraphState(); });
afterEach(() => { vi.clearAllTimers(); vi.useRealTimers(); });
afterAll(() => { state.cores.forEach((core) => core.destroy()); vi.unstubAllGlobals(); });

function importNode(metadata: Record<string, unknown>) {
	send({
		command: "importJson",
		json: JSON.stringify({ elements: { nodes: [{ data: { id: "focus_a", ...metadata } }] } }),
		settings: { wheelSensitivity: 1 },
		persist: { source: "json", fileName: "graph.json" },
	});
	return state.cores[state.cores.length - 1];
}

test.each([
	42, null, [null], [{ key: "cost", values: 42 }],
	[{ key: "cost", values: null }], [{ key: "cost", values: [42] }],
	[{ key: 42, values: ["10"] }],
])("rejects malformed details %j before replacing the graph", (details) => {
	const existing = render([node("existing")]);
	importNode({ details });
	expect(existing.destroyed()).toBe(false);
	expect(existing.$id("existing").length).toBe(1);
	expect(state.setState).not.toHaveBeenCalled();
	expect(state.postMessage).toHaveBeenCalledWith({
		command: "showError", message: expect.stringContaining("details") as unknown,
	});
});

test.each([
	null, { filename: "a.txt", line: 0, column: 1 },
	{ filename: "a.txt", line: 1.5, column: 1 },
	{ filename: "a.txt", line: 1, column: -1 },
	{ filename: "a.txt", line: 1, column: "1" },
	{ filename: "a.txt", line: Number.MAX_SAFE_INTEGER + 1, column: 1 },
	{ filename: "", line: 1, column: 1 },
])("rejects invalid location %j before replacing the graph", (location) => {
	const existing = render([node("existing")]);
	importNode({ location });
	expect(existing.destroyed()).toBe(false);
	expect(state.setState).not.toHaveBeenCalled();
	expect(state.postMessage).toHaveBeenCalledWith({
		command: "showError", message: expect.stringContaining("location") as unknown,
	});
});

test("generic imported nodes remain hoverable and safely non-navigable", () => {
	const cy = importNode({});
	expect(() => {
		cy.$id("focus_a").emit("mouseover");
		vi.advanceTimersByTime(1000);
		cy.$id("focus_a").emit("doubleTap");
	}).not.toThrow();
	expect(state.tips).toHaveLength(1);
	expect(state.postMessage).not.toHaveBeenCalledWith(expect.objectContaining({ command: "goToFile" }));
});

test("ordinary exported metadata still expands details and forwards navigation", () => {
	const location = { filename: "focus.txt", line: 3, column: 0 };
	const cy = importNode({ details: [{ key: "cost", values: ["10", "20"] }], location });
	cy.$id("focus_a").emit("mouseover");
	expect(() => vi.advanceTimersByTime(1000)).not.toThrow();
	cy.$id("focus_a").emit("doubleTap");
	expect(state.postMessage).toHaveBeenCalledWith({ command: "goToFile", uri: "focus.txt", line: 3, column: 0 });
	expect(state.setState).toHaveBeenCalledWith({ source: "json", fileName: "graph.json" });
});

test("an actual Cytoscape export round-trips with hover and navigation", () => {
	const graphNode = node("exported");
	graphNode.details = [{ key: "cost", values: ["10"] }];
	const original = render([graphNode]);
	const json = JSON.stringify(original.json());
	send({ command: "importJson", json, settings: { wheelSensitivity: 1 } });
	const imported = state.cores[state.cores.length - 1];
	expect(imported.$id("exported").length).toBe(1);
	expect(() => {
		imported.$id("exported").emit("mouseover");
		vi.advanceTimersByTime(1000);
		imported.$id("exported").emit("doubleTap");
	}).not.toThrow();
	expect(state.postMessage).toHaveBeenCalledWith({
		command: "goToFile", uri: "exported.txt", line: 1, column: 0,
	});
});

test("navigation validates coordinates again at the interaction boundary", () => {
	const cy = importNode({});
	cy.$id("focus_a").data("location", { filename: "a.txt", line: Infinity, column: NaN });
	expect(() => cy.$id("focus_a").emit("doubleTap")).not.toThrow();
	expect(state.postMessage).not.toHaveBeenCalledWith(expect.objectContaining({ command: "goToFile" }));
});


test.each([42, null, [], { toString: 0 }])("rejects malformed display names %j before replacement", (entityTypeDisplayName) => {
	const existing = render([node("existing")]);
	importNode({ entityTypeDisplayName });
	expect(existing.destroyed()).toBe(false);
	expect(state.setState).not.toHaveBeenCalled();
	expect(state.postMessage).toHaveBeenCalledWith({
		command: "showError", message: expect.stringContaining("entityTypeDisplayName") as unknown,
	});
});

test.each([
	{ source: "other" }, { target: "other" }, { group: "nodes", source: "other", target: "other" },
])("reserved fields do not bypass node metadata validation: %j", (reserved) => {
	const existing = render([node("existing")]);
	const { group, ...data } = reserved;
	send({
		command: "importJson",
		json: JSON.stringify({ elements: [{ group, data: { id: "focus_a", details: [{ key: "cost", values: 42 }], ...data } }] }),
		settings: { wheelSensitivity: 1 },
	});
	expect(existing.destroyed()).toBe(false);
	expect(state.postMessage).toHaveBeenCalledWith({
		command: "showError", message: expect.stringContaining("details") as unknown,
	});
});
