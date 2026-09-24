import type { ExtensionContext } from "vscode";
import { window, l10n, commands, StatusBarAlignment } from "vscode";
import type {
	LanguageClient,
	StateChangeEvent,
} from "vscode-languageclient/node";
import { State } from "vscode-languageclient/node";

import { commandProgressActive } from "./commandProgress";
import { clearCommandAvailability } from "./commands";
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
interface UpdateFileList {
	fileList: FileListItem[];
}

interface WorkspaceDiagnosticsBudgetReached {
	budget: number;
	heldBack: number;
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
): ServerNotifications {
	const loadingBarNotification = "loadingBar";
	const updateFileList = "updateFileList";
	let fileExplorer: FileExplorer;
	let lastFileListSignature: string | undefined;
	let initialScanStarted = false;
	let initialScanPending = true;
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
		status.text = l10n.t("CWTools: stopped");
		clearCommandAvailability();
	};

	context.subscriptions.push(
		// onDidChangeState is a lib getter returning Event<StateChangeEvent>;
		// type-aware lint resolves it as unsafe under skipLibCheck though tsc
		// types it (client, the handler and the returned Disposable are typed).
		// eslint-disable-next-line @typescript-eslint/no-unsafe-call, @typescript-eslint/no-unsafe-argument
		client.onDidChangeState((e: StateChangeEvent) => {
			if (e.newState === State.Starting) {
				status.text = l10n.t("CWTools: starting");
			} else if (
				e.newState === State.Stopped ||
				e.newState === State.StartFailed
			) {
				markStopped();
			}
		}),
	);

	client.onNotification(loadingBarNotification, (param: LoadingBarParams) => {
		if (param.enable) {
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
			status.text = l10n.t("CWTools: ready");
			if (initialScanPending && initialScanStarted) {
				initialScanPending = false;
				resolveInitialScan();
			}
		}
	});
	client.onNotification(
		"workspaceDiagnosticsBudgetReached",
		(params: WorkspaceDiagnosticsBudgetReached) => {
			const showOutput = l10n.t("Show Output");
			void Promise.resolve(
				window.showInformationMessage(
					l10n.t(
						"CWTools: workspace diagnostics are limited to {0} closed files per scan; {1} more were held back.",
						params.budget,
						params.heldBack,
					),
					showOutput,
				),
			)
				.then((choice: string | undefined) => {
					if (choice === showOutput) {
						return commands.executeCommand("cwtools.showOutput");
					}
					return undefined;
				})
				.catch((err: unknown) =>
					logError(
						"Failed to open CWTools output after diagnostics budget warning",
						err,
					),
				);
		},
	);
	client.onNotification(updateFileList, (params: UpdateFileList) => {
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
	return {
		initialScanDone,
		statusText: () => status.text,
		markStopped,
	};
}
