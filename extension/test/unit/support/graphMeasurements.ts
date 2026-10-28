import type cytoscape from "cytoscape";
import { performance } from "node:perf_hooks";
import { vi } from "vitest";
import { node, render, state } from "./graphHeadless";

export function listenerCounts(size: number) {
	const prototype = Object.getPrototypeOf(state.cores[0].collection()) as { on: cytoscape.NodeSingular["on"] };
	const corePrototype = Object.getPrototypeOf(state.cores[0]) as { on: cytoscape.Core["on"] };
	const nodeOn = vi.spyOn(prototype, "on");
	const coreOn = vi.spyOn(corePrototype, "on");
	try {
		const cy = render(Array.from({ length: size }, (_, i) => node(`n${i}`, [`n${(i + 1) % size}`])));
		return { cy, perNode: nodeOn.mock.calls.length, core: coreOn.mock.calls.length };
	} finally { nodeOn.mockRestore(); coreOn.mockRestore(); }
}

export function scratchCollections(cy: cytoscape.Core) {
	const collectionPrototype = Object.getPrototypeOf(cy.collection()) as object;
	const collections: { path: string; collection: cytoscape.CollectionReturnValue }[] = [];
	const visited = new Set<object>();
	const visit = (value: unknown, path: string) => {
		if (typeof value !== "object" || value === null) return;
		if (Object.prototype.isPrototypeOf.call(collectionPrototype, value)) {
			collections.push({ path, collection: value as cytoscape.CollectionReturnValue });
			return;
		}
		if (visited.has(value)) return;
		visited.add(value);
		if (value instanceof Map) {
			for (const [key, entry] of value) { visit(key, `${path}.key`); visit(entry, `${path}.value`); }
		} else if (value instanceof Set) {
			for (const entry of value) visit(entry, `${path}.value`);
		} else {
			for (const [key, entry] of Object.entries(value)) visit(entry, `${path}.${key}`);
		}
	};
	visit(cy.scratch(), "core");
	cy.nodes().forEach((n) => { visit(n.scratch(), n.id()); });
	return collections;
}

export function traverseHovers(cy: cytoscape.Core) {
	cy.nodes().forEach((n) => { n.emit("mouseover"); n.emit("mouseout"); });
}

export function measuredRender(size: number) {
	const corePrototype = Object.getPrototypeOf(state.cores[0]) as { fit: cytoscape.Core["fit"] };
	const collectionPrototype = Object.getPrototypeOf(state.cores[0].collection()) as {
		union: cytoscape.CollectionReturnValue["union"];
		layout: cytoscape.CollectionReturnValue["layout"];
		difference: cytoscape.CollectionReturnValue["difference"];
		degree: cytoscape.NodeSingular["degree"];
	};
	const originalFit = corePrototype.fit;
	const originalUnion = collectionPrototype.union;
	const originalLayout = collectionPrototype.layout;
	const originalDifference = collectionPrototype.difference;
	const originalDegree = collectionPrototype.degree;
	let active = false;
	let started = 0;
	let elapsed = 0;
	let unions = 0;
	let copiedElements = 0;
	let differenceInputs = 0;
	let degreeCalls = 0;
	const fit = vi.spyOn(corePrototype, "fit").mockImplementation(function (this: cytoscape.Core, ...args) {
		const result = originalFit.apply(this, args);
		started = performance.now();
		active = true;
		return result;
	});
	const union = vi.spyOn(collectionPrototype, "union").mockImplementation(function (this: cytoscape.CollectionReturnValue, other) {
		if (active) {
			unions++;
			copiedElements += this.length + (typeof other === "string" ? this.cy().$(other).length : other.length);
		}
		return originalUnion.call(this, other);
	});
	const difference = vi.spyOn(collectionPrototype, "difference").mockImplementation(function (this: cytoscape.CollectionReturnValue, other) {
		if (active) differenceInputs += this.length + (typeof other === "string" ? this.cy().$(other).length : other.length);
		return originalDifference.call(this, other);
	});
	const degree = vi.spyOn(collectionPrototype, "degree").mockImplementation(function (this: cytoscape.NodeSingular, ...args) {
		if (active) degreeCalls++;
		return originalDegree.apply(this, args);
	});
	const layout = vi.spyOn(collectionPrototype, "layout").mockImplementation(function (this: cytoscape.CollectionReturnValue, options) {
		if (options.name === "elk") {
			elapsed = performance.now() - started;
			active = false;
		}
		return originalLayout.call(this, options);
	});
	try {
		// Identical mostly-disconnected fixture: 490 isolated + one 10-node chain
		// at size 500. For smaller sizes, keep the same connected fraction.
		const connected = Math.max(2, Math.floor(size / 50));
		const cy = render(Array.from({ length: size }, (_, i) =>
			node(`n${i}`, i < connected - 1 ? [`n${i + 1}`] : [])));
		return { cy, unions, copiedElements, differenceInputs, degreeCalls, elapsed };
	} finally {
		fit.mockRestore(); union.mockRestore(); layout.mockRestore();
		difference.mockRestore(); degree.mockRestore();
	}
}
