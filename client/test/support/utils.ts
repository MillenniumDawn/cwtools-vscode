import * as fs from "node:fs";
import * as vscode from "vscode";
import * as path from "path";
import type { CwtoolsApi } from "../../extension/extension";
import type * as GraphPanelNamespace from "../../extension/graphPanel";

// Re-exported so host suites can keep importing it from here. The function
// itself lives in a vscode-free module so the parity harness can share it.
export { extractCompletionLabel } from "./labels";

// The published extension id: publisher.name from release/package.json.
export const EXTENSION_ID = "milleniumdawnmodteam.cwtools-md-edition";

/** Resolved path to the sample mod used by all test suites.
 *
 * The workspace in .vscode-test.mjs is the source tree
 * (./client/test/sample), not the stale compiled copy under
 * release/bin. Resolve from the workspace folder when available
 * so the path works both from the esbuild bundle
 * (release/bin/client/test/support) and from tsx/vitest source
 * (client/test/support). Falls back to probing candidates so a
 * bare import outside the host still finds the fixture.
 */
export const SAMPLE_ROOT = (() => {
	const ws = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
	if (ws && fs.existsSync(path.join(ws, "events/irm.txt"))) {
		return ws;
	}
	const candidates = [
		path.resolve(__dirname, "../../../../../client/test/sample"),
		path.resolve(__dirname, "../sample"),
		path.resolve(__dirname, "../../sample"),
		path.resolve(__dirname, "../../../client/test/sample"),
	];
	for (const c of candidates) {
		try {
			if (fs.existsSync(path.join(c, "events/irm.txt"))) return c;
		} catch (_e) {
			// probe next candidate
		}
	}
	return candidates[0];
})();

export async function activate(): Promise<CwtoolsApi | undefined> {
	const ext = vscode.extensions.getExtension(EXTENSION_ID)!;
	try {
		await ext.activate();
		return ext.exports as CwtoolsApi | undefined;
	} catch (error) {
		// Extension activation might fail due to missing language server in test environment
		// But we can still test other aspects of the extension
		console.warn(
			"Extension activation had issues (expected in test environment):",
			error,
		);
		return ext.exports as CwtoolsApi | undefined;
	}
}

/**
 * The graphPanel module the running extension uses, taken from its activation
 * API. Importing '../../extension/graphPanel' here instead would load a second
 * copy: the host runs the esbuild bundle, so the module-level
 * GraphPanel.currentPanel a test saw would not be the panel the extension
 * opened, and creating one would re-register the panel's commands and throw.
 * The import below is type-only, so it is erased and loads nothing at runtime.
 */
export async function graphPanelModule(): Promise<typeof GraphPanelNamespace> {
	const api = await activate();
	if (!api?.graphPanel) {
		throw new Error(
			"extension activated without exporting its API; cannot reach GraphPanel",
		);
	}
	return api.graphPanel();
}

/**
 * Shared small test utilities to reduce duplication across suites
 */
export async function wait(ms: number): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

export async function retryAsync(
	fn: () => boolean | Promise<boolean>,
	maxRetries = 3,
	delayMs = 500,
): Promise<boolean> {
	for (let attempt = 1; attempt <= maxRetries; attempt++) {
		try {
			const result = await fn();
			if (result === true) {
				return true;
			}
		} catch (err) {
			if (attempt === maxRetries) {
				throw err;
			}
		}
		if (attempt < maxRetries) {
			await wait(delayMs);
		}
	}
	return false;
}

export async function openDocumentAndShow(
	uri: vscode.Uri,
): Promise<vscode.TextDocument> {
	const doc = await vscode.workspace.openTextDocument(uri);
	await vscode.window.showTextDocument(doc);
	return doc;
}

/**
 * Wait for the language server to respond to completion requests. This
 * indicates the server has finished its first pass and is providing LSP
 * features (not just text fallback).
 */
export async function waitForLSP(
	uri: vscode.Uri,
	maxRetries = 60,
	delayMs = 500,
): Promise<void> {
	for (let attempt = 1; attempt <= maxRetries; attempt++) {
		try {
			const completions =
				await vscode.commands.executeCommand<vscode.CompletionList>(
					"vscode.executeCompletionItemProvider",
					uri,
					new vscode.Position(12, 0),
				);
			if (completions?.items?.length) {
				const hasLspCompletions = completions.items.some(
					(item) => (item.kind || 0) !== 0,
				);
				if (hasLspCompletions) {
					console.log(
						`LSP ready after ${attempt} attempts (${attempt * delayMs}ms) — found ${completions.items.length} completions`,
					);
					return;
				}
			}
		} catch (error) {
			console.log(
				`LSP check attempt ${attempt} failed:`,
				error instanceof Error ? error.message : String(error),
			);
		}
		if (attempt < maxRetries) {
			await wait(delayMs);
		}
	}
	throw new Error(
		`LSP not ready after ${maxRetries} attempts (${maxRetries * delayMs}ms total)`,
	);
}

/**
 * Wait for the language server to respond to hover requests at any position.
 * Cheaper than waitForLSP; use when a test only needs hover, not completions.
 * `[]` (provider registered, no hover at 0,0) is considered ready —
 * `undefined` means no hover provider yet, which is the pre-index signal.
 */
export async function waitForLanguageServer(
	uri: vscode.Uri,
	maxRetries = 30,
	delayMs = 500,
): Promise<boolean> {
	for (let attempt = 1; attempt <= maxRetries; attempt++) {
		try {
			const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
				"vscode.executeHoverProvider",
				uri,
				new vscode.Position(0, 0),
			);
			if (hovers !== undefined) {
				console.log(
					`Language server ready after ${attempt} attempts (${attempt * delayMs}ms)`,
				);
				return true;
			}
		} catch (error) {
			console.log(
				`LSP check attempt ${attempt} failed:`,
				error instanceof Error ? error.message : error,
			);
		}
		if (attempt < maxRetries) {
			await wait(delayMs);
		}
	}
	return false;
}
