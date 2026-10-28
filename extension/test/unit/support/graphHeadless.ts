import { vi } from "vitest";
import type cytoscape from "cytoscape";
import type { GraphNode } from "../../../src/common/graphTypes";

// Keep the real collection, event, style and graph algorithms. Only the browser
// surface and asynchronous layout engine are replaced; graph.ts and canvas.ts
// execute through their normal message dispatcher.
const state = vi.hoisted(() => ({
	cores: [] as cytoscape.Core[],
	layouts: [] as { name: string; ids: string[]; owner: number }[],
	layoutCallbacks: [] as Array<{ name: string; owner: number; callback: () => void; active: boolean }>,
	layoutFailure: undefined as Error | undefined,
	layoutEvents: [] as string[],
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
	graphNotice: { textContent: "", hidden: true },
	tips: [] as { show: ReturnType<typeof vi.fn>; hide: ReturnType<typeof vi.fn>; destroy: ReturnType<typeof vi.fn>; setProps: (next: Record<string, unknown>) => void; props: Record<string, unknown> }[],
	mergeImages: vi.fn(),
	fileReaderFailure: undefined as Error | undefined,
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
			function Layout(this: {
				options: { eles: cytoscape.CollectionReturnValue };
				callback?: () => void;
				callbackRecord?: (typeof state.layoutCallbacks)[number];
				owner: number;
				one: (event: string, callback: () => void) => void;
				removeListener: (event: string, callback: () => void) => void;
			}, options: { eles: cytoscape.CollectionReturnValue }) {
				this.options = options;
				this.owner = state.cores.length - 1;
				this.one = (_event, callback) => { this.callback = callback; };
				this.removeListener = (_event, callback) => {
					if (this.callback === callback) {
						this.callback = undefined;
						if (this.callbackRecord) this.callbackRecord.active = false;
					}
				};
			}
			Object.defineProperty(Layout.prototype as object, "run", {
				value: function (this: {
					options: { eles: cytoscape.CollectionReturnValue };
					callback?: () => void;
					callbackRecord?: (typeof state.layoutCallbacks)[number];
					owner: number;
				}) {
					if (name === "elk" && state.layoutFailure) throw state.layoutFailure;
					state.layouts.push({ name, owner: this.owner, ids: this.options.eles.map((e) => e.id()) });
					state.layoutEvents.push(`${name}:run`);
					if (name === "elk") {
						this.callbackRecord = {
							name,
							owner: this.owner,
							callback: this.callback ?? (() => {}),
							active: true,
						};
						state.layoutCallbacks.push(this.callbackRecord);
					}
					return this;
				},
			});
			factory("layout", name, Layout);
		}
	},
}));
vi.mock("cytoscape-popper", () => ({ default: () => {} }));
vi.mock("merge-images", () => ({ default: state.mergeImages }));
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
		getElementById: (id: string) => id === "graph-notice" ? state.graphNotice : state.container,
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
	// exportImage composites through OffscreenCanvas. This runtime has no renderer.
	vi.stubGlobal("OffscreenCanvas", class {
		getContext() { return state.ctx; }
		convertToBlob() { return Promise.resolve(new Blob(["png"], { type: "image/png" })); }
	});
	vi.stubGlobal("FileReader", class {
		result = "";
		error: Error | null = null;
		onloadend: (() => void) | null = null;
		onerror: (() => void) | null = null;
		readAsDataURL() {
			const failure = state.fileReaderFailure;
			if (failure) {
				this.error = failure;
				this.onerror?.();
				return;
			}
			this.result = "data:image/png;base64,b3ZlcmxheQ==";
			this.onloadend?.();
		}
	});
	await import("../../../src/webview/graph");
}

export function send(message: unknown) {
	state.listener!({ data: message });
}

export function render(nodes: GraphNode[], options: { completeLayouts?: boolean } = {}): cytoscape.Core {
	send({ command: "go", data: nodes, settings: { wheelSensitivity: 1 } });
	if (options.completeLayouts !== false) completeLayout("elk");
	return state.cores[state.cores.length - 1];
}

export function completeLayout(name: string, owner = state.cores.length - 1) {
	const pending = state.layoutCallbacks.find((layout) => layout.name === name && layout.owner === owner && layout.active);
	if (pending) {
		state.layoutCallbacks.splice(state.layoutCallbacks.indexOf(pending), 1);
		pending.active = false;
		state.layoutEvents.push(`${name}:stop`);
		pending.callback();
	}
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
	state.layoutCallbacks.length = 0;
	state.layoutFailure = undefined;
	state.layoutEvents.length = 0;
	state.postMessage.mockClear();
	state.mergeImages.mockReset();
	state.fileReaderFailure = undefined;
	state.tips.length = 0;
	state.graphNotice.textContent = "";
	state.graphNotice.hidden = true;
}

export { state };
