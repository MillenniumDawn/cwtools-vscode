import * as vscode from "vscode";

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
	children: Record<string, TreeNodeInternal>;
}

export function filesToTreeNodes(arr: FileListItem[]): TreeNode[] {
	const tree: Record<string, TreeNodeInternal> = {};

	function addnode(obj: FileListItem): void {
		const splitpath = (obj.scope + "/" + obj.logicalpath)
			.split("/")
			.filter((s) => s.length > 0);
		let ptr = tree;

		for (let i = 0; i < splitpath.length; i++) {
			const segment = splitpath[i];
			const isLastSegment = i === splitpath.length - 1;

			if (!ptr[segment]) {
				ptr[segment] = {
					fileName: segment,
					isDirectory: !isLastSegment,
					children: {},
				};

				if (isLastSegment) {
					ptr[segment].uri = obj.uri;
				}
			} else if (!isLastSegment) {
				// A node that first arrived as a leaf now has children; make
				// it a directory so they aren't hidden.
				ptr[segment].isDirectory = true;
			}

			ptr = ptr[segment].children;
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
		result.children = Object.values(node.children).map((c) =>
			convertToTreeNode(c, result),
		);
		return result;
	}

	arr.forEach(addnode);
	return Object.values(tree).map((n) => convertToTreeNode(n));
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
	constructor(files: FileListItem[]) {
		this.parseTree(files);
	}
	private _onDidChangeTreeData: vscode.EventEmitter<TreeNode | null> =
		new vscode.EventEmitter<TreeNode | null>();
	readonly onDidChangeTreeData: vscode.Event<TreeNode | null> =
		this._onDidChangeTreeData.event;

	private parseTree(files: FileListItem[]): void {
		this._tree.children = filesToTreeNodes(files);
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
				title: "Open File",
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
		const target = uri.toString();
		const stack = [...this._tree.children];
		while (stack.length > 0) {
			const node = stack.pop()!;
			if (!node.isDirectory && vscode.Uri.parse(node.uri).toString() === target) {
				return node;
			}
			stack.push(...node.children);
		}
		return undefined;
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
			vscode.commands.registerCommand(
				"cwtools-files.revealActiveFile",
				() => this.revealActiveFile(),
			),
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

	private openResource(resource: vscode.Uri): void {
		vscode.window.showTextDocument(resource);
	}

	dispose(): void {
		this.treeDataProvider.dispose();
	}

	refresh(files: FileListItem[]): void {
		this.treeDataProvider.refresh(files);
	}
}
