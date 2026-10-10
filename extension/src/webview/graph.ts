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
import { deriveNodeLabel } from "./graphLabel";

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
			label = deriveNodeLabel(
				node.data("entityType"),
				node.data("abbreviation"),
			);
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
let _cy: cytoscape.Core | undefined;
let _tips: Instance[] = [];
const _hoverTimers = new Set<NodeJS.Timeout>();
let _themeObserver: MutationObserver | undefined;

function disposeCytoscape() {
	_hoverTimers.forEach((timer) => clearTimeout(timer));
	_hoverTimers.clear();
	_themeObserver?.disconnect();
	_themeObserver = undefined;
	_tips.forEach((t) => t.destroy());
	_tips = [];
	if (_cy) {
		_cy.removeScratch("_hoverHood");
		_cy.nodes().forEach((node) => {
			node.removeScratch("_tooltip");
		});
		_cy.destroy();
		_cy = undefined;
		document.getElementById("cy")!.replaceChildren();
	}
}

function initCytoscape(settings: settings): cytoscape.Core {
	disposeCytoscape();
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
	interface TooltipHandlers {
		mouseover(): void;
		mouseout(): void;
	}
	const tooltipOf = (node: cytoscape.NodeSingular): TooltipHandlers => {
		const cached = node.scratch("_tooltip") as TooltipHandlers | undefined;
		if (cached) {
			return cached;
		}
		const buildTip = () => {
			const tip = document.createElement("div");
			const strong = document.createElement("strong");
			const displayName: unknown = node.data("entityTypeDisplayName");
			strong.textContent = typeof displayName === "string" ? displayName : "";
			tip.appendChild(strong);
			tip.appendChild(document.createTextNode(`: ${String(node.data("id"))}`));
			return tip;
		};
		const buildDetailTip = () => {
			const tip = buildTip();
			const table = document.createElement("table");
			table.className = "cwtools-table";
			const details: unknown = node.data("details");
			const detailsArr = Array.isArray(details) ? details.filter(isGraphNodeDetail) : [];
			if (detailsArr.length > 0) {
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
		let hoverTimeout: NodeJS.Timeout | undefined;
		const cancelExpansion = () => {
			if (hoverTimeout !== undefined) {
				clearTimeout(hoverTimeout);
				_hoverTimers.delete(hoverTimeout);
				hoverTimeout = undefined;
			}
		};
		const complexOptions = {
			getReferenceClientRect: () => getRef().getBoundingClientRect(),
			content: () => {
				const content = document.createElement("div");
				content.appendChild(getDetailTip().cloneNode(true));
				return content;
			},
			onHidden: (instance: Instance) => {
				cancelExpansion();
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
		const handlers: TooltipHandlers = {
			mouseover() {
				cancelExpansion();
				const instance = getTip();
				instance.show();
				const timer = setTimeout(() => {
					_hoverTimers.delete(timer);
					hoverTimeout = undefined;
					expandTooltip(instance);
				}, 1000);
				hoverTimeout = timer;
				_hoverTimers.add(timer);
			},
			mouseout() {
				cancelExpansion();
				if (isSimple && tip) {
					tip.hide();
				}
			},
		};
		node.scratch("_tooltip", handlers);
		return handlers;
	};
	cy.on("mouseover", "node", (event) => {
		tooltipOf(event.target as cytoscape.NodeSingular).mouseover();
	});
	cy.on("mouseout", "node", (event) => {
		const node = event.target as cytoscape.NodeSingular;
		(node.scratch("_tooltip") as TooltipHandlers | undefined)?.mouseout();
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

	// Degree includes self-loops: only truly isolated nodes go to the grid.
	// Keep all edges with the connected partition, including loop edges.
	const singles2 = cy.nodes().filter((node) => node.degree() === 0);
	const rest2 = cy.elements().difference(singles2);

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

	// Retain only the current closed neighborhood. Dimming uses classes on
	// the graph; no complement collection survives an interaction.
	let hovered: cytoscape.NodeSingular | undefined;
	cy.on("mouseover", "node", function (event) {
		const selected = event.target as cytoscape.NodeSingular;
		const previous = cy.scratch("_hoverHood") as CollectionReturnValue | undefined;
		const hood = selected.closedNeighborhood();
		cy.batch(() => {
			previous?.removeClass("highlight");
			cy.elements().difference(hood).addClass("semitransp");
			hood.removeClass("semitransp").addClass("highlight");
		});
		hovered = selected;
		cy.scratch("_hoverHood", hood);
	});
	cy.on("mouseout", "node", function (event) {
		if (event.target !== hovered) {
			return;
		}
		const hood = cy.scratch("_hoverHood") as CollectionReturnValue | undefined;
		cy.batch(() => {
			cy.elements().removeClass("semitransp");
			hood?.removeClass("highlight");
		});
		cy.removeScratch("_hoverHood");
		hovered = undefined;
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

type CytoscapeJson = {
	elements:
		| {
				nodes?: cytoscape.ElementDefinition[];
				edges?: cytoscape.ElementDefinition[];
		  }
		| cytoscape.ElementDefinition[];
} & Record<string, unknown>;

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isCytoscapeElement(value: unknown): boolean {
	return (
		isRecord(value) &&
		isRecord(value.data) &&
		typeof value.data.id === "string" &&
		value.data.id.length > 0
	);
}

function isCytoscapeElements(value: unknown): boolean {
	return Array.isArray(value) && value.every(isCytoscapeElement);
}

function isCytoscapeJson(value: unknown): value is CytoscapeJson {
	if (!isRecord(value)) {
		return false;
	}
	const elements = value.elements;
	if (Array.isArray(elements)) {
		return isCytoscapeElements(elements);
	}
	return (
		isRecord(elements) &&
		(elements.nodes === undefined || isCytoscapeElements(elements.nodes)) &&
		(elements.edges === undefined || isCytoscapeElements(elements.edges))
	);
}

function isGraphNodeDetail(value: unknown): value is GraphNodeDetail {
	return isRecord(value) && typeof value.key === "string" &&
		Array.isArray(value.values) && value.values.every((v: unknown) => typeof v === "string");
}

function isGraphLocation(value: unknown): value is GraphLocation {
	return isRecord(value) && typeof value.filename === "string" && value.filename.length > 0 &&
		typeof value.line === "number" && Number.isSafeInteger(value.line) && value.line >= 1 &&
		typeof value.column === "number" && Number.isSafeInteger(value.column) && value.column >= 0;
}

class GraphMetadataError extends Error {}

function validateImportedMetadata(json: CytoscapeJson) {
	const elements = Array.isArray(json.elements) ? json.elements : json.elements.nodes ?? [];
	for (const element of elements) {
		const data: unknown = element.data;
		if (!isRecord(data)) {
			continue;
		}
		// Imported IDs may contain command links rendered by host notifications.
		// Keep validation errors independent of all imported field values.
		if (data.entityTypeDisplayName !== undefined && typeof data.entityTypeDisplayName !== "string") {
			throw new GraphMetadataError("a node has an invalid entityTypeDisplayName (expected a string)");
		}
		if (data.details !== undefined &&
			(!Array.isArray(data.details) || !data.details.every(isGraphNodeDetail))) {
			throw new GraphMetadataError("a node has invalid details (expected string keys and arrays of strings)");
		}
		if (data.location !== undefined && !isGraphLocation(data.location)) {
			throw new GraphMetadataError("a node has an invalid location (expected filename and whole-number line/column coordinates)");
		}
	}
}

function reportImportError(fileName: string | undefined, detail?: string) {
	const source = fileName ? `"${fileName}"` : "the selected JSON file";
	vscode.postMessage({
		command: "showError",
		message: detail
			? `CWTools: couldn't import ${source}: ${detail}.`
			: `CWTools: couldn't import ${source}: it isn't valid Cytoscape graph JSON.`,
	});
}

function tech(
	data: techNode[],
	edges: Array<EdgeInput>,
	settings: settings,
	json?: CytoscapeJson,
) {
	const importingJson = json !== undefined;
	const cy = initCytoscape(settings);

	try {
		const layer = cy.cyCanvas({ zIndex: 1, pixelRatio: "auto" });
		const canvas = layer.getCanvas();
		const ctx = canvas.getContext("2d")!;

		if (!importingJson) {
			populateGraph(cy, data, edges);
		} else {
			cy.json(json);
		}
		cy.style(style);
		_themeObserver = new MutationObserver(() => cy.style().update());
		_themeObserver.observe(htmlEl, {
			attributes: true,
			attributeFilter: ["style"],
		});

		setupTooltips(cy);

		if (!importingJson) {
			runLayout(cy);
		}

		setupInteraction(cy, layer, ctx);
	} catch (error) {
		// An import that Cytoscape rejects must not leave a partial graph behind.
		disposeCytoscape();
		throw error;
	}
}

function goToNode(location: unknown) {
	if (!isGraphLocation(location)) {
		return;
	}
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

function reportUnavailableExport(kind: "an image" | "a JSON file") {
	vscode.postMessage({
		command: "showError",
		message: `CWTools: no graph is available to export as ${kind}.`,
	});
}

function reportImageExportError(error: unknown) {
	const detail = error instanceof Error ? error.message : String(error);
	vscode.postMessage({
		command: "showError",
		message: `CWTools: couldn't export graph image: ${detail}.`,
	});
}

export async function exportImage(pixelRatio: number) {
	const cy = _cy;
	if (!cy) {
		reportUnavailableExport("an image");
		return;
	}

	const png = cy.png({ full: true, output: "base64uri", scale: pixelRatio });
	const boundingBox = cy.elements().boundingBox({});
	const canvas = new OffscreenCanvas(
		Math.ceil(boundingBox.x2 - boundingBox.x1) * pixelRatio,
		Math.ceil(boundingBox.y2 - boundingBox.y1) * pixelRatio,
	);

	const ctx = canvas.getContext("2d") as unknown as CanvasRenderingContext2D;

	ctx.scale(pixelRatio, pixelRatio);
	ctx.translate(-1 * boundingBox.x1, -1 * boundingBox.y1);

	drawExtra(cy.nodes(), ctx, 1 / pixelRatio);

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
	const cy = _cy;
	if (!cy) {
		reportUnavailableExport("a JSON file");
		return;
	}

	const json = JSON.stringify(cy.json());
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
	| { command: "checkCytoscapeRendered"; id: number };

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
			void exportImage(1).catch(reportImageExportError);
			break;
		case "exportJson":
			exportJson();
			break;
		case "importJson":
			try {
				const json: unknown = JSON.parse(message.json);
				if (!isCytoscapeJson(json)) {
					throw new Error("invalid Cytoscape graph JSON");
				}
				validateImportedMetadata(json);
				tech([], [], message.settings, json);
				persistState(message.persist);
			} catch (error) {
				// Parse and validate before replacing a current graph. A Cytoscape
				// failure after that still disposes the partial replacement in tech.
				reportImportError(message.persist?.fileName,
					error instanceof GraphMetadataError ? error.message : undefined);
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
					id: message.id,
				});
				break;
			}
	}
});

vscode.postMessage({ command: "ready" });
