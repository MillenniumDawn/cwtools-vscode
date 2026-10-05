import * as vscode from "vscode";
import { logError } from "./logger";
import { confirmOpen } from "./trustedPaths";

//#region Utilities

export interface TreeNode {
	isDirectory: boolean;
	children: TreeNode[];
	fileName: string;
	uri: string;
	parent?: TreeNode;
}
export interface FileListItem {
	scope: string;
	uri: string;
	logicalpath: string;
}

interface TreeNodeInternal {
	fileName?: string;
	isDirectory?: boolean;
	uri?: string;
	children: Map<string, TreeNodeInternal>;
}

// A `Map`, not a plain object: a segment named `constructor`, `toString` or
// `__proto__` would otherwise resolve through `Object.prototype`, and
// integer-like names such as `10` would sort ahead of `9` in `Object.values`.
export function filesToTreeNodes(arr: FileListItem[]): TreeNode[] {
	const tree = new Map<string, TreeNodeInternal>();

	function addnode(obj: FileListItem): void {
		const splitpath = (obj.scope + "/" + obj.logicalpath)
			.split("/")
			.filter((s) => s.length > 0);
		let ptr = tree;

		for (let i = 0; i < splitpath.length; i++) {
			const segment = splitpath[i];
			const isLastSegment = i === splitpath.length - 1;

			let node = ptr.get(segment);
			if (!node) {
				node = {
					fileName: segment,
					isDirectory: !isLastSegment,
					children: new Map(),
				};
				if (isLastSegment) {
					node.uri = obj.uri;
				}
				ptr.set(segment, node);
			} else if (!isLastSegment) {
				// A node that first arrived as a leaf now has children; make
				// it a directory so they aren't hidden.
				node.isDirectory = true;
			}

			ptr = node.children;
		}
	}

	function convertToTreeNode(
		node: TreeNodeInternal,
		parent?: TreeNode,
	): TreeNode {
		const result: TreeNode = {
			isDirectory: node.isDirectory ?? true,
			fileName: node.fileName ?? "",
			uri: node.uri ?? "",
			children: [],
			parent,
		};
		result.children = Array.from(node.children.values(), (c) =>
			convertToTreeNode(c, result),
		);
		return result;
	}

	arr.forEach(addnode);
	return Array.from(tree.values(), (n) => convertToTreeNode(n));
}

/** The spelling `findNodeByUri` matches on, for both the index and the query. */
function uriKey(uri: string | vscode.Uri): string {
	return (typeof uri === "string" ? vscode.Uri.parse(uri) : uri).toString();
}

export class FilesProvider
	implements vscode.TreeDataProvider<TreeNode>, vscode.Disposable
{
	private readonly _tree: TreeNode = {
		fileName: "root",
		isDirectory: true,
		children: [],
		uri: "",
	};
	// Leaf nodes by normalised URI, built on the first reveal after each
	// refresh so a reveal is one lookup instead of a `Uri.parse` per file node,
	// while a refresh that is never followed by a reveal parses nothing (#876).
	private _byUri: Map<string, TreeNode> | undefined;
	constructor(files: FileListItem[]) {
		this.parseTree(files);
	}
	private _onDidChangeTreeData: vscode.EventEmitter<TreeNode | null> =
		new vscode.EventEmitter<TreeNode | null>();
	readonly onDidChangeTreeData: vscode.Event<TreeNode | null> =
		this._onDidChangeTreeData.event;

	private parseTree(files: FileListItem[]): void {
		this._tree.children = filesToTreeNodes(files);
		this._byUri = undefined;
	}

	private uriIndex(): Map<string, TreeNode> {
		if (this._byUri) {
			return this._byUri;
		}
		const byUri = new Map<string, TreeNode>();
		const stack = [...this._tree.children];
		while (stack.length > 0) {
			const node = stack.pop()!;
			if (node.isDirectory) {
				for (const child of node.children) {
					stack.push(child);
				}
			} else {
				const key = uriKey(node.uri);
				if (!byUri.has(key)) {
					byUri.set(key, node);
				}
			}
		}
		this._byUri = byUri;
		return byUri;
	}

	getTreeItem(element: TreeNode): vscode.TreeItem {
		const treeItem = new vscode.TreeItem(
			element.fileName,
			element.isDirectory
				? vscode.TreeItemCollapsibleState.Collapsed
				: vscode.TreeItemCollapsibleState.None,
		);
		if (!element.isDirectory) {
			treeItem.command = {
				command: "cwtools-files.openFile",
				title: vscode.l10n.t("Open File"),
				arguments: [vscode.Uri.parse(element.uri)],
			};
			treeItem.contextValue = "file";
			treeItem.resourceUri = vscode.Uri.parse(element.uri);
		}
		return treeItem;
	}
	getChildren(element?: TreeNode): TreeNode[] {
		return element ? element.children : this._tree.children;
	}
	getParent(element: TreeNode): TreeNode | undefined {
		return element.parent;
	}
	findNodeByUri(uri: vscode.Uri): TreeNode | undefined {
		return this.uriIndex().get(uriKey(uri));
	}
	refresh(files: FileListItem[]) {
		this.parseTree(files);
		this._onDidChangeTreeData.fire(null);
	}

	dispose(): void {
		this._onDidChangeTreeData.dispose();
	}
}

export class FileExplorer implements vscode.Disposable {
	private fileExplorer: vscode.TreeView<TreeNode>;
	private treeDataProvider: FilesProvider;

	constructor(context: vscode.ExtensionContext, files: FileListItem[]) {
		this.treeDataProvider = new FilesProvider(files);
		this.fileExplorer = vscode.window.createTreeView("cwtools-files", {
			treeDataProvider: this.treeDataProvider,
			showCollapseAll: true,
		});
		context.subscriptions.push(this.fileExplorer);
		context.subscriptions.push(
			vscode.commands.registerCommand(
				"cwtools-files.openFile",
				(resource: vscode.Uri) => this.openResource(resource),
			),
		);
		context.subscriptions.push(
			vscode.commands.registerCommand("cwtools-files.revealActiveFile", () =>
				this.revealActiveFile(),
			),
		);
		// The view/title button is gated on this key so it can't be clicked before
		// the command above is registered (the view itself appears at activation,
		// before the server sends its first file list).
		void vscode.commands.executeCommand(
			"setContext",
			"cwtoolsFilesLoaded",
			true,
		);
	}

	private revealActiveFile(): void {
		const editor = vscode.window.activeTextEditor;
		if (!editor) {
			return;
		}
		const node = this.treeDataProvider.findNodeByUri(editor.document.uri);
		if (node) {
			this.fileExplorer.reveal(node, { select: true, focus: true });
		}
	}

	private async openResource(resource: vscode.Uri): Promise<void> {
		try {
			if (!(await confirmOpen(resource))) {
				return;
			}
			await vscode.window.showTextDocument(resource);
		} catch (err) {
			logError(`Failed to open ${resource.fsPath}`, err);
			void vscode.window.showErrorMessage(
				vscode.l10n.t("CWTools: could not open {0}", resource.fsPath),
			);
		}
	}

	dispose(): void {
		this.treeDataProvider.dispose();
	}

	refresh(files: FileListItem[]): void {
		this.treeDataProvider.refresh(files);
	}
}
