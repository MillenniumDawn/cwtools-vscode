import type { ExtensionContext, Disposable } from 'vscode';
import { window } from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import { NotificationType } from 'vscode-languageclient/node';
import type { FileListItem } from './fileExplorer';
import { FileExplorer } from './fileExplorer';

interface LoadingBarParams { enable: boolean; value: string }
interface UpdateFileList { fileList: FileListItem[] }

export function registerServerNotifications(context: ExtensionContext, client: LanguageClient): void {
	const loadingBarNotification = new NotificationType<LoadingBarParams>('loadingBar');
	const updateFileList = new NotificationType<UpdateFileList>('updateFileList');
	let status: Disposable | undefined;
	let fileList : FileListItem[];
	let fileExplorer : FileExplorer;

	client.onNotification(loadingBarNotification, (param: LoadingBarParams) => {
		if (param.enable) {
			if (status !== undefined) {
				status.dispose();
			}
			status = window.setStatusBarMessage(param.value);
		}
		else if (status !== undefined) {
			status.dispose();
			status = undefined;
		}
	})
	client.onNotification(updateFileList, (params: UpdateFileList) => {
		fileList = params.fileList;
		if (fileExplorer) {
			fileExplorer.refresh(fileList);
		}
		else {
			fileExplorer = new FileExplorer(context, fileList);
		}
	})
}
