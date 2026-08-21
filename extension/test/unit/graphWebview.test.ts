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
	destroy: () => void;
	setProps: (props: FakeTippyProps) => void;
}

const {
	added,
	createdTags,
	fakeCy,
	graphNodes,
	makeNode,
	messageListener,
	postMessage,
	setState,
	tippy,
	tippyInstances,
} = vi.hoisted(() => {
	const messageListener: {
		listener?: (event: { data: unknown }) => void;
	} = {};
	const added: FakeElementDefinition[] = [];
	const createdTags: string[] = [];
	const tippyInstances: FakeTippyInstance[] = [];
	const graphNodes: { nodes: FakeGraphNode[] } = { nodes: [] };
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
	const fakeCy = () => ({
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
		elements: () => ({ components: () => [] }),
		fit: vi.fn(),
		height: () => 600,
		json: vi.fn(),
		nodes: () => ({
			forEach: (fn: (node: FakeGraphNode) => void) => {
				graphNodes.nodes.forEach(fn);
			},
		}),
		on: vi.fn(),
		style: vi.fn(),
		width: () => 800,
	});
	return {
		added,
		createdTags,
		fakeCy,
		graphNodes,
		makeNode,
		messageListener,
		postMessage: vi.fn(),
		setState: vi.fn(),
		tippy,
		tippyInstances,
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
vi.mock("merge-images", () => ({ default: vi.fn() }));
vi.mock("../../src/webview/canvas", () => ({ registerCytoscapeCanvas: vi.fn() }));

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
	beforeAll(async () => {
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
		vi.stubGlobal("acquireVsCodeApi", () => ({ postMessage, setState }));

		await import("../../src/webview/graph");

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
		setState.mockClear();
		tippy.mockClear();
		added.length = 0;
		createdTags.length = 0;
		tippyInstances.length = 0;
		graphNodes.nodes = [];
	});

	afterEach(() => {
		vi.useRealTimers();
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
