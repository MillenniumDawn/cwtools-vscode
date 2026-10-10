import { vi } from "vitest";
import type cytoscape from "cytoscape";
import type { GraphNode } from "../../../src/common/graphTypes";

// Keep the real collection, event, style and graph algorithms. Only the browser
// surface and asynchronous layout engine are replaced; graph.ts and canvas.ts
// execute through their normal message dispatcher.
const state = vi.hoisted(() => ({
	cores: [] as cytoscape.Core[],
	layouts: [] as { name: string; ids: string[] }[],
	listener: undefined as ((event: { data: unknown }) => void) | undefined,
	postMessage: vi.fn(),
	setState: vi.fn(),
	ctx: {
		save: vi.fn(), restore: vi.fn(), setTransform: vi.fn(),
		clearRect: vi.fn(), translate: vi.fn(), scale: vi.fn(),
		beginPath: vi.fn(), arc: vi.fn(), fill: vi.fn(), stroke: vi.fn(),
		fillText: vi.fn(), shadowBlur: 0, globalAlpha: 1,
	},
	container: {
		offsetWidth: 800, offsetHeight: 600, childNodes: [] as unknown[],
		appendChild: vi.fn(), replaceChildren: vi.fn(),
	},
	tips: [] as { show: ReturnType<typeof vi.fn>; hide: ReturnType<typeof vi.fn>; destroy: ReturnType<typeof vi.fn>; setProps: (next: Record<string, unknown>) => void; props: Record<string, unknown> }[],
}));

vi.mock("cytoscape", async (importOriginal) => {
	const actual = await importOriginal<{ default: typeof cytoscape }>();
	const real = actual.default;
	const wrapped = Object.assign((options?: cytoscape.CytoscapeOptions) => {
		const core = real({ ...options, container: undefined, headless: true, styleEnabled: true });
		core.container = () => state.container as unknown as HTMLElement;
		core.width = () => 800;
		core.height = () => 600;
		state.cores.push(core);
		return core;
	}, real);
	// canvas.ts registers an extension through the cytoscape factory itself.
	const factory = new Proxy(wrapped, {
		apply(target, thisArg, args) {
			return typeof args[0] === "string"
				? Reflect.apply(real, thisArg, args) as unknown
				: Reflect.apply(target, thisArg, args) as unknown;
		},
	});
	return { ...actual, default: factory };
});

vi.mock("cytoscape-elk", () => ({
	default: (factory: typeof cytoscape) => {
		for (const name of ["elk", "grid"]) {
			function Layout(this: { options: { eles: cytoscape.CollectionReturnValue } }, options: { eles: cytoscape.CollectionReturnValue }) {
				this.options = options;
			}
			Object.defineProperty(Layout.prototype as object, "run", {
				value: function (this: { options: { eles: cytoscape.CollectionReturnValue } }) {
					state.layouts.push({ name, ids: this.options.eles.map((e) => e.id()) });
					return this;
				},
			});
			factory("layout", name, Layout);
		}
	},
}));
vi.mock("cytoscape-popper", () => ({ default: () => {} }));
vi.mock("merge-images", () => ({ default: vi.fn() }));
vi.mock("tippy.js", () => ({
	default: (_element: unknown, props: Record<string, unknown>) => {
		(props.content as () => unknown)();
		const tip = {
			props, show: vi.fn(), hide: vi.fn(), destroy: vi.fn(),
			setProps(next: Record<string, unknown>) {
				(next.content as () => unknown)();
				this.props = next;
			},
		};
		state.tips.push(tip);
		return tip;
	},
}));

export async function installGraph() {
	vi.stubGlobal("document", {
		documentElement: { style: { getPropertyValue: () => "#fff" } },
		getElementById: () => state.container,
		createElement: (tag: string) => {
			const element = {
				style: {}, textContent: "", className: "", children: [] as unknown[],
				appendChild(child: unknown) { this.children.push(child); },
				cloneNode() { return this; },
				getContext: () => tag === "canvas" ? state.ctx : undefined,
			};
			return element;
		},
		createTextNode: (text: string) => ({ textContent: text }),
	});
	vi.stubGlobal("window", {
		devicePixelRatio: 2,
		addEventListener: (_type: string, listener: typeof state.listener) => { state.listener = listener; },
	});
	vi.stubGlobal("MutationObserver", class {
		observe() {}
		disconnect() {}
	});
	vi.stubGlobal("acquireVsCodeApi", () => ({ postMessage: state.postMessage, setState: state.setState }));
	await import("../../../src/webview/graph");
}

export function send(message: unknown) {
	state.listener!({ data: message });
}

export function render(nodes: GraphNode[]): cytoscape.Core {
	send({ command: "go", data: nodes, settings: { wheelSensitivity: 1 } });
	return state.cores[state.cores.length - 1];
}

export function node(id: string, targets: string[] = []): GraphNode {
	return {
		id, name: id, isPrimary: true, entityType: "idea",
		references: targets.map((key) => ({ key, isOutgoing: true })),
		location: { filename: `${id}.txt`, line: 1, column: 0 },
	};
}

export function resetGraphState() {
	state.layouts.length = 0;
	state.tips.length = 0;
	vi.clearAllMocks();
}

export { state };
