import { afterAll, beforeAll, beforeEach, suite, test, vi } from "vitest";
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

interface FakeGraphNode {
	data(key: string): unknown;
	on(event: string, handler: () => void): void;
	popperRef(): { getBoundingClientRect: () => Record<string, number> };
	handlers: Map<string, () => void>;
}

interface FakeTippyProps {
	content: () => FakeElement;
}

interface FakeTippyInstance {
	props: FakeTippyProps;
	show: () => void;
	hide: () => void;
	destroy: () => void;
	setProps: (props: FakeTippyProps) => void;
}

const {
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
	const tippy = vi.fn((_reference: unknown, props: FakeTippyProps) => {
		const instance: FakeTippyInstance = {
			props,
			show: vi.fn(),
			hide: vi.fn(),
			destroy: vi.fn(),
			setProps: (next: FakeTippyProps) => {
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
vi.mock("../../webview/canvas", () => ({ registerCytoscapeCanvas: vi.fn() }));

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

		await import("../../webview/graph");

		// The script announces itself the moment it loads.
		assert.deepStrictEqual(postMessage.mock.calls, [[{ command: "ready" }]]);
	});

	afterAll(() => {
		vi.unstubAllGlobals();
	});

	beforeEach(() => {
		setState.mockClear();
		tippy.mockClear();
		createdTags.length = 0;
		tippyInstances.length = 0;
		graphNodes.nodes = [];
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

	test("builds the header on hover and the detail table only on expand", () => {
		const node = makeNode("a");
		graphNodes.nodes = [node];
		vi.useFakeTimers();
		try {
			render({
				command: "go",
				data: [graphNode],
				settings: { wheelSensitivity: 1 },
			});
			node.handlers.get("mouseover")?.();
			assert.strictEqual(tippyInstances.length, 1);

			// tippy only calls content() when it renders, so drive that here.
			tippyInstances[0].props.content();
			assert.ok(createdTags.includes("strong"));
			assert.ok(!createdTags.includes("table"));

			vi.advanceTimersByTime(1000);
			tippyInstances[0].props.content();
			assert.ok(createdTags.includes("table"));
		} finally {
			vi.useRealTimers();
		}
	});
});
