import {
	afterAll,
	afterEach,
	beforeAll,
	beforeEach,
	suite,
	test,
	vi,
} from "vitest";
import * as assert from "assert";
import type * as graphModule from "../../src/webview/graph";

// The webview module runs at import time: it grabs the DOM, calls
// acquireVsCodeApi(), registers a window message listener, and posts "ready".
// Those boundaries are stubbed before the dynamic import below, and cytoscape
// and friends are mocked so rendering runs against a minimal fake core.

interface FakeElement {
	tagName: string;
	className: string;
	textContent: string;
	appendChild(child: unknown): void;
	cloneNode(deep?: boolean): FakeElement;
}

interface FakeElementDefinition {
	group: "nodes" | "edges";
	data: { source?: string; target?: string; label?: string };
}

interface FakeGraphNode {
	data(key: string): unknown;
	on(event: string, handler: () => void): void;
	popperRef(): { getBoundingClientRect: () => Record<string, number> };
	handlers: Map<string, () => void>;
}

interface FakeTippyProps {
	content: () => FakeElement;
	onHidden?: (instance: FakeTippyInstance) => void;
}

interface FakeTippyInstance {
	props: FakeTippyProps;
	show: () => void;
	hide: () => void;
	destroy: ReturnType<typeof vi.fn>;
	setProps: (props: FakeTippyProps) => void;
}

interface FakeCytoscapeCore {
	destroy: ReturnType<typeof vi.fn>;
	json: ReturnType<typeof vi.fn>;
	png: ReturnType<typeof vi.fn>;
}

interface FakeOverlayCanvas {
	width: number;
	height: number;
	ctx: {
		scale: ReturnType<typeof vi.fn>;
		translate: ReturnType<typeof vi.fn>;
	};
	convertToBlob: ReturnType<typeof vi.fn>;
}

const {
	added,
	bounds,
	createdTags,
	cytoscapeCores,
	fakeCy,
	FileReader,
	graphNodes,
	jsonFailure,
	makeNode,
	mergeImages,
	messageListener,
	MutationObserver,
	offscreenCanvases,
	pngSignature,
	GRAPH_BODY,
	OVERLAY_BODY,
	MERGED_BODY,
	postMessage,
	setState,
	styleUpdate,
	themeObservers,
	tippy,
	tippyInstances,
	OffscreenCanvas,
} = vi.hoisted(() => {
	const messageListener: {
		listener?: (event: { data: unknown }) => void;
	} = {};
	const added: FakeElementDefinition[] = [];
	const createdTags: string[] = [];
	const tippyInstances: FakeTippyInstance[] = [];
	const cytoscapeCores: FakeCytoscapeCore[] = [];
	const styleUpdate = vi.fn();
	const themeObservers: Array<{
		callback: () => void;
		observe: ReturnType<typeof vi.fn>;
		disconnect: ReturnType<typeof vi.fn>;
	}> = [];
	const graphNodes: { nodes: FakeGraphNode[] } = { nodes: [] };
	const jsonFailure: { error?: Error } = {};
	const makeNode = (id: string): FakeGraphNode => {
		const handlers = new Map<string, () => void>();
		const data: Record<string, unknown> = {
			id,
			entityTypeDisplayName: "Idea",
			details: [{ key: "cost", values: ["10"] }],
		};
		return {
			data: (key: string) => data[key],
			on: (event: string, handler: () => void) => {
				handlers.set(event, handler);
			},
			popperRef: () => ({ getBoundingClientRect: () => ({}) }),
			handlers,
		};
	};
	const pngSignature = Buffer.from([
		0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
	]);
	const GRAPH_BODY =
		"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
	const OVERLAY_BODY = Buffer.concat([
		pngSignature,
		Buffer.from("overlay"),
	]).toString("base64");
	const MERGED_BODY = Buffer.concat([
		pngSignature,
		Buffer.from("merged"),
	]).toString("base64");
	const bounds = { x1: 0, y1: 0, x2: 0, y2: 0 };
	const offscreenCanvases: FakeOverlayCanvas[] = [];
	const OffscreenCanvas = class {
		constructor(width: number, height: number) {
			const record: FakeOverlayCanvas = {
				width: width,
				height: height,
				ctx: { scale: vi.fn(), translate: vi.fn() },
				convertToBlob: vi.fn(() =>
					Promise.resolve(
						new Blob([Buffer.from(OVERLAY_BODY, "base64")], {
							type: "image/png",
						}),
					),
				),
			};
			offscreenCanvases.push(record);
			Object.assign(this, record);
		}
		getContext(_type: string): unknown {
			return offscreenCanvases[offscreenCanvases.length - 1]?.ctx;
		}
	};
	const FileReader = class {
		result = "";
		onloadend: (() => void) | null = null;
		onerror: (() => void) | null = null;
		async readAsDataURL(blob: Blob): Promise<void> {
			try {
				const bytes = Buffer.from(await blob.arrayBuffer());
				this.result = `data:${blob.type || "application/octet-stream"};base64,${bytes.toString("base64")}`;
				this.onloadend?.();
			} catch {
				this.onerror?.();
			}
		}
	};
	const mergeImages = vi.fn();

	// tippy v6 evaluates the content prop eagerly, both when the instance is
	// created and on every setProps (tippy.cjs.js evaluateProps, called from
	// createTippy and setProps). The fake does the same, so these tests see the
	// real call sequence rather than one invented by the test.
	const tippy = vi.fn((_reference: unknown, props: FakeTippyProps) => {
		props.content();
		const instance: FakeTippyInstance = {
			props,
			show: vi.fn(),
			hide: vi.fn(),
			destroy: vi.fn(),
			setProps: (next: FakeTippyProps) => {
				next.content();
				instance.props = next;
			},
		};
		tippyInstances.push(instance);
		return instance;
	});
	const fakeCollection = () => ({
		boundingBox: () => ({ y2: 0 }),
		layout: () => ({ run: () => {} }),
		shift: () => {},
		union: () => fakeCollection(),
	});
	const fakeCy = () => {
		const cy = {
			add: (elements: FakeElementDefinition[]) => {
				added.push(...elements);
			},
			collection: () => fakeCollection(),
			cyCanvas: () => ({
				clear: vi.fn(),
				getCanvas: () => ({ getContext: () => ({}) }),
				resetTransform: vi.fn(),
				setTransform: vi.fn(),
			}),
			destroy: vi.fn(),
			elements: () => ({
				components: () => [],
				boundingBox: () => ({
					x1: bounds.x1,
					y1: bounds.y1,
					x2: bounds.x2,
					y2: bounds.y2,
				}),
			}),
			fit: vi.fn(),
			height: () => 600,
			json: vi.fn((json?: unknown) => {
				if (json !== undefined && jsonFailure.error) {
					throw jsonFailure.error;
				}
				return { elements: { nodes: [], edges: [] } };
			}),
			png: vi.fn(() => `data:image/png;base64,${GRAPH_BODY}`),
			nodes: () => ({
				forEach: (fn: (node: FakeGraphNode) => void) => {
					graphNodes.nodes.forEach(fn);
				},
			}),
			on: vi.fn(),
			style: vi.fn(() => ({ update: styleUpdate })),
			width: () => 800,
		};
		cytoscapeCores.push(cy);
		return cy;
	};
	class FakeMutationObserver {
		callback: () => void;
		observe = vi.fn();
		disconnect = vi.fn();

		constructor(callback: () => void) {
			this.callback = callback;
			themeObservers.push(this);
		}
	}
	const MutationObserver = vi.fn(FakeMutationObserver);
	return {
		added,
		bounds,
		createdTags,
		cytoscapeCores,
		fakeCy,
		FileReader,
		graphNodes,
		jsonFailure,
		makeNode,
		mergeImages,
		messageListener,
		MutationObserver,
		offscreenCanvases,
		pngSignature,
		GRAPH_BODY,
		OVERLAY_BODY,
		MERGED_BODY,
		postMessage: vi.fn(),
		setState: vi.fn(),
		styleUpdate,
		themeObservers,
		tippy,
		tippyInstances,
		OffscreenCanvas,
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
vi.mock("tippy.js", () => ({ default: tippy }));
vi.mock("merge-images", () => ({ default: mergeImages }));
vi.mock("../../src/webview/canvas", () => ({
	registerCytoscapeCanvas: vi.fn(),
}));

const createElement = (tagName: string): FakeElement => {
	createdTags.push(tagName);
	const element: FakeElement = {
		tagName,
		className: "",
		textContent: "",
		appendChild: () => {},
		cloneNode: () => element,
	};
	return element;
};

const graphNode = {
	id: "a",
	name: "A",
	isPrimary: true,
	entityType: "idea",
	location: { filename: "a.txt", line: 1, column: 0 },
	references: [],
};

suite("graph webview", () => {
	let graph: typeof graphModule;

	beforeAll(async () => {
		vi.stubGlobal("OffscreenCanvas", OffscreenCanvas);
		vi.stubGlobal("FileReader", FileReader);
		vi.stubGlobal("document", {
			documentElement: { style: { getPropertyValue: () => "" } },
			getElementById: () => ({ replaceChildren: () => {} }),
			createElement,
			createTextNode: (text: string) => ({ textContent: text }),
		});
		vi.stubGlobal("window", {
			addEventListener: (
				_type: string,
				listener: (event: { data: unknown }) => void,
			) => {
				messageListener.listener = listener;
			},
		});
		vi.stubGlobal("MutationObserver", MutationObserver);
		vi.stubGlobal("acquireVsCodeApi", () => ({ postMessage, setState }));

		graph = await import("../../src/webview/graph");

		// The script announces itself the moment it loads.
		assert.deepStrictEqual(postMessage.mock.calls, [[{ command: "ready" }]]);
	});

	afterAll(() => {
		vi.unstubAllGlobals();
	});

	// Fake timers for every test, not just the ones that advance the clock: a
	// hover schedules the tooltip's 1s expand timer, and a real one left running
	// fires inside whichever test happens to be executing a second later and
	// builds a detail table into that test's createdTags.
	beforeEach(() => {
		vi.useFakeTimers();
		postMessage.mockClear();
		setState.mockClear();
		tippy.mockClear();
		styleUpdate.mockClear();
		themeObservers.length = 0;
		added.length = 0;
		createdTags.length = 0;
		tippyInstances.length = 0;
		cytoscapeCores.length = 0;
		graphNodes.nodes = [];
		jsonFailure.error = undefined;
		bounds.x1 = 0;
		bounds.y1 = 0;
		bounds.x2 = 0;
		bounds.y2 = 0;
		offscreenCanvases.length = 0;
		mergeImages.mockClear();
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	const render = (message: unknown) =>
		messageListener.listener?.({ data: message });

	function clearGraph() {
		jsonFailure.error = new Error("clear graph");
		render({
			command: "importJson",
			json: '{"elements":{"nodes":[{"data":{"id":"clear"}}]}}',
			settings: { wheelSensitivity: 1 },
		});
		jsonFailure.error = undefined;
		postMessage.mockClear();
	}

	test("reports unavailable exports before a graph renders", async () => {
		clearGraph();

		await assert.doesNotReject(() => graph.exportImage(1));
		assert.doesNotThrow(() => graph.exportJson());

		assert.deepStrictEqual(postMessage.mock.calls, [
			[
				{
					command: "showError",
					message: "CWTools: no graph is available to export as an image.",
				},
			],
			[
				{
					command: "showError",
					message: "CWTools: no graph is available to export as a JSON file.",
				},
			],
		]);
	});

	test("saves the merged image with the data-URI prefix stripped", async () => {
		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});
		const cy = cytoscapeCores[0];
		assert.ok(cy);
		bounds.x1 = -40;
		bounds.y1 = -25;
		bounds.x2 = 120;
		bounds.y2 = 75;
		mergeImages.mockResolvedValueOnce(`data:image/png;base64,${MERGED_BODY}`);

		await graph.exportImage(1);

		assert.deepStrictEqual(cy.png.mock.calls, [
			[{ full: true, output: "base64uri", scale: 1 }],
		]);
		const canvas = offscreenCanvases[0];
		assert.ok(canvas);
		assert.strictEqual(canvas.width, 160);
		assert.strictEqual(canvas.height, 100);
		assert.deepStrictEqual(canvas.ctx.scale.mock.calls, [[1, 1]]);
		assert.deepStrictEqual(canvas.ctx.translate.mock.calls, [[40, 25]]);
		assert.deepStrictEqual(canvas.convertToBlob.mock.calls, [[{ type: "png" }]]);
		assert.deepStrictEqual(mergeImages.mock.calls, [
			[
				[
					`data:image/png;base64,${GRAPH_BODY}`,
					`data:image/png;base64,${OVERLAY_BODY}`,
				],
			],
		]);
		assert.deepStrictEqual(postMessage.mock.calls, [
			[{ command: "saveImage", image: MERGED_BODY }],
		]);
		const posted = (postMessage.mock.calls[0][0] as { image: string }).image;
		assert.ok(
			Buffer.from(posted, "base64").subarray(0, 8).equals(pngSignature),
			"the posted body must decode to PNG bytes",
		);
	});

	test("scales the overlay canvas by the export pixel ratio", async () => {
		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});
		const cy = cytoscapeCores[0];
		assert.ok(cy);
		bounds.x1 = -40;
		bounds.y1 = -25;
		bounds.x2 = 120;
		bounds.y2 = 75;
		mergeImages.mockResolvedValueOnce(`data:image/png;base64,${MERGED_BODY}`);

		await graph.exportImage(2);

		assert.deepStrictEqual(cy.png.mock.calls, [
			[{ full: true, output: "base64uri", scale: 2 }],
		]);
		const canvas = offscreenCanvases[0];
		assert.ok(canvas);
		assert.strictEqual(canvas.width, 320);
		assert.strictEqual(canvas.height, 200);
		assert.deepStrictEqual(canvas.ctx.scale.mock.calls, [[2, 2]]);
		assert.deepStrictEqual(canvas.ctx.translate.mock.calls, [[40, 25]]);
	});

	test("repaints the graph when VS Code changes its theme", () => {
		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});

		const observer = themeObservers[themeObservers.length - 1];
		assert.ok(observer);
		assert.strictEqual(observer.observe.mock.calls.length, 1);
		const elementCount = added.length;

		observer.callback();

		assert.strictEqual(styleUpdate.mock.calls.length, 1);
		assert.strictEqual(added.length, elementCount);
	});

	test("disconnects the previous theme observer when rebuilding the graph", () => {
		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});
		const previous = themeObservers[themeObservers.length - 1];

		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});
		const current = themeObservers[themeObservers.length - 1];

		assert.ok(previous);
		assert.ok(current);
		assert.notStrictEqual(previous, current);
		assert.strictEqual(previous.disconnect.mock.calls.length, 1);
		assert.strictEqual(current.disconnect.mock.calls.length, 0);
	});

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

	test("imports and persists a saved Cytoscape graph", () => {
		render({
			command: "importJson",
			json: '{"elements":{"nodes":[{"data":{"id":"saved"}}],"edges":[]}}',
			settings: { wheelSensitivity: 1 },
			persist: { source: "json", fileName: "saved-graph.json" },
		});

		assert.deepStrictEqual(setState.mock.calls, [
			[{ source: "json", fileName: "saved-graph.json" }],
		]);
		assert.deepStrictEqual(cytoscapeCores[0]?.json.mock.calls, [
			[
				{
					elements: { nodes: [{ data: { id: "saved" } }], edges: [] },
				},
			],
		]);
	});

	test("does not persist when the message carries no request parameters", () => {
		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});

		assert.deepStrictEqual(setState.mock.calls, []);
	});

	test("keeps the current graph when a named JSON import fails to parse", () => {
		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});
		const current = cytoscapeCores[0];
		const observer = themeObservers[themeObservers.length - 1];
		assert.ok(current);
		assert.ok(observer);

		render({
			command: "importJson",
			json: "{not json",
			settings: { wheelSensitivity: 1 },
			persist: { source: "json", fileName: "broken-graph.json" },
		});

		assert.deepStrictEqual(setState.mock.calls, []);
		assert.deepStrictEqual(postMessage.mock.calls, [
			[
				{
					command: "showError",
					message:
						"CWTools: couldn't import \"broken-graph.json\": it isn't valid Cytoscape graph JSON.",
				},
			],
		]);
		assert.strictEqual(cytoscapeCores.length, 1);
		assert.strictEqual(current.destroy.mock.calls.length, 0);
		assert.strictEqual(observer.disconnect.mock.calls.length, 0);

		graph.exportJson();
		assert.deepStrictEqual(current.json.mock.calls, [[]]);
		assert.deepStrictEqual(postMessage.mock.calls[1], [
			{
				command: "saveJson",
				json: '{"elements":{"nodes":[],"edges":[]}}',
			},
		]);
	});

	test("rejects missing-ID elements before replacing a current graph", () => {
		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});
		const current = cytoscapeCores[0];
		const observer = themeObservers[themeObservers.length - 1];
		assert.ok(current);
		assert.ok(observer);

		render({
			command: "importJson",
			json: '{"elements":[{"data":{}}]}',
			settings: { wheelSensitivity: 1 },
			persist: { source: "json", fileName: "missing-id.json" },
		});

		assert.deepStrictEqual(setState.mock.calls, []);
		assert.deepStrictEqual(postMessage.mock.calls, [
			[
				{
					command: "showError",
					message:
						"CWTools: couldn't import \"missing-id.json\": it isn't valid Cytoscape graph JSON.",
				},
			],
		]);
		assert.strictEqual(cytoscapeCores.length, 1);
		assert.strictEqual(current.destroy.mock.calls.length, 0);
		assert.strictEqual(observer.disconnect.mock.calls.length, 0);
	});

	test("cleans up a rejected Cytoscape replacement before exports", async () => {
		const node = makeNode("a");
		graphNodes.nodes = [node];
		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});
		node.handlers.get("mouseover")?.();
		const previous = cytoscapeCores[0];
		const observer = themeObservers[themeObservers.length - 1];
		const tip = tippyInstances[0];
		assert.ok(previous);
		assert.ok(observer);
		assert.ok(tip);

		jsonFailure.error = new Error("Cytoscape rejected import");
		render({
			command: "importJson",
			json: '{"elements":{"nodes":[{"data":{"id":"replacement"}}]}}',
			settings: { wheelSensitivity: 1 },
			persist: { source: "json", fileName: "rejected.json" },
		});

		assert.strictEqual(previous.destroy.mock.calls.length, 1);
		assert.strictEqual(observer.disconnect.mock.calls.length, 1);
		assert.strictEqual(tip.destroy.mock.calls.length, 1);
		assert.strictEqual(cytoscapeCores[1]?.destroy.mock.calls.length, 1);
		assert.deepStrictEqual(setState.mock.calls, []);

		await assert.doesNotReject(() => graph.exportImage(1));
		assert.doesNotThrow(() => graph.exportJson());
		assert.deepStrictEqual(postMessage.mock.calls, [
			[
				{
					command: "showError",
					message:
						"CWTools: couldn't import \"rejected.json\": it isn't valid Cytoscape graph JSON.",
				},
			],
			[
				{
					command: "showError",
					message: "CWTools: no graph is available to export as an image.",
				},
			],
			[
				{
					command: "showError",
					message: "CWTools: no graph is available to export as a JSON file.",
				},
			],
		]);
	});

	test("builds no tooltip DOM while rendering the graph", () => {
		graphNodes.nodes = [makeNode("a"), makeNode("b")];

		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});

		assert.deepStrictEqual(createdTags, []);
		assert.strictEqual(tippy.mock.calls.length, 0);
	});

	test("builds the header on hover, without the detail table", () => {
		const node = makeNode("a");
		graphNodes.nodes = [node];

		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});
		node.handlers.get("mouseover")?.();

		assert.strictEqual(tippyInstances.length, 1);
		assert.ok(createdTags.includes("strong"));
		assert.ok(!createdTags.includes("table"));
	});

	test("builds the detail table only once the hover expands the tooltip", () => {
		const node = makeNode("a");
		graphNodes.nodes = [node];

		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});
		node.handlers.get("mouseover")?.();
		assert.ok(!createdTags.includes("table"));

		// One tick short of the expand timeout the table must still be absent, or
		// the assertion above would pass for the wrong reason.
		vi.advanceTimersByTime(999);
		assert.ok(!createdTags.includes("table"));

		vi.advanceTimersByTime(1);
		assert.deepStrictEqual(
			createdTags.filter((tag) => tag === "table" || tag === "td"),
			["table", "td", "td"],
		);
	});

	test("a hover that ends before the timeout builds no detail table", () => {
		const node = makeNode("a");
		graphNodes.nodes = [node];

		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});
		node.handlers.get("mouseover")?.();
		node.handlers.get("mouseout")?.();
		vi.advanceTimersByTime(5000);

		assert.ok(!createdTags.includes("table"));
	});

	test("reuses the built tooltip rather than rebuilding it per render", () => {
		const node = makeNode("a");
		graphNodes.nodes = [node];

		render({
			command: "go",
			data: [graphNode],
			settings: { wheelSensitivity: 1 },
		});
		node.handlers.get("mouseover")?.();
		vi.advanceTimersByTime(1000);
		const instance = tippyInstances[0];
		createdTags.length = 0;

		// Collapsing back to the simple tooltip re-renders it, which is where a
		// dropped memo would show up as a second header build.
		instance.props.onHidden?.(instance);

		assert.deepStrictEqual(createdTags, ["div"]);
	});

	const edgesOf = () =>
		added
			.filter((element) => element.group === "edges")
			.map((element) => [
				element.data.source,
				element.data.target,
				element.data.label,
			]);

	const nodeWith = (id: string, references: unknown[]) => ({
		...graphNode,
		id,
		references,
	});

	test("collapses references that repeat the same source, target and label", () => {
		render({
			command: "go",
			settings: { wheelSensitivity: 1 },
			data: [
				nodeWith("a", [
					{ key: "b", isOutgoing: true, label: "needs" },
					{ key: "b", isOutgoing: true, label: "needs" },
				]),
				nodeWith("b", []),
			],
		});

		assert.deepStrictEqual(edgesOf(), [["a", "b", "needs"]]);
	});

	test("keeps two references between the same pair when the labels differ", () => {
		render({
			command: "go",
			settings: { wheelSensitivity: 1 },
			data: [
				nodeWith("a", [
					{ key: "b", isOutgoing: true, label: "needs" },
					{ key: "b", isOutgoing: true, label: "unlocks" },
				]),
				nodeWith("b", []),
			],
		});

		assert.deepStrictEqual(edgesOf(), [
			["a", "b", "needs"],
			["a", "b", "unlocks"],
		]);
	});

	test("reverses the endpoints of an incoming reference", () => {
		render({
			command: "go",
			settings: { wheelSensitivity: 1 },
			data: [
				nodeWith("a", [{ key: "b", isOutgoing: false, label: "needs" }]),
				nodeWith("b", []),
			],
		});

		assert.deepStrictEqual(edgesOf(), [["b", "a", "needs"]]);
	});

	test("treats an outgoing and an incoming reference as distinct edges", () => {
		render({
			command: "go",
			settings: { wheelSensitivity: 1 },
			data: [
				nodeWith("a", [
					{ key: "b", isOutgoing: true, label: "" },
					{ key: "b", isOutgoing: false, label: "" },
				]),
				nodeWith("b", []),
			],
		});

		assert.deepStrictEqual(edgesOf(), [
			["a", "b", ""],
			["b", "a", ""],
		]);
	});

	test("keeps edges distinct when the ids and labels contain delimiters", () => {
		// The dedup key joins three fields into one string. A printable separator
		// would let ("a", "b", "c|d") and ("a", "b|c", "d") collapse into one edge.
		render({
			command: "go",
			settings: { wheelSensitivity: 1 },
			data: [
				nodeWith("a", [
					{ key: "b", isOutgoing: true, label: "c|d" },
					{ key: "b|c", isOutgoing: true, label: "d" },
				]),
				nodeWith("b", []),
				nodeWith("b|c", []),
			],
		});

		assert.deepStrictEqual(edgesOf(), [
			["a", "b", "c|d"],
			["a", "b|c", "d"],
		]);
	});

	test("renders a node whose references the server omitted", () => {
		const node: Record<string, unknown> = { ...graphNode };
		delete node.references;

		render({
			command: "go",
			data: [node],
			settings: { wheelSensitivity: 1 },
		});

		assert.deepStrictEqual(edgesOf(), []);
	});

	test("drops a reference to an id that is not in the graph", () => {
		render({
			command: "go",
			settings: { wheelSensitivity: 1 },
			data: [nodeWith("a", [{ key: "gone", isOutgoing: true, label: "" }])],
		});

		assert.deepStrictEqual(edgesOf(), []);
	});
});
