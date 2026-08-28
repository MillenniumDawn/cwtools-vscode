/* --------------------------------------------------------------------------------------------
 * Copyright (c) Microsoft Corporation. All rights reserved.
 * Licensed under the MIT License. See License.txt in the project root for license information.
 * ------------------------------------------------------------------------------------------ */

import * as path from "path";
import * as os from "os";
import * as fsPromises from "fs/promises";
import * as vscode from "vscode";
import type { ExtensionContext } from "vscode";
import { workspace, window, commands, l10n } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";

import { serverExe as resolveServerExe } from "./engine";
import { detectGameAndVanilla } from "./detectGame";
import { resolveRulesCache, fetchRulesInBackground } from "./rulesSetup";
import { createLanguageClient } from "./lspClient";
import { registerServerNotifications } from "./serverNotifications";
import { registerDocumentLanguage } from "./documentLanguage";
import { registerCommands, publishCommandAvailability } from "./commands";
import { setTrustedRoots } from "./trustedPaths";
import { logInfo, logError, errorMessage } from "./logger";
import { showServerBlockedDialog } from "./serverBlockedDialog";
import type * as GraphPanelModule from "./graphPanel";

export let defaultClient: LanguageClient;
// The dir resolveRulesCache/fetchRulesInBackground clone into, keyed by
// language below it. Exposed via CwtoolsApi so the rules-sync host suite
// can find what activation chose without guessing globalStorage's path.
let rulesCacheRoot: string | undefined;
// The status item's text getter, once registerServerNotifications has run.
let statusText: (() => string | undefined) | undefined;

// What activate() hands back to other extensions and to the host tests. The
// tests can't import graphPanel.ts directly: the extension host runs the
// esbuild bundle, so a direct import would give them a second copy of the
// module with its own GraphPanel.currentPanel, and the panel they inspect
// would never be the one the extension opened.
export interface CwtoolsApi {
	graphPanel(): Promise<typeof GraphPanelModule>;
	/** Commands the running server advertised, empty if it never started. */
	serverCommands(): readonly string[];
	/** The rules cache root dir, once activation has resolved it. */
	rulesCacheRoot(): string | undefined;
	/** The status bar item's current text, once activation has created it. */
	serverStatusText(): string | undefined;
}

export async function activate(context: ExtensionContext): Promise<CwtoolsApi> {
	void commands.executeCommand("setContext", "cwtoolsEnabled", true);
	// The editor/title graph button is gated on `cwtoolsWebview == false`, which an
	// unset key does not satisfy. Only GraphPanel ever wrote this key, so without a
	// seed the button could not appear until a panel had opened and closed once.
	void commands.executeCommand("setContext", "cwtoolsWebview", false);

	// Writable, per-extension cache dir. globalStorage survives extension
	// updates and is writable everywhere; the install dir (extensionPath) is
	// wiped on every update and can be read-only.
	const cacheDir = path.join(context.globalStorageUri.fsPath, ".cwtools");
	rulesCacheRoot = cacheDir;

	const init = async function (language: string) {
		// Include `.` in the word pattern so a dotted event/decision id
		// (`namespace.1`) selects whole on double-click and resolves via
		// go-to-definition, instead of splitting at the dot. (#39)
		const langConfigDisposable = vscode.languages.setLanguageConfiguration(
			"paradox",
			{ wordPattern: /"?([^\s]+)"?/ },
		);
		context.subscriptions.push(langConfigDisposable);

		// The Rust language server, bundled per-platform. Resolve the binary for
		// this platform or bail with a clear message.
		const serverExe = resolveServerExe(context);
		if (!serverExe) {
			// Not awaited: an error notification stays up until it is dismissed, so
			// awaiting it parks activation forever wherever nobody can click it,
			// which is exactly the headless extension host the tests run in.
			void window.showErrorMessage(
				l10n.t(
					"CWTools: no language server binary found. Re-install the extension or build the server.",
				),
			);
			return;
		}
		logInfo(`Using server: ${serverExe}`);

		if (os.platform() !== "win32") {
			try {
				const stat = await fsPromises.stat(serverExe);
				if ((stat.mode & 0o111) === 0) {
					await fsPromises.chmod(serverExe, 0o755);
				}
			} catch (e) {
				logError("stat/chmod error on server binary", e);
			}
		}

		const { rulesCache, fetchUpstream } = await resolveRulesCache(
			language,
			cacheDir,
		);

		// Where a location the server reports is allowed to point. Anything else
		// needs the user to confirm before it opens (see trustedPaths.ts).
		setTrustedRoots([
			cacheDir,
			rulesCache,
			workspace.getConfiguration("cwtools").get<string>(`cache.${language}`),
		]);

		// registerServerNotifications (below) owns markStopped, but the
		// errorHandler that calls it has to be in clientOptions before the
		// client exists, so this holder lets the client be created first.
		const stopped: { notify?: () => void } = {};
		const client = createLanguageClient(
			context,
			{ language, serverExe, cacheDir, rulesCache },
			() => stopped.notify?.(),
		);
		defaultClient = client;
		client.registerProposedFeatures();

		const tracker = await registerDocumentLanguage(context, client, "paradox");
		const notifications = registerServerNotifications(context, client);
		const initialScanDone = notifications.initialScanDone;
		statusText = notifications.statusText;
		stopped.notify = notifications.markStopped;

		if (workspace.name === undefined) {
			void window.showWarningMessage(
				l10n.t(
					'You have opened a file directly.\n\rFor CWTools to work correctly, the mod folder should be opened using "File, Open Folder"',
				),
			);
		}

		registerCommands(context, client, tracker, serverExe);

		// Subscriptions are pushed here so the client is disposed with the extension.
		context.subscriptions.push(client);
		try {
			await client.start();
			// Capabilities are only known once the server has answered initialize.
			publishCommandAvailability(client);
			// Classify the already-focused editor now that getFileTypes can be
			// answered. Not awaited: activation shouldn't wait on a server round-trip.
			void tracker.classifyActiveEditor();
			// Clone/pull the rules repo without blocking activation; the server
			// reloads its rules once the fetch lands (see rulesSetup.ts).
			if (fetchUpstream) {
				fetchRulesInBackground(
					language,
					cacheDir,
					client,
					initialScanDone,
					context.globalState,
				);
			}
		} catch (err) {
			const msg = errorMessage(err);
			logError("client.start() error", err);
			if (showServerBlockedDialog(err, serverExe)) {
				return;
			}
			window.showErrorMessage(
				l10n.t("CWTools language server failed to start: {0}", msg),
			);
			return;
		}
	};

	const { languageId } = await detectGameAndVanilla();

	await init(languageId);

	return {
		graphPanel: () => import("./graphPanel"),
		serverCommands: () =>
			defaultClient?.initializeResult?.capabilities.executeCommandProvider
				?.commands ?? [],
		rulesCacheRoot: () => rulesCacheRoot,
		serverStatusText: () => statusText?.(),
	};
}
