import type { ExtensionContext, StatusBarItem } from "vscode";
import { window, StatusBarAlignment } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";

import type { FileListItem } from "./fileExplorer";
import { FileExplorer } from "./fileExplorer";
import { fileListSignature } from "./fileListSignature";
import { commandProgressActive } from "./commandProgress";

interface LoadingBarParams {
	enable: boolean;
	value: string;
	/** 0-100, sent by servers new enough to know where they are. */
	percentage?: number;
}
interface UpdateFileList {
	fileList: FileListItem[];
}

export function registerServerNotifications(
	context: ExtensionContext,
	client: LanguageClient,
): Promise<void> {
	const loadingBarNotification = "loadingBar";
	const updateFileList = "updateFileList";
	let status: StatusBarItem | undefined;
	let fileExplorer: FileExplorer;
	let lastFileListSignature: string | undefined;
	let initialScanStarted = false;
	let initialScanPending = true;
	let resolveInitialScan: () => void;
	const initialScanDone = new Promise<void>(
		(resolve) => (resolveInitialScan = resolve),
	);

	client.onNotification(loadingBarNotification, (param: LoadingBarParams) => {
		if (param.enable) {
			if (initialScanPending) {
				initialScanStarted = true;
			}
			// A command notification is showing the same phases with a cancel
			// button attached; a status-bar copy alongside it is the third
			// indicator for one operation (cwtools-vscode#145). The scan-started
			// bookkeeping above still runs, so the activation gate is unaffected.
			if (commandProgressActive()) {
				status?.hide();
				return;
			}
			// One persistent item updated in place: a scan emits many progress
			// ticks, and dispose+recreate per tick makes the status bar churn.
			if (status === undefined) {
				status = window.createStatusBarItem(StatusBarAlignment.Left);
				context.subscriptions.push(status);
			}
			status.text =
				typeof param.percentage === "number"
					? `${param.value} ${param.percentage}%`
					: param.value;
			status.show();
		} else {
			status?.hide();
			if (initialScanPending && initialScanStarted) {
				initialScanPending = false;
				resolveInitialScan();
			}
		}
	});
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
	return initialScanDone;
}
