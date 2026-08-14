import * as cyM from "cytoscape";
import type {
	CollectionReturnValue,
	EventObject,
	StylesheetJsonBlock,
} from "cytoscape";
import { registerCytoscapeCanvas } from "./canvas";
import cytoscapeelk from "cytoscape-elk";
import popper from "cytoscape-popper";
import type { Props } from "tippy.js";
import tippy, { type Instance } from "tippy.js";
import "tippy.js/dist/tippy.css";
import mergeimages from "merge-images";
import type {
	GraphLocation,
	GraphPanelState,
	GraphReference,
	GraphNodeDetail,
} from "../common/graphTypes";

declare module "cytoscape" {
	interface Core {
		cyCanvas(options: { pixelRatio: string; zIndex: number }): {
			getCanvas(): HTMLCanvasElement;
			clear: (ctx: CanvasRenderingContext2D) => void;
			resetTransform(ctx: CanvasRenderingContext2D): void;
			setTransform: (ctx: CanvasRenderingContext2D) => void;
		};
	}
}

registerCytoscapeCanvas(cyM.default());
cyM.default.use(cytoscapeelk as cytoscape.Ext);
cyM.default.use(popper);

interface vscode {
	postMessage(message: unknown): void;
	setState(state: unknown): void;
}

declare const acquireVsCodeApi: () => vscode;
const vscode: vscode = acquireVsCodeApi();

const htmlEl = document.documentElement;
const vscodeFg = () =>
	htmlEl.style.getPropertyValue("--vscode-editor-foreground");
const vscodeBg = () =>
	htmlEl.style.getPropertyValue("--vscode-editor-background");

// Beyond these bounds the blur is invisible but still costs a full shadow
// pass per draw call, so drop it.
const SHADOW_NODE_LIMIT = 300;
const SHADOW_MIN_ZOOM = 0.4;

function drawExtra(
	nodes: cytoscape.NodeCollection,
	ctx: CanvasRenderingContext2D,
	zoom: number,
	withShadows = true,
) {
	// Draw shadows under nodes
	ctx.shadowColor = "black";
	ctx.shadowBlur = withShadows ? 25 * zoom : 0;
	ctx.font = "16px sans-serif";
	ctx.textAlign = "center";
	ctx.textBaseline = "middle";
	nodes.forEach((node) => {
		let label: string = node.scratch("_drawLabel") as string;
		if (label === undefined) {
			const text: string = node.data("entityType") as string;
			label =
				(node.data("abbreviation") as string) ||
				text
					.split("_")
					.map((f) => f[0].toUpperCase())
					.join("");
			node.scratch("_drawLabel", label);
		}
		const pos = node.position();

		ctx.fillStyle = node.data("isPrimary") ? "#EEE" : "#444";
		ctx.globalAlpha = node.hasClass("semitransp") ? 0.5 : 1;
		ctx.beginPath();
		ctx.arc(pos.x, pos.y, 15, 0, 2 * Math.PI, false);
		ctx.fill();
		ctx.fillStyle = "black";
		ctx.stroke();

		if (node.data("deadend_option")) {
			ctx.arc(pos.x, pos.y, 13, 0, 2 * Math.PI, false);
			ctx.stroke();
		}

		ctx.fillText(label, pos.x, pos.y);
	});
}

const style: StylesheetJsonBlock[] = [
	// the stylesheet for the graph
	{
		selector: "node",
		style: {
			"background-color": function (ele) {
				if (ele.data("isPrimary")) {
					return "#666";
				} else {
					return "#AAA";
				}
			},
			label: "data(label)",
			color: vscodeFg,
			"text-background-color": vscodeBg,
			"text-background-opacity": 0.8,
			"text-wrap": "wrap",
			"text-max-width": "200px",
		},
	},

	{
		selector: "edge",
		style: {
			width: 3,
			"line-color": "#ccc",
			"mid-target-arrow-color": "#ccc",
			"mid-target-arrow-shape": "triangle",
			"curve-style": "haystack",
			"line-style": function (ele) {
				if (ele.data("isPrimary")) {
					return "solid";
				} else {
					return "dashed";
				}
			},
		},
	},
	{
		selector: "edge[label]",
		style: {
			label: "data(label)",
			color: vscodeFg,
			"text-background-color": vscodeBg,
			"text-background-opacity": 0.8,
		},
	},
	{
		selector: "node.highlight",
		style: {
			"border-color": "#FFF",
			"border-width": "2px",
		},
	},
	{
		selector: "node.semitransp",
		style: { opacity: 0.5 },
	},
	{
		selector: "edge.highlight",
		style: { "mid-target-arrow-color": "#FFF" },
	},
	{
		selector: "edge.semitransp",
		style: { opacity: 0.2 },
	},
];
let _cy: cytoscape.Core;
let _tips: Instance[] = [];

function initCytoscape(settings: settings): cytoscape.Core {
	if (_cy) {
		_tips.forEach((t) => t.destroy());
		_tips = [];
		_cy.destroy();
		document.getElementById("cy")!.replaceChildren();
	}
	const cy = cyM.default({
		container: document.getElementById("cy"),
		minZoom: 0.1,
		maxZoom: 5,
		layout: { name: "preset", padding: 10 },
		pixelRatio: 1,
		wheelSensitivity: settings.wheelSensitivity,
	});
	_cy = cy;
	return cy;
}

function populateGraph(
	cy: cytoscape.Core,
	data: techNode[],
	edges: EdgeInput[],
) {
	const allIDs = new Set(data.map((el) => el.id));
	const nonPrimary = new Set(
		data.filter((el) => el.isPrimary === false).map((el) => el.id),
	);

	const elements: cytoscape.ElementDefinition[] = data.map((element) => ({
		group: "nodes",
		data: {
			id: element.id,
			label: element.name || element.id,
			isPrimary: element.isPrimary,
			entityType: element.entityType,
			abbreviation: element.abbreviation,
			entityTypeDisplayName: element.entityTypeDisplayName
				? element.entityTypeDisplayName
				: element.entityType,
			details: element.details,
			location: element.location,
		},
	}));

	for (const edge of edges) {
		if (allIDs.has(edge.source) && allIDs.has(edge.target)) {
			// An edge is primary only when both endpoints are primary; a single
			// non-primary endpoint demotes it. Compute it once here instead of
			// re-scanning every edge for each non-primary node (was O(nodes*edges)).
			const isPrimary = !(
				nonPrimary.has(edge.source) || nonPrimary.has(edge.target)
			);
			elements.push({
				group: "edges",
				data: {
					source: edge.source,
					target: edge.target,
					label: edge.label,
					isPrimary,
				},
			});
		}
	}

	cy.add(elements);
}

function setupTooltips(cy: cytoscape.Core) {
	cy.nodes().forEach(function (node) {
		const buildTip = () => {
			const tip = document.createElement("div");
			const strong = document.createElement("strong");
			strong.textContent = String(node.data("entityTypeDisplayName"));
			tip.appendChild(strong);
			tip.appendChild(document.createTextNode(`: ${String(node.data("id"))}`));
			return tip;
		};
		const buildDetailTip = () => {
			const tip = buildTip();
			const table = document.createElement("table");
			table.className = "cwtools-table";
			const detailsArr = node.data("details") as GraphNodeDetail[] | undefined;
			if (detailsArr && detailsArr.length > 0) {
				for (const d of detailsArr) {
					const tr = document.createElement("tr");
					const tdKey = document.createElement("td");
					tdKey.textContent = d.key;
					const tdVals = document.createElement("td");
					tdVals.textContent = d.values.join(", ");
					tr.appendChild(tdKey);
					tr.appendChild(tdVals);
					table.appendChild(tr);
				}
			} else {
				const tr = document.createElement("tr");
				const td = document.createElement("td");
				td.className = "cwtools-text-center";
				td.textContent = "-";
				tr.appendChild(td);
				table.appendChild(tr);
			}
			tip.appendChild(table);
			return tip;
		};
		// Built on demand for the same reason as getRef below, and the detail
		// table only for a hover held long enough to expand the tooltip.
		let simpleTip: HTMLElement | undefined;
		const getSimpleTip = () => (simpleTip ??= buildTip());
		let detailTip: HTMLElement | undefined;
		const getDetailTip = () => (detailTip ??= buildDetailTip());
		// Defer popperRef() (it allocates a DOM node) until the tooltip is first
		// shown, so a graph with thousands of nodes does not create thousands of
		// DOM elements up front.
		let ref: ReturnType<typeof node.popperRef> | undefined;
		const getRef = () => (ref ??= node.popperRef());
		let isSimple = true;
		const simpleOptions: Partial<Props> = {
			getReferenceClientRect: () => getRef().getBoundingClientRect(),
			content: () => {
				const content = document.createElement("div");
				content.appendChild(getSimpleTip().cloneNode(true));
				return content;
			},
			sticky: true,
			trigger: "manual",
			delay: [null, 200],
		};
		let hoverTimeout: NodeJS.Timeout;
		const complexOptions = {
			getReferenceClientRect: () => getRef().getBoundingClientRect(),
			content: () => {
				const content = document.createElement("div");
				content.appendChild(getDetailTip().cloneNode(true));
				return content;
			},
			onHidden: (instance: Instance) => {
				clearTimeout(hoverTimeout);
				instance.setProps(simpleOptions);
				isSimple = true;
			},
			sticky: true,
			flipOnUpdate: true,
			interactive: true,
			trigger: "manual",
		};
		let tip: Instance | undefined;
		const getTip = () => {
			if (!tip) {
				tip = tippy(document.createElement("div"), simpleOptions);
				_tips.push(tip);
			}
			return tip;
		};
		const expandTooltip = function (element: Instance) {
			element.setProps(complexOptions);
			isSimple = false;
		};
		node.on("mouseover", () => {
			const instance = getTip();
			instance.show();
			hoverTimeout = setTimeout(expandTooltip, 1000, instance);
		});
		node.on("mouseout", () => {
			clearTimeout(hoverTimeout);
			if (isSimple && tip) {
				tip.hide();
			}
		});
	});
}

function runLayout(cy: cytoscape.Core) {
	cy.fit();
	const opts = {
		name: "elk",
		nodeDimensionsIncludeLabels: true,
		elk: {
			"elk.edgeRouting": "SPLINES",
			"elk.direction": "DOWN",
			"elk.aspectRatio": cy.width() / cy.height(),
			"elk.algorithm": "layered",
			"elk.layered.nodePlacement.bk.edgeStraightening": "NONE",
			"elk.layered.compaction.connectedComponents": true,
			"elk.hierarchyHandling": "SEPARATE_CHILDREN",
		},
	};

	const t = cy.elements();
	const groups: CollectionReturnValue[] = t.components();
	const singles = groups.filter((f) => f.length === 1);
	const singles2 = singles.reduce((p, c) => p.union(c), cy.collection());
	const rest = groups.filter((f) => f.length !== 1);
	const rest2 = rest.reduce((p, c) => p.union(c), cy.collection());

	const lrest = rest2.layout(opts);
	lrest.run();
	const opts2 = {
		name: "grid",
		condense: true,
		nodeDimensionsIncludeLabels: true,
	};
	const lsingles = singles2.layout(opts2);
	lsingles.run();
	singles2.shift("y", (singles2.boundingBox({}).y2 + 10) * -1);
	cy.fit();
}

function setupInteraction(
	cy: cytoscape.Core,
	layer: ReturnType<typeof cy.cyCanvas>,
	ctx: CanvasRenderingContext2D,
) {
	let tappedBefore: cytoscape.NodeSingular | null;
	let tappedTimeout: NodeJS.Timeout;

	cy.on("tap", function (event: EventObject) {
		const tappedNow = event.target as cytoscape.NodeSingular;
		if (tappedTimeout && tappedBefore) {
			clearTimeout(tappedTimeout);
		}
		if (tappedBefore === tappedNow) {
			tappedNow.trigger("doubleTap");
			tappedBefore = null;
		} else {
			tappedTimeout = setTimeout(function () {
				tappedBefore = null;
			}, 300);
			tappedBefore = tappedNow;
		}
	});
	cy.on("doubleTap", "node", function (event) {
		goToNode(
			(event.target as cytoscape.NodeSingular).data(
				"location",
			) as GraphLocation,
		);
	});

	// The graph is static after setup, so the neighborhood partition per node
	// can be computed once instead of on every hover.
	const hoods = new Map<
		string,
		{ hood: CollectionReturnValue; rest: CollectionReturnValue }
	>();
	const hoodOf = (sel: cytoscape.NodeSingular) => {
		let entry = hoods.get(sel.id());
		if (!entry) {
			const hood = sel.closedNeighborhood();
			entry = { hood, rest: cy.elements().difference(hood) };
			hoods.set(sel.id(), entry);
		}
		return entry;
	};
	cy.on("mouseover", "node", function (e) {
		const { hood, rest } = hoodOf(e.target as cytoscape.NodeSingular);
		rest.addClass("semitransp");
		hood.addClass("highlight");
	});
	cy.on("mouseout", "node", function (e) {
		const { hood, rest } = hoodOf(e.target as cytoscape.NodeSingular);
		rest.removeClass("semitransp");
		hood.removeClass("highlight");
	});

	cy.on("render", function () {
		layer.resetTransform(ctx);
		layer.clear(ctx);
		layer.setTransform(ctx);
		const nodes = cy.nodes();
		const zoom = cy.zoom();
		drawExtra(
			nodes,
			ctx,
			zoom,
			nodes.length <= SHADOW_NODE_LIMIT && zoom >= SHADOW_MIN_ZOOM,
		);
	});
}

function tech(
	data: techNode[],
	edges: Array<EdgeInput>,
	settings: settings,
	json?: {
		elements:
			| {
					nodes?: cytoscape.ElementDefinition[];
					edges?: cytoscape.ElementDefinition[];
			  }
			| cytoscape.ElementDefinition[];
	} & Record<string, unknown>,
) {
	const importingJson = json !== undefined;
	const cy = initCytoscape(settings);

	const layer = cy.cyCanvas({ zIndex: 1, pixelRatio: "auto" });
	const canvas = layer.getCanvas();
	const ctx = canvas.getContext("2d")!;

	if (!importingJson) {
		populateGraph(cy, data, edges);
	} else {
		cy.json(json);
	}
	cy.style(style);

	setupTooltips(cy);

	if (!importingJson) {
		runLayout(cy);
	}

	setupInteraction(cy, layer, ctx);
}

export function goToNode(location: GraphLocation) {
	const uri = location.filename;
	const line = location.line;
	const column = location.column;
	vscode.postMessage({
		command: "goToFile",
		uri: uri,
		line: line,
		column: column,
	});
}

export async function exportImage(pixelRatio: number) {
	const png = _cy.png({ full: true, output: "base64uri", scale: pixelRatio });
	const boundingBox = _cy.elements().boundingBox({});
	const canvas = new OffscreenCanvas(
		Math.ceil(boundingBox.x2 - boundingBox.x1) * pixelRatio,
		Math.ceil(boundingBox.y2 - boundingBox.y1) * pixelRatio,
	);

	const ctx = canvas.getContext("2d") as unknown as CanvasRenderingContext2D;

	ctx.scale(pixelRatio, pixelRatio);
	ctx.translate(-1 * boundingBox.x1, -1 * boundingBox.y1);

	drawExtra(_cy.nodes(), ctx, 1 / pixelRatio);

	const canvasImage = await canvas.convertToBlob({ type: "png" });
	const bufferImage = await blobToDataURL(canvasImage);
	const mergedImages = await mergeimages([png, bufferImage]);
	vscode.postMessage({
		command: "saveImage",
		image: mergedImages.substring(mergedImages.indexOf(",") + 1),
	});
}

async function blobToDataURL(blob: Blob): Promise<string> {
	return await new Promise<string>((resolve, reject) => {
		const reader = new FileReader();
		reader.onloadend = () => resolve(reader.result as string);
		reader.onerror = () =>
			reject(
				reader.error
					? new Error(reader.error.message)
					: new Error("failed to read blob"),
			);
		reader.readAsDataURL(blob);
	});
}

export function exportJson() {
	const json = JSON.stringify(_cy.json());
	vscode.postMessage({ command: "saveJson", json: json });
}

interface techNode {
	name: string;
	references: Array<GraphReference>;
	id: string;
	location: GraphLocation;
	isPrimary: boolean;
	details?: Array<GraphNodeDetail>;
	entityTypeDisplayName?: string;
	abbreviation?: string;
	entityType: string;
}
interface settings {
	wheelSensitivity: number;
}
interface EdgeInput {
	source: string;
	target: string;
	label: string;
}

export function go(nodesJ: Array<techNode>, settings: settings) {
	const seen = new Set<string>();
	const edgesfin: EdgeInput[] = [];
	for (const a of nodesJ) {
		// Defaulted, not assumed: an older server that omits `references` would
		// otherwise throw here and take the whole render down.
		for (const b of a.references ?? []) {
			const source = b.isOutgoing ? a.id : b.key;
			const target = b.isOutgoing ? b.key : a.id;
			const label = b.label ?? "";
			// NUL cannot occur in a script id or a localised label, so a plain
			// delimiter is unambiguous here without paying for JSON.stringify.
			const key = `${source}\0${target}\0${label}`;
			if (!seen.has(key)) {
				seen.add(key);
				edgesfin.push({ source, target, label });
			}
		}
	}
	tech(nodesJ, edgesfin, settings);
}

type InboundMessage =
	| {
			command: "go";
			data: techNode[];
			settings: settings;
			persist?: GraphPanelState;
	  }
	| { command: "exportImage" }
	| { command: "exportJson" }
	| {
			command: "importJson";
			settings: settings;
			json: string;
			persist?: GraphPanelState;
	  }
	| { command: "checkCytoscapeRendered" };

// Persist where the graph's data came from, so the window-reload serializer
// can re-request it. Only the request parameters are kept, not the data.
function persistState(state: GraphPanelState | undefined) {
	if (state) {
		vscode.setState(state);
	}
}

window.addEventListener("message", (event) => {
	const message = event.data as InboundMessage; // The JSON data our extension sent
	switch (message.command) {
		case "go":
			go(message.data, message.settings);
			persistState(message.persist);
			break;
		case "exportImage":
			void exportImage(1);
			break;
		case "exportJson":
			exportJson();
			break;
		case "importJson":
			try {
				tech(
					[],
					[],
					message.settings,
					JSON.parse(message.json) as Parameters<typeof tech>[3],
				);
				persistState(message.persist);
			} catch {
				// Malformed import: leave the graph empty rather than take the
				// whole message handler down.
			}
			break;
		case "checkCytoscapeRendered": // Check if cytoscape is initialized and has rendered elements
			{
				const rendered =
					_cy !== undefined &&
					_cy.elements().length > 0 &&
					document.getElementById("cy") !== null;
				vscode.postMessage({
					command: "cytoscapeRenderedResult",
					rendered: rendered,
				});
				break;
			}
	}
});

vscode.postMessage({ command: "ready" });
