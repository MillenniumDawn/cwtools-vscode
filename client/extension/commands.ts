import * as vscode from 'vscode';
import type { ExtensionContext } from 'vscode';
import { workspace, window, commands } from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import { ExecuteCommandRequest } from 'vscode-languageclient/node';
import { getGraphData } from '../common/graphTypes';
import type { EditorTracker } from './documentLanguage';

export function registerCommands(context: ExtensionContext, client: LanguageClient, tracker: EditorTracker): void {
	let currentGraphDepth = 3;
	const wheelSensitivity = (): number => workspace.getConfiguration('cwtools.graph').get('zoomSensitivity') ?? 1;
	const showGraph = async function() {
		const [gp, graphData] = await Promise.all([
			import('./graphPanel'),
			getGraphData(tracker.getLatestType(), currentGraphDepth),
		]);
		gp.GraphPanel.create(context.extensionPath);
		gp.GraphPanel.currentPanel!.initialiseGraph(graphData, wheelSensitivity());
	}
	context.subscriptions.push(commands.registerCommand('showGraph', async () => {
		await showGraph();
	}));
	context.subscriptions.push(commands.registerCommand('setGraphDepth', async () => {
		const res = await window.showInputBox(
			{
				placeHolder: "default: 3",
				prompt: "Set graph depth (how many connections to go back from this file)",
				value: currentGraphDepth.toString(),
				validateInput: (v : string) => Number.isInteger(Number(v)) ? undefined : "Please enter a number"
		 });
			if (Number.isInteger(Number(res)))
		{
			currentGraphDepth = Number(res)
			await showGraph()
		}
	}));
	context.subscriptions.push(commands.registerCommand('graphFromJson', async () => {
		const uri = await window.showOpenDialog({filters: {'Json': ['json']}})
		if(!uri){
			return;
		}
		const bytes = await vscode.workspace.fs.readFile(uri[0]);
		const data = new TextDecoder('utf-8').decode(bytes);
		const gp = await import('./graphPanel');
		gp.GraphPanel.create(context.extensionPath);
		gp.GraphPanel.currentPanel!.initialiseGraph(data, wheelSensitivity());
	}));
	// cacheVanilla / clearAllCaches are NOT registered here: the language
	// client registers them from the server's executeCommandProvider, and
	// the executeCommand middleware surfaces their results.

	// Fetch the server's accumulated profiling report and save it to a file.
	// The server only fills the buffer when launched with CWTOOLS_PROFILE=1
	// (the cwtools.profiling setting), so prompt to enable it if empty.
	context.subscriptions.push(commands.registerCommand('cwtools.exportProfilingLog', async () => {
		let log: unknown;
		try {
			log = await client.sendRequest(ExecuteCommandRequest.type, { command: 'exportProfilingLog', arguments: [] });
		} catch (err) {
			const msg = err instanceof Error ? err.message : String(err);
			window.showErrorMessage(`CWTools: could not fetch profiling log: ${msg}`);
			return;
		}
		if (typeof log !== 'string' || log.length === 0) {
			window.showWarningMessage("CWTools: profiling log is empty. Turn on 'cwtools.profiling', reload the window, reproduce the slowdown, then export.");
			return;
		}
		const uri = await window.showSaveDialog({ filters: { 'Log': ['log', 'txt'] }, saveLabel: 'Export CWTools profiling log' });
		if (!uri) { return; }
		await workspace.fs.writeFile(uri, Buffer.from(log, 'utf8'));
		window.showInformationMessage(`CWTools: profiling log written to ${uri.fsPath}`);
	}));
	context.subscriptions.push(vscode.commands.registerCommand("cwtools.reloadExtension", () =>
		commands.executeCommand('workbench.action.reloadWindow')
	));
}
