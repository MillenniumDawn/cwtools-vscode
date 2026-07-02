import * as cyM from 'cytoscape';
import type { CollectionReturnValue, EventObject, StylesheetJsonBlock } from 'cytoscape'
import { registerCytoscapeCanvas } from './canvas'
import cytoscapeelk from 'cytoscape-elk'
import popper from 'cytoscape-popper';
import type { Props} from 'tippy.js';
import tippy, { type Instance } from 'tippy.js';
import 'tippy.js/dist/tippy.css';
import mergeimages from 'merge-images'
import type { GraphLocation, GraphReference, GraphNodeDetail } from '../common/graphTypes'

declare module 'cytoscape' {
    interface Core {
        cyCanvas(options: { pixelRatio: string; zIndex: number }): {
            getCanvas(): HTMLCanvasElement;
            clear: (ctx: CanvasRenderingContext2D) => void;
            resetTransform(ctx: CanvasRenderingContext2D): void;
            setTransform: (ctx: CanvasRenderingContext2D) => void;
        };
    }

}


interface vscode {
    postMessage(message: unknown): void;
}

declare const acquireVsCodeApi : () => vscode;
const vscode : vscode = acquireVsCodeApi();

/** Escape HTML entities to prevent XSS from server-supplied data. */
function escapeHtml(str: string): string {
    return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function drawExtra(nodes : cytoscape.NodeCollection, ctx : CanvasRenderingContext2D, zoom : number){
    // Draw shadows under nodes
    ctx.shadowColor = "black";
    ctx.shadowBlur = 25 * zoom;
    ctx.fillStyle = "#666";
    nodes.forEach((node) => {
        const text: string = node.data('entityType');
        const eventChars = text.split('_').map(f => f[0].toUpperCase()).join('');
        const eventChars2 = node.data('abbreviation') ? node.data('abbreviation') : eventChars;
        const pos = node.position();

        ctx.fillStyle = node.data('isPrimary') ? "#EEE" : '#444';
        ctx.globalAlpha = node.hasClass('semitransp') ? 0.5 : 1;
        ctx.beginPath();
        ctx.arc(pos.x, pos.y, 15, 0, 2 * Math.PI, false);
        ctx.fill();
        ctx.fillStyle = "black";
        ctx.stroke();

        if (node.data('deadend_option')) {
            ctx.arc(pos.x, pos.y, 13, 0, 2 * Math.PI, false);
            ctx.stroke();
        }

        //Set text to black, center it and set font.
        ctx.fillStyle = "black";
        ctx.font = "16px sans-serif";
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        ctx.fillText(eventChars2, pos.x, pos.y);
    });
}

const style : StylesheetJsonBlock[] = [ // the stylesheet for the graph
    {
        selector: 'node',
        style: {
            'background-color': function (ele) { if (ele.data("isPrimary")) { return '#666' } else { return '#AAA' } },
            'label': 'data(label)',
            'color': function () { return document.getElementsByTagName("html")[0].style.getPropertyValue("--vscode-editor-foreground") },
            'text-background-color': function () { return document.getElementsByTagName("html")[0].style.getPropertyValue("--vscode-editor-background") },
            'text-background-opacity': 0.8,
            'text-wrap': "wrap",
            'text-max-width':"200px"

        }
    },

    {
        selector: 'edge',
        style: {
            'width': 3,
            'line-color': '#ccc',
            'mid-target-arrow-color': '#ccc',
            'mid-target-arrow-shape': 'triangle',
            'curve-style': 'haystack',
            'line-style': function (ele) { if (ele.data("isPrimary")) { return 'solid' } else { return 'dashed' } }
        }
    },
    {
        selector: 'edge[label]',
        style: {
            'label': 'data(label)',
            'color': function () { return document.getElementsByTagName("html")[0].style.getPropertyValue("--vscode-editor-foreground") },
            'text-background-color': function () { return document.getElementsByTagName("html")[0].style.getPropertyValue("--vscode-editor-background") },
            'text-background-opacity': 0.8,
        }
    },
    {
        selector: 'node.highlight',
        style: {
            'border-color': '#FFF',
            'border-width': '2px'
        }
    },
    {
        selector: 'node.semitransp',
        style: { 'opacity': 0.5 }
    },
    {
        selector: 'edge.highlight',
        style: { 'mid-target-arrow-color': '#FFF' }
    },
    {
        selector: 'edge.semitransp',
        style: { 'opacity': 0.2 }
    }
]
let _cy: cytoscape.Core;

function initCytoscape(settings: settings): cytoscape.Core {
    if (_cy) {
        _cy.destroy();
        document.getElementById('cy')!.replaceChildren();
    }
    registerCytoscapeCanvas(cyM.default());
    cyM.default.use(cytoscapeelk);
    cyM.default.use(popper);
    const cy = cyM.default({
        container: document.getElementById('cy'),
        minZoom: 0.1,
        maxZoom: 5,
        layout: { name: 'preset', padding: 10 },
        pixelRatio: 1,
        wheelSensitivity: settings.wheelSensitivity
    });
    _cy = cy;
    return cy;
}

function populateGraph(cy: cytoscape.Core, data: techNode[], edges: EdgeInput[]) {
    data.forEach(function (element) {
        cy.add({ group: 'nodes', data: {
            id: element.id,
            label: element.name || element.id,
            isPrimary: element.isPrimary,
            entityType: element.entityType,
            abbreviation: element.abbreviation,
            entityTypeDisplayName: element.entityTypeDisplayName ? element.entityTypeDisplayName : element.entityType,
            details: element.details,
            location: element.location
        }});
    });
    const allIDs = new Set(data.map((el) => el.id));
    edges.forEach(function (edge) {
        if (allIDs.has(edge.source) && allIDs.has(edge.target)){
            cy.add({ group: 'edges', data: { source: edge.source, target: edge.target, label: edge.label } }).data("isPrimary", true);
        }
    });
    data.forEach(function (element) {
        if(element.isPrimary === false){
            cy.edges().filter((n) => n.target().id() === element.id || n.source().id() === element.id).forEach((e) => {e.data("isPrimary", false);});
        }
    });
}

function setupTooltips(cy: cytoscape.Core) {
    cy.nodes().forEach(function(node) {
        const simpleTooltip = `<strong>${escapeHtml(String(node.data("entityTypeDisplayName")))}</strong>: ${escapeHtml(String(node.data("id")))}`;
        const createRow = function (details : { key : string, values : string[]}) {
            const vals = details.values.join(", ");
            return `<tr><td>${escapeHtml(details.key)}</td><td>${escapeHtml(vals)}</td></tr>`;
        };
        const detailsText = node.data("details") ? node.data("details").map(createRow).join("") : "";
        const detailsTable =
            `${simpleTooltip}
            <table class="cwtools-table">
            ${detailsText ? detailsText : "<tr><td class=\"cwtools-text-center\">-</td></tr>"}
            </table>`;
        const ref = node.popperRef();
        let isSimple = true;
        const simpleOptions : Partial<Props> = {
            getReferenceClientRect: ref.getBoundingClientRect,
            content: () => {
                const content = document.createElement('div');
                content.innerHTML = simpleTooltip;
                return content;
            },
            sticky: true,
            trigger: "manual",
            delay: [null, 200]
        };
        let hoverTimeout : NodeJS.Timeout;
        const complexOptions = {
            getReferenceClientRect: ref.getBoundingClientRect,
            content: () => {
                const content = document.createElement('div');
                content.innerHTML = detailsTable;
                return content;
            },
            onHidden: (instance: Instance) =>
            {
                clearTimeout(hoverTimeout);
                instance.setProps(simpleOptions);
                isSimple = true;
            },
            sticky: true,
            flipOnUpdate: true,
            interactive: true,
            trigger: "manual"
        };
        const dummyDomEle = document.createElement("div");
        let tip : Instance | undefined;
        const getTip = () => (tip ??= tippy(dummyDomEle, simpleOptions));
        const expandTooltip = function(element : Instance) {
            element.setProps(complexOptions);
            isSimple = false;
        };
        node.on('mouseover', () => {
            const instance = getTip();
            instance.show();
            hoverTimeout = setTimeout(expandTooltip, 1000, instance);
        });
        node.on('mouseout', () =>
        {
            clearTimeout(hoverTimeout);
            if(isSimple && tip) {
                tip.hide();
            }
        });
    });
}

function runLayout(cy: cytoscape.Core) {
    cy.fit();
    const opts = {
        name: 'elk',
        nodeDimensionsIncludeLabels: true,
        elk: {
            "elk.edgeRouting": "SPLINES",
            "elk.direction": "DOWN",
            "elk.aspectRatio": (cy.width() / cy.height()),
            "elk.algorithm": "layered",
            "elk.layered.nodePlacement.bk.edgeStraightening": "NONE",
            "elk.layered.compaction.connectedComponents": true,
            "elk.hierarchyHandling": "SEPARATE_CHILDREN",
        }
    };

    const t = cy.elements();
    const groups: CollectionReturnValue[] = t.components();
    const singles = groups.filter((f) => f.length === 1);
    const singles2 = singles.reduce((p, c) => p.union(c), cy.collection());
    const rest = groups.filter((f) => f.length !== 1);
    const rest2 = rest.reduce((p, c) => p.union(c), cy.collection());

    const lrest = rest2.layout(opts);
    lrest.run();
    const opts2 = { name: 'grid', condense: true, nodeDimensionsIncludeLabels: true };
    const lsingles = singles2.layout(opts2);
    lsingles.run();
    singles2.shift("y", (singles2.boundingBox({}).y2 + 10) * -1);
    cy.fit();
}

function setupInteraction(cy: cytoscape.Core, layer: ReturnType<typeof cy.cyCanvas>, ctx: CanvasRenderingContext2D) {
    let tappedBefore : EventTarget | null;
    let tappedTimeout : NodeJS.Timeout;

    cy.on('tap', function (event : EventObject) {
        const tappedNow = event.target;
        if (tappedTimeout && tappedBefore) {
            clearTimeout(tappedTimeout);
        }
        if (tappedBefore === tappedNow) {
            tappedNow.trigger('doubleTap', event);
            tappedBefore = null;
        } else {
            tappedTimeout = setTimeout(function () { tappedBefore = null; }, 300);
            tappedBefore = tappedNow;
        }
    });
    cy.on('doubleTap', 'node', function (event) {
        goToNode(event.target.data('location'));
    });

    cy.on('mouseover', 'node', function (e) {
        const sel = e.target;
        cy.elements()
            .difference(sel.outgoers().union(sel.incomers()))
            .not(sel)
            .addClass('semitransp');
        sel.addClass('highlight')
            .outgoers()
            .union(sel.incomers())
            .addClass('highlight');
    });
    cy.on('mouseout', 'node', function (e) {
        const sel = e.target;
        cy.elements().removeClass('semitransp');
        sel.removeClass('highlight')
            .outgoers()
            .union(sel.incomers())
            .removeClass('highlight');
    });

    cy.on("render", function () {
        layer.resetTransform(ctx);
        layer.clear(ctx);
        layer.setTransform(ctx);
        drawExtra(cy.nodes(), ctx, cy.zoom());
    });
}

function tech(data: techNode[], edges: Array<EdgeInput>, settings: settings, json?: { elements: { nodes?: cytoscape.ElementDefinition[]; edges?: cytoscape.ElementDefinition[]; } | cytoscape.ElementDefinition[]; } & Record<string, unknown>) {
    const importingJson = json !== undefined;
    const cy = initCytoscape(settings);

    const layer = cy.cyCanvas({ zIndex: 1, pixelRatio: "auto" });
    const canvas = layer.getCanvas();
    const ctx = canvas.getContext('2d')!;

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

export function goToNode(location : GraphLocation) {
    const uri = location.filename
    const line = location.line
    const column = location.column
    vscode.postMessage({"command": "goToFile", "uri": uri, "line": line, "column": column})
}

export async function exportImage(pixelRatio: number) {

    const png = _cy.png({ full: true, output: 'base64uri', scale: pixelRatio });
    const boundingBox = _cy.elements().boundingBox({})
    const canvas = new OffscreenCanvas(Math.ceil(boundingBox.x2 - boundingBox.x1) * pixelRatio, Math.ceil(boundingBox.y2 - boundingBox.y1) * pixelRatio)

    const ctx = canvas.getContext("2d") as unknown as CanvasRenderingContext2D;

    ctx.scale(pixelRatio, pixelRatio)
    ctx.translate(-1 * boundingBox.x1, -1 * boundingBox.y1)

    drawExtra(_cy.nodes(), ctx, (1/pixelRatio))

    const canvasImage = await canvas.convertToBlob({ "type":"png"});
    const bufferImage = await blobToDataURL(canvasImage);
    const mergedImages = await mergeimages([png, bufferImage]);
    vscode.postMessage({ "command": "saveImage", "image": mergedImages.substring(mergedImages.indexOf(',') + 1) });
}

async function blobToDataURL(blob: Blob): Promise<string> {
    const buffer = await blob.arrayBuffer();
    const bytes = new Uint8Array(buffer);
    let binary = '';
    for (let i = 0; i < bytes.byteLength; i++) {
        binary += String.fromCharCode(bytes[i]!);
    }
    const base64 = btoa(binary);
    return `data:${blob.type};base64,${base64}`;
}

export function exportJson() {
    const json = JSON.stringify(_cy.json())
    vscode.postMessage({"command": "saveJson", "json": json});
}


interface techNode
{
    name : string
    references: Array<GraphReference>
    id : string
    location: GraphLocation
    isPrimary : boolean
    details? : Array<GraphNodeDetail>
    entityTypeDisplayName? : string
    abbreviation? : string
    entityType : string
}
interface settings
{
    wheelSensitivity: number
}
interface EdgeInput {
    source : string
    target : string
    label : string
}

export function go(nodesJ: Array<techNode>, settings: settings) {
    const nodes: Array<techNode> = nodesJ;
    const edges = nodes.map((a) => a.references.map((b)  => b.isOutgoing ? { source: a.id, target: b.key, label: b.label ?? '' } : { source: b.key, target: a.id, label: b.label ?? '' }));
    const edges2 = edges.flat();
    const edgesfin = new Set(edges2)
    tech(nodes, [...edgesfin], settings);
}

window.addEventListener('message', event => {

    const message = event.data; // The JSON data our extension sent
    switch (message.command) {
        case 'go':
            go(message.data, message.settings)
            break;
        case 'exportImage':
            exportImage(1);
            break;
        case 'exportJson':
            exportJson();
            break;
        case 'importJson':
            tech([],[],message.settings, JSON.parse(message.json))
            break;
        case 'checkCytoscapeRendered':
            // Check if cytoscape is initialized and has rendered elements
            {
                const rendered = _cy !== undefined &&
                             _cy.elements().length > 0 &&
                             document.getElementById('cy') !== null;
            vscode.postMessage({
                "command": "cytoscapeRenderedResult",
                "rendered": rendered
            });
            break; }
    }
});

vscode.postMessage({ "command": "ready"});
