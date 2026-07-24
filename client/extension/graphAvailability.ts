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
