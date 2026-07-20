import type { ExtensionContext, StatusBarItem } from 'vscode';
import { window, StatusBarAlignment } from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import { NotificationType } from 'vscode-languageclient/node';
import type { FileListItem } from './fileExplorer';
import { FileExplorer } from './fileExplorer';
import { fileListSignature } from './fileListSignature';

interface LoadingBarParams { enable: boolean; value: string }
interface UpdateFileList { fileList: FileListItem[] }

export function registerServerNotifications(context: ExtensionContext, client: LanguageClient): Promise<void> {
	const loadingBarNotification = new NotificationType<LoadingBarParams>('loadingBar');
	const updateFileList = new NotificationType<UpdateFileList>('updateFileList');
	let status: StatusBarItem | undefined;
	let fileExplorer : FileExplorer;
	let lastFileListSignature: string | undefined;
	let initialScanStarted = false;
	let initialScanPending = true;
	let resolveInitialScan: () => void;
	const initialScanDone = new Promise<void>(resolve => resolveInitialScan = resolve);

	client.onNotification(loadingBarNotification, (param: LoadingBarParams) => {
		if (param.enable) {
			if (initialScanPending) {
				initialScanStarted = true;
			}
			// One persistent item updated in place: a scan emits many progress
			// ticks, and dispose+recreate per tick makes the status bar churn.
			if (status === undefined) {
				status = window.createStatusBarItem(StatusBarAlignment.Left);
				context.subscriptions.push(status);
			}
			status.text = param.value;
			status.show();
		}
		else {
			status?.hide();
			if (initialScanPending && initialScanStarted) {
				initialScanPending = false;
				resolveInitialScan();
			}
		}
	})
	client.onNotification(updateFileList, (params: UpdateFileList) => {
		const signature = fileListSignature(params.fileList);
		if (!fileExplorer) {
			fileExplorer = new FileExplorer(context, params.fileList);
			lastFileListSignature = signature;
		}
		else if (lastFileListSignature !== signature) {
			fileExplorer.refresh(params.fileList);
			lastFileListSignature = signature;
		}
	})
	return initialScanDone;
}
