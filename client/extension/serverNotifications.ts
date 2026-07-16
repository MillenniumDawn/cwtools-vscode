import type { ExtensionContext, Disposable } from 'vscode';
import { window } from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import { NotificationType } from 'vscode-languageclient/node';
import type { FileListItem } from './fileExplorer';
import { FileExplorer } from './fileExplorer';

interface LoadingBarParams { enable: boolean; value: string }
interface UpdateFileList { fileList: FileListItem[] }

export function registerServerNotifications(context: ExtensionContext, client: LanguageClient): Promise<void> {
	const loadingBarNotification = new NotificationType<LoadingBarParams>('loadingBar');
	const updateFileList = new NotificationType<UpdateFileList>('updateFileList');
	let status: Disposable | undefined;
	let fileExplorer : FileExplorer;
	let initialScanStarted = false;
	let initialScanPending = true;
	let resolveInitialScan: () => void;
	const initialScanDone = new Promise<void>(resolve => resolveInitialScan = resolve);

	client.onNotification(loadingBarNotification, (param: LoadingBarParams) => {
		if (param.enable) {
			if (initialScanPending) {
				initialScanStarted = true;
			}
			if (status !== undefined) {
				status.dispose();
			}
			status = window.setStatusBarMessage(param.value);
		}
		else {
			if (status !== undefined) {
				status.dispose();
				status = undefined;
			}
			if (initialScanPending && initialScanStarted) {
				initialScanPending = false;
				resolveInitialScan();
			}
		}
	})
	client.onNotification(updateFileList, (params: UpdateFileList) => {
		if (fileExplorer) {
			fileExplorer.refresh(params.fileList);
		}
		else {
			fileExplorer = new FileExplorer(context, params.fileList);
		}
	})
	return initialScanDone;
}
