import * as fs from "node:fs";
import * as vscode from "vscode";
import * as path from "path";
import type { CwtoolsApi } from "../../src/host/extension";
import type * as GraphPanelNamespace from "../../src/host/graphPanel";

// Re-exported so host suites can keep importing it from here. The function
// itself lives in a vscode-free module so the parity harness can share it.
export { extractCompletionLabel } from "./labels";

// The published extension id: publisher.name from extension/package/package.json.
export const EXTENSION_ID = "milleniumdawnmodteam.cwtools-md-edition";

/** Resolved path to the sample mod used by all test suites.
 *
 * The workspace in .vscode-test.mjs is the source tree under
 * extension/test/workspaces/stellaris, not the compiled copy under dist/.
 * Resolve from the workspace folder when available so the path works both
 * from the compiled host tests and from TypeScript source.
 */
export const SAMPLE_ROOT = (() => {
	const ws = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
	if (ws && fs.existsSync(path.join(ws, "events/irm.txt"))) {
		return ws;
	}
	const candidates = [
		path.resolve(
			__dirname,
			"../../../../../../extension/test/workspaces/stellaris",
		),
		path.resolve(__dirname, "../workspaces/stellaris"),
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
 * API. Importing '../../src/host/graphPanel' here instead would load a second
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

export async function serverOutputChannel(): Promise<
	Pick<vscode.OutputChannel, "appendLine">
> {
	const api = await activate();
	const outputChannel = api?.serverOutputChannel();
	if (!outputChannel) {
		throw new Error(
			"extension activated without a running language client output channel",
		);
	}
	return outputChannel;
}

/**
 * Shared small test utilities to reduce duplication across suites
 */
export async function wait(ms: number): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Poll `probe` until it returns true, or the deadline passes.
 *
 * Poll granularity and time budget are separate on purpose. A retry count
 * multiplied by a coarse delay ties them together, so every wait pays the
 * full delay even when the condition is already met a few ms later, and
 * raising the budget for a loaded CI runner makes the fast path slower too.
 */
export async function waitUntil(
	probe: () => boolean | Promise<boolean>,
	timeoutMs = 10_000,
	intervalMs = 25,
): Promise<boolean> {
	const deadline = Date.now() + timeoutMs;
	for (;;) {
		try {
			if (await probe()) {
				return true;
			}
		} catch (_e) {
			// Transient while the host settles; report the timeout, not the throw.
		}
		if (Date.now() >= deadline) {
			return false;
		}
		await wait(intervalMs);
	}
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
	timeoutMs = 30_000,
): Promise<void> {
	const ready = await waitUntil(async () => {
		const completions =
			await vscode.commands.executeCommand<vscode.CompletionList>(
				"vscode.executeCompletionItemProvider",
				uri,
				new vscode.Position(12, 0),
			);
		// kind 0 is Text, the word-completion fallback VS Code supplies with no
		// server behind it; anything else means the server answered.
		return !!completions?.items?.some((item) => (item.kind || 0) !== 0);
	}, timeoutMs);
	if (!ready) {
		throw new Error(`LSP not ready within ${timeoutMs}ms`);
	}
}

/**
 * Wait for the language server to respond to hover requests at any position.
 * Cheaper than waitForLSP; use when a test only needs hover, not completions.
 * `[]` (provider registered, no hover at 0,0) is considered ready —
 * `undefined` means no hover provider yet, which is the pre-index signal.
 */
export async function waitForLanguageServer(
	uri: vscode.Uri,
	timeoutMs = 15_000,
): Promise<boolean> {
	return waitUntil(async () => {
		const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
			"vscode.executeHoverProvider",
			uri,
			new vscode.Position(0, 0),
		);
		return hovers !== undefined;
	}, timeoutMs);
}
