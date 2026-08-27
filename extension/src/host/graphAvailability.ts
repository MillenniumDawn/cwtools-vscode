// The graph is built from the server's `getGraphData` command. The Rust engine
// hasn't ported it (techGraph / event-graph are still F#-only), so the commands
// that build a graph live are hidden rather than failing with a raw
// "command 'getGraphData' not found". Reading the capability instead of
// hardcoding the answer means a server that gains the command lights the
// commands back up with no client change.
export const GRAPH_DATA_COMMAND = 'getGraphData';

export function graphDataAvailable(serverCommands: readonly string[] | undefined): boolean {
	return serverCommands?.includes(GRAPH_DATA_COMMAND) ?? false;
}

// The workspace-wide auto-fix runs the server's `fixAllWorkspace` command. The
// pinned engine doesn't advertise it, so the palette entry is hidden rather than
// dead-ending in a protocol error; a newer server lights it back up.
export const FIX_ALL_WORKSPACE_COMMAND = 'fixAllWorkspace';

export function fixAllWorkspaceAvailable(serverCommands: readonly string[] | undefined): boolean {
	return serverCommands?.includes(FIX_ALL_WORKSPACE_COMMAND) ?? false;
}

export const FORMAT_WORKSPACE_COMMAND = 'formatWorkspace';

export function formatWorkspaceAvailable(serverCommands: readonly string[] | undefined): boolean {
	return serverCommands?.includes(FORMAT_WORKSPACE_COMMAND) ?? false;
}
