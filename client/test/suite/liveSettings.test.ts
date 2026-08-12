import * as assert from "assert";
import * as path from "path";
import * as vscode from "vscode";
import { activate, SAMPLE_ROOT, waitForLanguageServer } from "../support/utils";

const settingsFile = path.join(
	SAMPLE_ROOT,
	"common/live_settings/cwtools_live_settings.txt",
);
const localisationPosition = new vscode.Position(1, 9);
const ownerPosition = new vscode.Position(2, 2);

async function hoverText(
	uri: vscode.Uri,
	position: vscode.Position,
): Promise<string> {
	const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
		"vscode.executeHoverProvider",
		uri,
		position,
	);
	return (hovers ?? [])
		.flatMap((hover) => hover.contents)
		.map((content) =>
			typeof content === "string" ? content : content.value,
		)
		.join("\n");
}

async function waitForHover(
	uri: vscode.Uri,
	position: vscode.Position,
	predicate: (text: string) => boolean,
	message: string,
): Promise<string> {
	let lastText = "";
	for (let attempt = 0; attempt < 30; attempt++) {
		const text = await hoverText(uri, position);
		lastText = text;
		if (predicate(text)) {
			return text;
		}
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	throw new Error(`${message}: ${lastText}`);
}

suite("Live settings", function () {
	this.timeout(60_000);

	let document: vscode.TextDocument;
	let config: vscode.WorkspaceConfiguration;

	suiteSetup(async () => {
		await activate();
		document = await vscode.workspace.openTextDocument(settingsFile);
		await vscode.window.showTextDocument(document);
		const ready = await waitForLanguageServer(document.uri, 60, 100);
		assert.ok(ready, "language server should be ready");
		config = vscode.workspace.getConfiguration("cwtools");
	});

	suiteTeardown(async () => {
		await config.update("localisation.languages", undefined);
		await config.update("localisation.hoverShowAllLanguages", undefined);
		await config.update("hover.debug", undefined);
		await config.update("hover.scopeDisplay", undefined);
		await vscode.commands.executeCommand("workbench.action.closeAllEditors");
	});

	test("updates localisation language in the running server", async () => {
		await config.update("localisation.languages", ["English"]);
		await waitForHover(
			document.uri,
			localisationPosition,
			(text) => text.includes("English setting") && !text.includes("Paramètre français"),
			"English localisation should be the only hover text before the update",
		);

		await config.update("localisation.languages", ["French"]);
		const text = await waitForHover(
			document.uri,
			localisationPosition,
			(text) => text.includes("Paramètre français") && !text.includes("English setting"),
			"French localisation should replace English without restarting the server",
		);
		assert.ok(text.includes("Paramètre français"));
	});

	test("updates all-language hovers in the running server", async () => {
		await config.update("localisation.languages", ["French"]);
		await config.update("localisation.hoverShowAllLanguages", false);
		await waitForHover(
			document.uri,
			localisationPosition,
			(text) => text.includes("Paramètre français") && !text.includes("English setting"),
			"only the selected localisation language should be visible before the update",
		);

		await config.update("localisation.hoverShowAllLanguages", true);
		const text = await waitForHover(
			document.uri,
			localisationPosition,
			(text) => text.includes("Paramètre français") && text.includes("English setting"),
			"all configured localisation languages should appear without restarting the server",
		);
		assert.ok(text.includes("English setting"));
	});

	test("updates debug hover output in the running server", async () => {
		await config.update("hover.debug", false);
		await waitForHover(
			document.uri,
			localisationPosition,
			(text) => !text.includes("**Localisation key** —"),
			"debug classification should be hidden before the update",
		);

		await config.update("hover.debug", true);
		const text = await waitForHover(
			document.uri,
			localisationPosition,
			(text) => text.includes("**Localisation key** —"),
			"debug classification should appear without restarting the server",
		);
		assert.ok(text.includes("**Localisation key** —"));
	});

	test("updates resolved scope hover output in the running server", async () => {
		await config.update("hover.scopeDisplay", "context");
		await waitForHover(
			document.uri,
			ownerPosition,
			(text) => !text.includes("**Resolves to**: state"),
			"resolved scope should be hidden before the update",
		);

		await config.update("hover.scopeDisplay", "resolved");
		const text = await waitForHover(
			document.uri,
			ownerPosition,
			(text) => text.includes("**Resolves to**: state"),
			"resolved scope should appear without restarting the server",
		);
		assert.ok(text.includes("**Resolves to**: state"));
	});
});
