import * as vscode from 'vscode';

/**
 * Result of a hover assertion. Either passed=true with the actual content,
 * or passed=false with the list of substrings that were missing.
 *
 * Returned (not thrown) so the parity suite can record gaps without failing
 * the test outright.
 */
export interface HoverCheckResult {
	passed: boolean;
	actual: string;
	missing: string[];
}

/**
 * Run a hover at the given position and verify the markdown content
 * contains every required substring. Returns a result instead of throwing
 * so the parity suite can collect gaps.
 */
export async function checkHoverContains(
	uri: vscode.Uri,
	position: vscode.Position,
	required: string[]
): Promise<HoverCheckResult> {
	const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
		'vscode.executeHoverProvider',
		uri,
		position
	);

	const hover = hovers?.[0];
	if (!hover || hover.contents.length === 0) {
		return { passed: false, actual: '', missing: required };
	}

	const content = hover.contents[0];
	const actual = content instanceof vscode.MarkdownString ? content.value : String(content);
	const missing = required.filter(s => !actual.includes(s));
	return { passed: missing.length === 0, actual, missing };
}
