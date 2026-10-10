import * as path from "path";
import { isTrustedPath } from "./trustedPaths";

// Match the server's content roots at startup. Parent mods overlapping the
// selected workspace are rejected by the server, so they must not authorize
// language changes or watched-file events outside the selected mod.
export function gameContentRoots(
	workspaceRoot: string,
	parents: readonly string[] = [],
	vanilla?: string,
): string[] {
	const acceptedParents = parents
		.map((root) => path.resolve(workspaceRoot, root))
		.filter((root) =>
			!isTrustedPath(root, [workspaceRoot]) &&
			!isTrustedPath(workspaceRoot, [root]),
		);
	return [workspaceRoot, ...acceptedParents, ...(vanilla ? [vanilla] : [])];
}
