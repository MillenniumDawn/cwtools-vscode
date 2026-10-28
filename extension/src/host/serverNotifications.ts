import type { ExtensionContext } from "vscode";
import * as path from "node:path";
import { window, l10n, commands, StatusBarAlignment, Uri } from "vscode";
import type {
	LanguageClient,
	StateChangeEvent,
} from "vscode-languageclient/node";
import { State } from "vscode-languageclient/node";

import { commandProgressActive } from "./commandProgress";
import {
	clearCommandAvailability,
	publishCommandAvailability,
} from "./commands";
import type { FileListItem } from "./fileExplorer";
import { FileExplorer } from "./fileExplorer";
import { fileListSignature } from "./fileListSignature";
import { logError } from "./logger";

interface LoadingBarParams {
	enable: boolean;
	value: string;
	/** 0-100, sent by servers new enough to know where they are. */
	percentage?: number;
}
interface DiagnosticsBaselineStatus {
	suppressed: number;
}
interface UpdateFileList {
	fileList: FileListItem[];
	/** Effective parent roots and canonical spellings, as file URIs. */
	parentRoots?: string[];
	/** Configured/discovered and canonical spellings of the install, as file URIs. */
	vanillaRoots?: string[];
}

export interface ServerNotifications {
	initialScanDone: Promise<void>;
	/** The status bar item's current text, for the host tests. */
	statusText: () => string | undefined;
	/** Called once the client's error handler gives up restarting. */
	markStopped: () => void;
}

export function registerServerNotifications(
	context: ExtensionContext,
	client: LanguageClient,
	updateServerContentRoots?: (
		parentRoots: readonly string[],
		vanillaRoots: readonly string[],
	) => Promise<void>,
): ServerNotifications {
	const loadingBarNotification = "loadingBar";
	const updateFileList = "updateFileList";
	let fileExplorer: FileExplorer;
	let lastFileListSignature: string | undefined;
	let initialScanStarted = false;
	let initialScanPending = true;
	let acceptFileList = true;
	let suppressedDiagnostics = 0;
	let ready = false;
	let resolveInitialScan: () => void;
	const initialScanDone = new Promise<void>(
		(resolve) => (resolveInitialScan = resolve),
	);

	// Persistent and visible even when idle, rather than the scan-only item
	// this used to be, so restart/stopped state has somewhere to show.
	const status = window.createStatusBarItem(StatusBarAlignment.Left);
	status.command = "cwtools.statusBarMenu";
	status.text = l10n.t("CWTools: starting");
	context.subscriptions.push(status);
	status.show();

	context.subscriptions.push(
		// Literal id so the commands.test.ts namespace scan sees it.
		commands.registerCommand("cwtools.statusBarMenu", async () => {
			const restart = l10n.t("Restart Server");
			const showOutput = l10n.t("Show Output");
			const choice = await window.showQuickPick([restart, showOutput]);
			if (choice === restart) {
				await commands.executeCommand("cwtools.restartServer");
			} else if (choice === showOutput) {
				await commands.executeCommand("cwtools.showOutput");
			}
		}),
	);

	const markStopped = (): void => {
		acceptFileList = false;
		setServerContentRoots([], []);
		suppressedDiagnostics = 0;
		ready = false;
		status.text = l10n.t("CWTools: stopped");
		clearCommandAvailability();
	};
	const setServerContentRoots = (
		parentRoots: readonly string[],
		vanillaRoots: readonly string[],
	): void => {
		if (updateServerContentRoots) {
			void updateServerContentRoots(parentRoots, vanillaRoots).catch((err: unknown) =>
				logError("Failed to classify game-content documents", err),
			);
		}
	};

	context.subscriptions.push(
		client.onDidChangeState((e: StateChangeEvent) => {
			if (e.newState === State.Starting) {
				acceptFileList = false;
				setServerContentRoots([], []);
				suppressedDiagnostics = 0;
				ready = false;
				status.text = l10n.t("CWTools: starting");
			} else if (e.newState === State.Running) {
				acceptFileList = true;
				// The library sets initializeResult just before this event fires,
				// so this covers activation, manual restarts and automatic ones.
				publishCommandAvailability(client);
			} else if (
				e.newState === State.Stopped ||
				e.newState === State.StartFailed
			) {
				markStopped();
			}
		}),
	);

	const loadingBarListener = client.onNotification(
		loadingBarNotification,
		(param: LoadingBarParams) => {
			if (param.enable) {
				ready = false;
				if (initialScanPending) {
					initialScanStarted = true;
				}
				// A command notification is showing the same phases with a cancel
				// button attached; a status-bar copy alongside it is the third
				// indicator for one operation (cwtools-vscode#145). Leave the
				// persistent item's text as-is rather than duplicating the phase.
				if (commandProgressActive()) {
					return;
				}
				status.text =
					typeof param.percentage === "number"
						? `${param.value} ${param.percentage}%`
						: param.value;
			} else {
				ready = true;
				status.text = readyStatusText();
				if (initialScanPending && initialScanStarted) {
					initialScanPending = false;
					resolveInitialScan();
				}
			}
		},
	);
	const readyStatusText = (): string =>
		suppressedDiagnostics > 0
			? l10n.t("CWTools: ready ({0} baseline-suppressed)", suppressedDiagnostics)
			: l10n.t("CWTools: ready");
	context.subscriptions.push(client.onNotification("diagnosticsBaselineStatus", (params: DiagnosticsBaselineStatus) => {
		if (!acceptFileList || typeof params?.suppressed !== "number" || !Number.isSafeInteger(params.suppressed) || params.suppressed < 0) return;
		suppressedDiagnostics = params.suppressed;
		if (ready) {
			status.text = readyStatusText();
		}
	}));
	const updateFileListListener = client.onNotification(updateFileList, (params: UpdateFileList) => {
		if (!acceptFileList) return;
		const parseRoots = (rootList: unknown): string[] => {
			const roots: string[] = [];
			if (!Array.isArray(rootList)) return roots;
			for (const root of rootList) {
				// Uri.parse makes empty/relative file paths absolute. Require the
				// explicit hierarchical path the server produces before parsing.
				if (typeof root !== "string" || !/^file:\/\/[^/?#]*\//.test(root)) continue;
				try {
					const uri = Uri.parse(root, true);
					if (uri.scheme === "file" && path.isAbsolute(uri.fsPath)) roots.push(uri.fsPath);
				} catch {
					// Ignore malformed root URIs from incompatible server versions.
				}
			}
			return roots;
		};
		setServerContentRoots(
			parseRoots(params.parentRoots),
			parseRoots(params.vanillaRoots),
		);
		const signature = fileListSignature(params.fileList);
		if (!fileExplorer) {
			fileExplorer = new FileExplorer(context, params.fileList);
			// The explorer owns the tree provider's EventEmitter; dispose it with
			// the extension like every other emitter in the codebase.
			context.subscriptions.push(fileExplorer);
			lastFileListSignature = signature;
		} else if (lastFileListSignature !== signature) {
			fileExplorer.refresh(params.fileList);
			lastFileListSignature = signature;
		}
	});
	// The library wraps each onNotification in a Disposable; hooking these off
	// context.subscriptions (like diagnosticsBaselineStatus above) stops the
	// handlers firing into a disposed extension.
	context.subscriptions.push(loadingBarListener, updateFileListListener);
	return {
		initialScanDone,
		statusText: () => status.text,
		markStopped,
	};
}
