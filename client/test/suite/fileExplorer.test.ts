import * as assert from "assert";
import * as vscode from "vscode";
import type { FileListItem, TreeNode } from "../../extension/fileExplorer";
import { filesToTreeNodes, FilesProvider } from "../../extension/fileExplorer";

suite("FileExplorer — filesToTreeNodes", () => {
	test("returns an empty array for an empty input", () => {
		assert.deepStrictEqual(filesToTreeNodes([]), []);
	});

	test("builds a single-file tree at the root scope", () => {
		const files: FileListItem[] = [
			{ scope: "events", uri: "file:///events/a.txt", logicalpath: "irm.txt" },
		];
		const tree = filesToTreeNodes(files);
		assert.strictEqual(tree.length, 1);
		assert.strictEqual(tree[0].fileName, "events");
		assert.strictEqual(tree[0].isDirectory, true);
		assert.strictEqual(tree[0].children.length, 1);
		assert.strictEqual(tree[0].children[0].fileName, "irm.txt");
		assert.strictEqual(tree[0].children[0].isDirectory, false);
		assert.strictEqual(tree[0].children[0].uri, "file:///events/a.txt");
	});

	test("nests files at multiple depths and merges siblings", () => {
		const files: FileListItem[] = [
			{
				scope: "common",
				uri: "file:///common/buildings/castle.txt",
				logicalpath: "buildings/castle.txt",
			},
			{
				scope: "common",
				uri: "file:///common/buildings/temple.txt",
				logicalpath: "buildings/temple.txt",
			},
			{
				scope: "common",
				uri: "file:///common/technology.txt",
				logicalpath: "technology.txt",
			},
		];
		const tree = filesToTreeNodes(files);
		assert.strictEqual(tree.length, 1);
		const common = tree[0];
		assert.strictEqual(common.fileName, "common");
		assert.strictEqual(common.isDirectory, true);
		assert.strictEqual(common.children.length, 2);

		const buildings = common.children.find((c) => c.fileName === "buildings")!;
		assert.ok(buildings, "expected a buildings directory");
		assert.strictEqual(buildings.isDirectory, true);
		assert.strictEqual(buildings.children.length, 2);
		assert.deepStrictEqual(buildings.children.map((c) => c.fileName).sort(), [
			"castle.txt",
			"temple.txt",
		]);

		const tech = common.children.find((c) => c.fileName === "technology.txt")!;
		assert.ok(tech, "expected technology.txt at common/");
		assert.strictEqual(tech.isDirectory, false);
		assert.strictEqual(tech.uri, "file:///common/technology.txt");
	});

	test("strips leading and trailing slashes from paths", () => {
		const files: FileListItem[] = [
			{ scope: "common", uri: "file:///x.txt", logicalpath: "//inner.txt/" },
		];
		const tree = filesToTreeNodes(files);
		const common = tree[0];
		assert.strictEqual(common.children.length, 1);
		assert.strictEqual(common.children[0].fileName, "inner.txt");
		assert.strictEqual(common.children[0].isDirectory, false);
	});

	test("treats an intermediate segment without further children as a directory", () => {
		// Path with one segment should still be a leaf file; two segments -> intermediate dir.
		const leaf: FileListItem[] = [
			{ scope: "s", uri: "u1", logicalpath: "a.txt" },
		];
		const nested: FileListItem[] = [
			{ scope: "s", uri: "u2", logicalpath: "dir/a.txt" },
		];
		assert.strictEqual(
			filesToTreeNodes(leaf)[0].children[0].isDirectory,
			false,
		);
		assert.strictEqual(
			filesToTreeNodes(nested)[0].children[0].isDirectory,
			true,
		);
	});

	test("promotes a node to a directory when a deeper path arrives after the leaf", () => {
		// 'shared' first appears as a leaf file, then as a parent. It must end up
		// a directory so its children aren't hidden.
		const files: FileListItem[] = [
			{ scope: "common", uri: "file:///common/shared", logicalpath: "shared" },
			{
				scope: "common",
				uri: "file:///common/shared/child.txt",
				logicalpath: "shared/child.txt",
			},
		];
		const tree = filesToTreeNodes(files);
		const shared = tree[0].children.find((c) => c.fileName === "shared")!;
		assert.ok(shared, "expected a shared node");
		assert.strictEqual(shared.isDirectory, true);
		assert.strictEqual(shared.children.length, 1);
		assert.strictEqual(shared.children[0].fileName, "child.txt");
	});
});

suite("FileExplorer — FilesProvider", () => {
	const sampleFiles = (): FileListItem[] => [
		{ scope: "events", uri: "file:///events/irm.txt", logicalpath: "irm.txt" },
		{
			scope: "events",
			uri: "file:///events/irm_faction.txt",
			logicalpath: "irm_faction.txt",
		},
	];

	test("exposes the parsed tree at the root", () => {
		const provider = new FilesProvider(sampleFiles());
		const children = provider.getChildren();
		assert.strictEqual(children.length, 1);
		assert.strictEqual(children[0].fileName, "events");
		assert.strictEqual(children[0].isDirectory, true);
		assert.strictEqual(children[0].children.length, 2);
	});

	test("builds a TreeItem that opens the file on click for leaf nodes", () => {
		const provider = new FilesProvider(sampleFiles());
		const root = provider.getChildren()[0];
		const leaf = root.children[0];
		const item = provider.getTreeItem(leaf);
		assert.strictEqual(item.label, leaf.fileName);
		assert.strictEqual(item.collapsibleState, 0 /* None */);
		assert.ok(item.command, "leaf items must register a command");
		assert.strictEqual(item.command.command, "cwtools-files.openFile");
		// The command stores a parsed vscode.Uri, not the raw string.
		const [arg] = item.command.arguments! as [vscode.Uri];
		assert.ok(
			arg && typeof arg === "object" && "scheme" in arg,
			"expected a Uri argument",
		);
		assert.strictEqual(arg.scheme, "file");
		assert.strictEqual(arg.path, leaf.uri.replace(/^file:\/\//, ""));
		assert.strictEqual(item.contextValue, "file");
	});

	test("builds a collapsible TreeItem for directories with no open command", () => {
		const provider = new FilesProvider([
			{ scope: "common", uri: "u", logicalpath: "buildings/x.txt" },
		]);
		const common = provider.getChildren()[0];
		const buildings = common.children[0];
		const item = provider.getTreeItem(buildings);
		assert.strictEqual(item.collapsibleState, 1 /* Collapsed */);
		assert.strictEqual(item.command, undefined);
	});

	test("refresh replaces the tree and fires a change event", () => {
		const provider = new FilesProvider(sampleFiles());
		const seen: (TreeNode | null)[] = [];
		provider.onDidChangeTreeData((node) => seen.push(node));
		provider.refresh([{ scope: "events", uri: "u", logicalpath: "irm.txt" }]);
		const children = provider.getChildren();
		assert.strictEqual(children.length, 1);
		assert.strictEqual(children[0].children.length, 1);
		assert.strictEqual(seen.length, 1);
		assert.strictEqual(seen[0], null);
	});

	test("getParent returns the enclosing directory, undefined at the root", () => {
		const provider = new FilesProvider(sampleFiles());
		const root = provider.getChildren()[0];
		assert.strictEqual(provider.getParent(root), undefined);
		const leaf = root.children[0];
		assert.strictEqual(provider.getParent(leaf), root);
	});

	test("findNodeByUri locates a leaf by its resource uri", () => {
		const provider = new FilesProvider(sampleFiles());
		const found = provider.findNodeByUri(
			vscode.Uri.parse("file:///events/irm_faction.txt"),
		);
		assert.ok(found, "expected a matching leaf");
		assert.strictEqual(found.fileName, "irm_faction.txt");
		assert.strictEqual(found.isDirectory, false);
	});

	test("findNodeByUri returns undefined for an unknown uri", () => {
		const provider = new FilesProvider(sampleFiles());
		assert.strictEqual(
			provider.findNodeByUri(vscode.Uri.parse("file:///events/missing.txt")),
			undefined,
		);
	});
});
