// The graph is built from the server's `getGraphData` command. Reading the
// capability instead of hardcoding the answer keeps the commands hidden while
// the server is unavailable and lights them back up when it advertises them.
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
