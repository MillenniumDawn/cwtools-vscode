import * as assert from "assert";
import * as fs from "fs/promises";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";
import {
	activate,
	SAMPLE_ROOT,
	serverOutputChannel,
	waitForLanguageServer,
	waitUntil,
} from "../support/utils";

const settingsFile = path.join(
	SAMPLE_ROOT,
	"common/live_settings/cwtools_live_settings.txt",
);
const workspaceRulesFolder = ".cwtools-test-rules";
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
		.map((content) => (typeof content === "string" ? content : content.value))
		.join("\n");
}

async function waitForHover(
	uri: vscode.Uri,
	position: vscode.Position,
	predicate: (text: string) => boolean,
	message: string,
): Promise<string> {
	let lastText = "";
	// didChangeConfiguration triggers a full loc re-index that can exceed 3 s on
	// a CI runner with a cold cache, so the budget stays generous even though
	// the poll is fine-grained.
	const matched = await waitUntil(async () => {
		lastText = await hoverText(uri, position);
		return predicate(lastText);
	}, 15_000);
	if (!matched) {
		throw new Error(`${message}: ${JSON.stringify(lastText)}`);
	}
	return lastText;
}

suite("Live settings", function () {
	this.timeout(60_000);

	let document: vscode.TextDocument;

	function cwtoolsConfig(): vscode.WorkspaceConfiguration {
		return vscode.workspace.getConfiguration("cwtools");
	}

	async function resetLiveSettings(): Promise<void> {
		const cfg = cwtoolsConfig();
		const target = vscode.ConfigurationTarget.Workspace;
		await cfg.update("localisation.languages", undefined, target);
		await cfg.update("localisation.hoverShowAllLanguages", undefined, target);
		await cfg.update("hover.debug", undefined, target);
		await cfg.update("hover.scopeDisplay", undefined, target);
	}

	async function updateLiveSetting(
		section: string,
		value: unknown,
	): Promise<void> {
		await cwtoolsConfig().update(
			section,
			value,
			vscode.ConfigurationTarget.Workspace,
		);
	}

	suiteSetup(async () => {
		const cfg = cwtoolsConfig();
		await activate();
		assert.notStrictEqual(
			cfg.get<string>("rules_folder"),
			workspaceRulesFolder,
			"workspace-supplied rules_folder should not be effective",
		);
		document = await vscode.workspace.openTextDocument(settingsFile);
		await vscode.window.showTextDocument(document);
		const ready = await waitForLanguageServer(document.uri);
		assert.ok(ready, "language server should be ready");
		// Start from a known baseline so the first polling assertion is not flaky.
		await resetLiveSettings();
		await waitForHover(
			document.uri,
			localisationPosition,
			(text) => text.length > 0,
			"initial hover should be available",
		);
	});

	suiteTeardown(async () => {
		await resetLiveSettings();
		await vscode.commands.executeCommand("workbench.action.closeAllEditors");
	});

	setup(async () => {
		await resetLiveSettings();
	});

	teardown(async () => {
		await resetLiveSettings();
	});

	test("updates localisation language in the running server", async () => {
		await updateLiveSetting("localisation.languages", ["English"]);
		await waitForHover(
			document.uri,
			localisationPosition,
			(text) =>
				text.includes("English setting") &&
				!text.includes("Paramètre français"),
			"English localisation should be the only hover text before the update",
		);

		await updateLiveSetting("localisation.languages", ["French"]);
		const text = await waitForHover(
			document.uri,
			localisationPosition,
			(text) =>
				text.includes("Paramètre français") &&
				!text.includes("English setting"),
			"French localisation should replace English without restarting the server",
		);
		assert.ok(text.includes("Paramètre français"));
	});

	test("updates all-language hovers in the running server", async () => {
		await updateLiveSetting("localisation.languages", ["French"]);
		await updateLiveSetting("localisation.hoverShowAllLanguages", false);
		await waitForHover(
			document.uri,
			localisationPosition,
			(text) =>
				text.includes("Paramètre français") &&
				!text.includes("English setting"),
			"only the selected localisation language should be visible before the update",
		);

		await updateLiveSetting("localisation.hoverShowAllLanguages", true);
		const text = await waitForHover(
			document.uri,
			localisationPosition,
			(text) =>
				text.includes("Paramètre français") && text.includes("English setting"),
			"all configured localisation languages should appear without restarting the server",
		);
		assert.ok(text.includes("English setting"));
	});

	test("updates debug hover output in the running server", async () => {
		await updateLiveSetting("hover.debug", false);
		await waitForHover(
			document.uri,
			localisationPosition,
			(text) => !text.includes("**Localisation key** —"),
			"debug classification should be hidden before the update",
		);

		await updateLiveSetting("hover.debug", true);
		const text = await waitForHover(
			document.uri,
			localisationPosition,
			(text) => text.includes("**Localisation key** —"),
			"debug classification should appear without restarting the server",
		);
		assert.ok(text.includes("**Localisation key** —"));
	});

	test("switches rules folders and watches external rules files live", async function () {
		this.timeout(45_000);
		const cfg = cwtoolsConfig();
		assert.strictEqual(cfg.get<boolean>("rules.autoReload"), true);
		const previousFolder = cfg.get<string>("rules_folder");
		const previousTestFolder = process.env.CWTOOLS_TEST_RULES_FOLDER;
		const rulesRoot = await fs.mkdtemp(
			path.join(os.tmpdir(), "cwtools-rules-live-"),
		);
		const rulesFile = path.join(rulesRoot, "live.cwt");
		const output = await serverOutputChannel();
		const messages: string[] = [];
		const info = output.info;
		output.info = new Proxy(info, {
			apply(target, thisArg, args) {
				if (typeof args[0] === "string") messages.push(args[0]);
				Reflect.apply(target, thisArg, args);
			},
		});
		const matchingLoads = () =>
			messages.filter((message) =>
				message.includes(`Loaded rules from ${rulesRoot}`),
			);
		const watcherReadyMessage = `Watching rules in ${rulesRoot}`;
		try {
			await fs.writeFile(
				rulesFile,
				'types = { type[first] = { path = "common/first" } }\n',
			);
			process.env.CWTOOLS_TEST_RULES_FOLDER = rulesRoot;
			await cfg.update(
				"rules_folder",
				rulesRoot,
				vscode.ConfigurationTarget.Global,
			);
			assert.ok(
				await waitUntil(() => matchingLoads().length >= 1, 15_000),
				"a valid rules_folder change should load rules without restarting",
			);
			assert.ok(
				await waitUntil(() => messages.includes(watcherReadyMessage), 15_000),
				"the external rules watcher should be registered before the file edit",
			);
			await fs.writeFile(
				rulesFile,
				'types = { type[second] = { path = "common/second" } }\n',
			);
			assert.ok(
				await waitUntil(() => matchingLoads().length >= 2, 15_000),
				"an edit under the external rules folder should trigger one rules reload",
			);
		} finally {
			process.env.CWTOOLS_TEST_RULES_FOLDER = previousTestFolder;
			await cfg.update(
				"rules_folder",
				previousFolder,
				vscode.ConfigurationTarget.Global,
			);
			await fs.rm(rulesRoot, { recursive: true, force: true });
			output.info = info;
		}
	});

	test("updates resolved scope hover output in the running server", async () => {
		await updateLiveSetting("hover.scopeDisplay", "context");
		await waitForHover(
			document.uri,
			ownerPosition,
			(text) => !text.includes("**Resolves to**: state"),
			"resolved scope should be hidden before the update",
		);

		await updateLiveSetting("hover.scopeDisplay", "resolved");
		const text = await waitForHover(
			document.uri,
			ownerPosition,
			(text) => text.includes("**Resolves to**: state"),
			"resolved scope should appear without restarting the server",
		);
		assert.ok(text.includes("**Resolves to**: state"));
	});
});
