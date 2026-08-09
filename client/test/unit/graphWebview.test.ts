import { afterAll, beforeAll, beforeEach, suite, test, vi } from "vitest";
import * as assert from "assert";

// The webview module runs at import time: it grabs the DOM, calls
// acquireVsCodeApi(), registers a window message listener, and posts "ready".
// Those boundaries are stubbed before the dynamic import below, and cytoscape
// and friends are mocked so rendering runs against a minimal fake core.

const { fakeCy, messageListener, postMessage, setState } = vi.hoisted(() => {
	const messageListener: {
		listener?: (event: { data: unknown }) => void;
	} = {};
	const fakeCollection = () => ({
		boundingBox: () => ({ y2: 0 }),
		layout: () => ({ run: () => {} }),
		shift: () => {},
		union: () => fakeCollection(),
	});
	const fakeCy = () => ({
		add: vi.fn(),
		collection: () => fakeCollection(),
		cyCanvas: () => ({
			clear: vi.fn(),
			getCanvas: () => ({ getContext: () => ({}) }),
			resetTransform: vi.fn(),
			setTransform: vi.fn(),
		}),
		destroy: vi.fn(),
		elements: () => ({ components: () => [] }),
		fit: vi.fn(),
		height: () => 600,
		json: vi.fn(),
		nodes: () => ({ forEach: () => {} }),
		on: vi.fn(),
		style: vi.fn(),
		width: () => 800,
	});
	return {
		fakeCy,
		messageListener,
		postMessage: vi.fn(),
		setState: vi.fn(),
	};
});

vi.mock("cytoscape", () => ({
	default: Object.assign(
		vi.fn(() => fakeCy()),
		{ use: vi.fn() },
	),
}));
vi.mock("cytoscape-elk", () => ({ default: {} }));
vi.mock("cytoscape-popper", () => ({ default: {} }));
vi.mock("tippy.js", () => ({ default: vi.fn() }));
vi.mock("merge-images", () => ({ default: vi.fn() }));
vi.mock("../../webview/canvas", () => ({ registerCytoscapeCanvas: vi.fn() }));

const graphNode = {
	id: "a",
	name: "A",
	isPrimary: true,
	entityType: "idea",
	location: { filename: "a.txt", line: 1, column: 0 },
	references: [],
};

suite("graph webview", () => {
	beforeAll(async () => {
		vi.stubGlobal("document", {
			documentElement: { style: { getPropertyValue: () => "" } },
			getElementById: () => ({ replaceChildren: () => {} }),
		});
		vi.stubGlobal("window", {
			addEventListener: (
				_type: string,
				listener: (event: { data: unknown }) => void,
			) => {
				messageListener.listener = listener;
			},
		});
		vi.stubGlobal("acquireVsCodeApi", () => ({ postMessage, setState }));

		await import("../../webview/graph");

		// The script announces itself the moment it loads.
		assert.deepStrictEqual(postMessage.mock.calls, [[{ command: "ready" }]]);
	});

	afterAll(() => {
		vi.unstubAllGlobals();
	});

	beforeEach(() => {
		setState.mockClear();
	});

	const render = (message: unknown) =>
		messageListener.listener?.({ data: message });

	test("persists the request parameters when a server graph renders", () => {
		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
			persist: { source: "server", entityType: "idea", depth: 3 },
		});

		assert.deepStrictEqual(setState.mock.calls, [
			[{ source: "server", entityType: "idea", depth: 3 }],
		]);
	});

	test("persists the import source when a JSON graph renders", () => {
		render({
			command: "importJson",
			json: '{"elements":{}}',
			settings: { wheelSensitivity: 1 },
			persist: { source: "json" },
		});

		assert.deepStrictEqual(setState.mock.calls, [[{ source: "json" }]]);
	});

	test("does not persist when the message carries no request parameters", () => {
		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});

		assert.deepStrictEqual(setState.mock.calls, []);
	});

	test("does not persist a JSON import that fails to parse", () => {
		render({
			command: "importJson",
			json: "{not json",
			settings: { wheelSensitivity: 1 },
			persist: { source: "json" },
		});

		// A broken import leaves the graph empty, so claiming "json" state
		// would make the reload serializer prompt for a file that never
		// rendered.
		assert.deepStrictEqual(setState.mock.calls, []);
	});
});
