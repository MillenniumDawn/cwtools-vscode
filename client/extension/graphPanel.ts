import * as vscode from "vscode";
import * as path from 'path';
import { writeFile } from 'fs/promises';
import * as crypto from 'crypto';
import type { GraphData } from "../common/graphTypes";
import { logError } from './logger';

export enum State {
    New,
    DataReady,
    ClientReady,
    Done
}
type GraphMessage =
    | { command: 'importJson'; json: string; settings: { wheelSensitivity: number } }
    | { command: 'go'; data: GraphData; settings: { wheelSensitivity: number } };
type WebviewMessage =
    | { command: 'goToFile'; uri: string; line: number; column: number }
    | { command: 'saveImage'; image: string }
    | { command: 'saveJson'; json: string }
    | { command: 'ready' }
    | { command: 'cytoscapeRenderedResult'; rendered: boolean };
export class GraphPanel {

    /**
    * Track the currently panel. Only allow a single panel to exist at a time.
    */
    public static currentPanel: GraphPanel | undefined;
    private static readonly viewType = 'cwtools-graph';
    private readonly _panel: vscode.WebviewPanel;
    private _state: State;
    private pendingMessage: GraphMessage | null = null;

    // Methods for testing
    public getState(): State {
        return this._state;
    }
    private pendingRequest : ((data: boolean) => void) | null = null

    // Method to check if cytoscape has rendered elements
    public async checkCytoscapeRendered() {
        // Settle any in-flight check before replacing it, so it isn't orphaned.
        if (this.pendingRequest !== null) {
            this.pendingRequest(false);
        }
        const promise = new Promise<boolean>((resolve) => {
            this.pendingRequest = resolve;
        });
        this._panel.webview.postMessage({ "command": "checkCytoscapeRendered" });
        return promise;
    }

    private _disposed = false;
    private _disposables: vscode.Disposable[] = [];
    private readonly _webviewRootPath: string;


    public static create(extensionPath: string) {
        const column = vscode.window.activeTextEditor ? vscode.window.activeTextEditor.viewColumn : undefined;

        if (GraphPanel.currentPanel) {
            GraphPanel.currentPanel._panel.reveal(column);
            return;
        }
        GraphPanel.currentPanel = new GraphPanel(extensionPath, column || vscode.ViewColumn.One);
    }

    private constructor(extensionPath: string, column: vscode.ViewColumn) {
        this._webviewRootPath = path.join(extensionPath, 'bin/client/webview');

        this._state = State.New;

        // Create and show a new webview panel
        this._panel = vscode.window.createWebviewPanel(GraphPanel.viewType, "Graph", column, {
            // Enable javascript in the webview
            enableScripts: true,
            retainContextWhenHidden: true,

            // And restric the webview to only loading content from our extension's `media` directory.
            localResourceRoots: [
                vscode.Uri.file(this._webviewRootPath)
            ]
        });

        // Set the webview's initial html content
        this._panel.webview.html = this._getHtmlForWebview();

        // Listen for when the panel is disposed
        // This happens when the user closes the panel or when the panel is closed programatically
        this._panel.onDidDispose(() => this.dispose(), null, this._disposables);

        // Handle messages from the webview
        this._disposables.push((this._panel.webview.onDidReceiveMessage(async (message: WebviewMessage) => {
            try {
                switch (message.command) {
                    case 'goToFile':
                        {
                            const uri = vscode.Uri.file(message.uri);
                            // GraphLocation is 1-based, vscode.Range is 0-based.
                            // Clamped so a defensive 0 can't go negative.
                            const line = Math.max(0, message.line - 1);
                            const column = Math.max(0, message.column - 1);
                            const range = new vscode.Range(line, column, line, column);
                            const texteditor = await vscode.window.showTextDocument(uri);
                            texteditor.revealRange(range, vscode.TextEditorRevealType.AtTop);
                            return;
                        }
                    case 'saveImage':
                        {
                            const image = message.image;
                            const dest = await vscode.window.showSaveDialog({ filters: { 'Image': ['png'] } });
                            if(dest){
                                await writeFile(dest.fsPath, image, "base64");
                            }
                            return;
                        }
                    case 'saveJson':
                        {
                            const json = message.json;
                            const dest = await vscode.window.showSaveDialog({ filters: { 'Json': ['json'] } });
                            if(dest){
                                await writeFile(dest.fsPath, json, "utf-8");
                            }
                            return;
                        }
                    case 'ready':
                        if (this._state === State.DataReady) {
                            this._state = State.Done;
                            this.flushPendingMessage();
                        } else {
                            this._state = State.ClientReady;
                        }
                        return;
                    case 'cytoscapeRenderedResult':
                        {
                            if(this.pendingRequest !== null){
                                const resolve = this.pendingRequest;
                                this.pendingRequest = null;
                                resolve(message.rendered); // Use 'rendered' property from webview response
                            }
                            return;
                        }
                }
            } catch (error) {
                logError('graph webview message handler failed', error);
            }
        }, null, this._disposables)));

        // Handle state change
        this._disposables.push((this._panel.onDidChangeViewState((e) => {
            vscode.commands.executeCommand('setContext', "cwtoolsWebview", e.webviewPanel.active);
        }, null, this._disposables)))

        // Set up commands
        this._disposables.push(vscode.commands.registerCommand('cwtools.saveGraphImage', () => {
            this._panel.webview.postMessage({ "command": "exportImage" })
        }))
        this._disposables.push(vscode.commands.registerCommand('cwtools.saveGraphJson', () => {
            this._panel.webview.postMessage({ "command": "exportJson" })
        }))

        vscode.commands.executeCommand('setContext', "cwtoolsWebview", true);

    }

    public initialiseGraph(data: string | GraphData, wheelSensitivity: number) {
        const settings = {
            wheelSensitivity: wheelSensitivity
        }
        const msg: GraphMessage = typeof(data) === 'string'
            ? { command: 'importJson', json: data, settings }
            : { command: 'go', data: data, settings };

        if (this._state === State.Done) {
            this._panel.webview.postMessage(msg);
            return;
        }

        this.pendingMessage = msg;
        if (this._state === State.ClientReady) {
            this._state = State.Done;
            this.flushPendingMessage();
        } else {
            this._state = State.DataReady;
        }
    }

    private flushPendingMessage() {
        if (this.pendingMessage) {
            this._panel.webview.postMessage(this.pendingMessage);
            this.pendingMessage = null;
        }
    }

    public dispose() {
        // onDidDispose calls back in here when the panel closes, so guard against
        // a double pass.
        if (this._disposed) {
            return;
        }
        this._disposed = true;
        vscode.commands.executeCommand('setContext', "cwtoolsWebview", false);

        // Clean up our resources
        this._panel.dispose();

        while (this._disposables.length) {
            const x = this._disposables.pop();
            if (x) {
                x.dispose();
            }
        }
        GraphPanel.currentPanel = undefined;
    }


    private _getHtmlForWebview() {
        const scriptUri = this._panel.webview.asWebviewUri(vscode.Uri.file(path.join(this._webviewRootPath, 'graph.js')));
        const styleUri = this._panel.webview.asWebviewUri(vscode.Uri.file(path.join(this._webviewRootPath, 'site.css')));

        const nonce = this.getNonce();
        // cspSource, not the pre-1.55 `vscode-resource:` scheme: asWebviewUri now
        // returns an https://…vscode-cdn.net origin, which that scheme does not
        // cover, so site.css was blocked and #cy lost its flex sizing.
        const cspSource = this._panel.webview.cspSource;
        return `
        <!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
   <meta http-equiv="Content-Security-Policy" content="default-src 'nonce-${nonce}'; img-src ${cspSource} https: data:; script-src 'nonce-${nonce}' 'strict-dynamic'; base-uri 'self'; object-src 'none'; style-src ${cspSource} 'unsafe-inline'">
           <link href="${styleUri.toString()}" rel="stylesheet" type="text/css" nonce="${nonce}" />
    </head>
<body>
    <div class="vbox viewport body-content">

        <div class="hbox cy-container">
    <div class="cy-row" id="cy"></div>
</div>

         <script src="${scriptUri.toString()}" nonce="${nonce}"></script>
</div>
</body>
</html>
`;


    }
    private getNonce() {
        // CSP nonce: use a CSPRNG, not Math.random(). hex keeps it 32 chars.
        return crypto.randomBytes(16).toString('hex');
    };
}
