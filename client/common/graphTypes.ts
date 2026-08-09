/**
 * Types for the graph data returned by getGraphData command
 */

// Static, not `await import('vscode')`: esbuild bundles this file as CJS with
// vscode external, and it leaves a native dynamic import alone. That goes
// through Node's ESM resolver, which never sees the host's require('vscode')
// shim, so the call threw ERR_MODULE_NOT_FOUND in the packaged extension. The
// webview imports this module with `import type` only, so nothing browser-side
// pulls vscode in.
import { commands } from 'vscode';

/**
 * Represents a location in a file
 */
export interface GraphLocation {
    /** File path with forward slashes */
    filename: string;
    /** Line number (1-based) */
    line: number;
    /** Column number (1-based) */
    column: number;
}

/**
 * Represents a reference between graph nodes
 */
export interface GraphReference {
    /** The key/id of the referenced node */
    key: string;
    /** Whether this is an outgoing reference */
    isOutgoing: boolean;
    /** Optional label for the reference */
    label?: string;
}

/**
 * Represents a node in the graph
 */
export interface GraphNode {
    /** Unique identifier for the node */
    id: string;
    /** Display name for the node */
    name?: string;
    /** List of references to other nodes */
    references: GraphReference[];
    /** Location of the node in a file */
    location?: GraphLocation;
    /** Additional details as key-value pairs */
    details?: GraphNodeDetail[];
    /** Whether this is a primary node */
    isPrimary: boolean;
    /** Type of the entity */
    entityType: string;
    /** Display name for the entity type */
    entityTypeDisplayName?: string;
    /** Abbreviation for the node */
    abbreviation?: string;
}

/**
 * Represents a detail entry for a graph node
 */
export interface GraphNodeDetail {
    /** Key for the detail */
    key: string;
    /** Values associated with the key */
    values: string[];
}

/**
 * Complete graph data returned by getGraphData
 */
export type GraphData = GraphNode[];

/**
 * State the graph webview persists across window reloads via setState.
 * Only the request parameters are kept: the graph data itself is re-requested
 * from the server (or re-imported) when the panel is restored.
 */
export interface GraphPanelState {
    /** Where the graph's data came from. */
    source: "server" | "json";
    /** Entity type the graph was requested for. */
    entityType?: string;
    /** Connection depth the graph was requested at. */
    depth?: number;
}

/**
 * Wrapper function for getGraphData command
 * @param entityType The type of entity to get graph data for
 * @param depth The depth of connections to include
 * @returns Promise with the graph data
 */
export async function getGraphData(entityType: string, depth: number): Promise<GraphData> {
    const result = await commands.executeCommand<unknown[]>("getGraphData", entityType, depth);
    return result as GraphData;
}
