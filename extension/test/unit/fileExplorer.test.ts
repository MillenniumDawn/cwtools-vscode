import { suite, test } from "vitest";
import * as assert from "assert";
import { Uri } from "vscode";
import { FilesProvider, filesToTreeNodes } from "../../src/host/fileExplorer";
import type { FileListItem } from "../../src/host/fileExplorer";

function item(logicalpath: string, scope = "mod"): FileListItem {
	return { scope, logicalpath, uri: `file:///ws/${scope}/${logicalpath}` };
}

suite("filesToTreeNodes", () => {
	test("returns an empty array for an empty input", () => {
		assert.deepStrictEqual(filesToTreeNodes([]), []);
	});

	test("nests files at multiple depths and merges siblings", () => {
		const [mod, ...rest] = filesToTreeNodes([
			item("buildings/castle.txt"),
			item("buildings/temple.txt"),
			item("technology.txt"),
		]);
		assert.deepStrictEqual(rest, []);
		assert.deepStrictEqual(
			mod.children.map((c) => [c.fileName, c.isDirectory, c.uri]),
			[
				["buildings", true, ""],
				["technology.txt", false, "file:///ws/mod/technology.txt"],
			],
		);
		assert.deepStrictEqual(
			mod.children[0].children.map((c) => c.fileName),
			["castle.txt", "temple.txt"],
		);
	});

	test("strips leading and trailing slashes from paths", () => {
		const [mod] = filesToTreeNodes([item("//inner.txt/")]);
		assert.deepStrictEqual(
			mod.children.map((c) => [c.fileName, c.isDirectory]),
			[["inner.txt", false]],
		);
	});

	test("promotes a leaf to a directory when a deeper path arrives after it", () => {
		const [mod] = filesToTreeNodes([item("shared"), item("shared/child.txt")]);
		const shared = mod.children[0];
		assert.strictEqual(shared.isDirectory, true);
		assert.deepStrictEqual(
			shared.children.map((c) => c.fileName),
			["child.txt"],
		);
	});

	test("segments that collide with Object.prototype become ordinary nodes", () => {
		const [mod] = filesToTreeNodes([
			item("constructor/a.txt"),
			item("__proto__/b.txt"),
			item("toString"),
			item("hasOwnProperty/valueOf.txt"),
		]);
		assert.deepStrictEqual(
			mod.children.map((c) => [c.fileName, c.isDirectory]),
			[
				["constructor", true],
				["__proto__", true],
				["toString", false],
				["hasOwnProperty", true],
			],
		);
		assert.strictEqual(mod.children[1].children[0].fileName, "b.txt");
		assert.strictEqual(mod.children[2].uri, "file:///ws/mod/toString");
	});

	test("keeps the server's order for integer-like names", () => {
		const [mod] = filesToTreeNodes([
			item("events/b.txt"),
			item("events/10.txt"),
			item("events/9.txt"),
			item("events/a.txt"),
		]);
		assert.deepStrictEqual(
			mod.children[0].children.map((c) => c.fileName),
			["b.txt", "10.txt", "9.txt", "a.txt"],
		);
	});
});

suite("FilesProvider", () => {
	const provider = () =>
		new FilesProvider([item("events/irm.txt"), item("events/irm_faction.txt")]);

	test("a leaf opens its file on click and a directory only collapses", () => {
		const files = provider();
		const events = files.getChildren()[0].children[0];
		const leaf = events.children[0];

		const leafItem = files.getTreeItem(leaf);
		assert.strictEqual(leafItem.label, "irm.txt");
		assert.strictEqual(leafItem.collapsibleState, 0);
		assert.strictEqual(leafItem.contextValue, "file");
		assert.strictEqual(leafItem.command?.command, "cwtools-files.openFile");
		assert.strictEqual(String(leafItem.command.arguments?.[0]), leaf.uri);

		const directoryItem = files.getTreeItem(events);
		assert.strictEqual(directoryItem.collapsibleState, 1);
		assert.strictEqual(directoryItem.command, undefined);
	});

	test("getParent returns the enclosing directory, undefined at the root", () => {
		const files = provider();
		const mod = files.getChildren()[0];
		assert.strictEqual(files.getParent(mod), undefined);
		assert.strictEqual(files.getParent(mod.children[0]), mod);
	});

	test("findNodeByUri locates a leaf and follows a refresh", () => {
		const files = provider();
		const faction = Uri.parse("file:///ws/mod/events/irm_faction.txt");
		assert.strictEqual(files.findNodeByUri(faction)?.fileName, "irm_faction.txt");
		assert.strictEqual(
			files.findNodeByUri(Uri.parse("file:///ws/mod/events/missing.txt")),
			undefined,
		);

		files.refresh([item("events/irm.txt")]);
		assert.strictEqual(files.findNodeByUri(faction), undefined);
		assert.strictEqual(files.getChildren()[0].children[0].children.length, 1);
	});
});
