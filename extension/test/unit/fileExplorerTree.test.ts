import { suite, test, vi } from "vitest";
import * as assert from "assert";
import type { Uri } from "vscode";
import type * as VscodeStub from "./_stubs/vscode";

const { uriParse } = vi.hoisted(() => ({
	uriParse: vi.fn((value: string) => ({
		fsPath: value.replace(/^file:\/\//, ""),
		toString: () => value,
	})),
}));

vi.mock("vscode", async (importOriginal) => {
	const original = await importOriginal<typeof VscodeStub>();
	return {
		...original,
		Uri: { ...original.Uri, parse: uriParse },
		l10n: { t: (message: string) => message },
	};
});

import { FilesProvider, filesToTreeNodes } from "../../src/host/fileExplorer";
import type { FileListItem } from "../../src/host/fileExplorer";

function item(logicalpath: string, scope = "mod"): FileListItem {
	return { scope, logicalpath, uri: `file:///ws/${scope}/${logicalpath}` };
}

suite("filesToTreeNodes", () => {
	test("segments that collide with Object.prototype become ordinary nodes", () => {
		const tree = filesToTreeNodes([
			item("constructor/a.txt"),
			item("__proto__/b.txt"),
			item("toString"),
			item("hasOwnProperty/valueOf.txt"),
		]);
		const mod = tree[0];
		assert.deepStrictEqual(
			mod.children.map((c) => [c.fileName, c.isDirectory]),
			[
				["constructor", true],
				["__proto__", true],
				["toString", false],
				["hasOwnProperty", true],
			],
		);
		assert.strictEqual(mod.children[0].children[0].fileName, "a.txt");
		assert.strictEqual(mod.children[1].children[0].fileName, "b.txt");
		assert.strictEqual(mod.children[2].uri, "file:///ws/mod/toString");
		assert.strictEqual(mod.children[3].children[0].fileName, "valueOf.txt");
	});

	test("keeps the server's order for integer-like names", () => {
		const tree = filesToTreeNodes([
			item("events/b.txt"),
			item("events/10.txt"),
			item("events/9.txt"),
			item("events/a.txt"),
		]);
		assert.deepStrictEqual(
			tree[0].children[0].children.map((c) => c.fileName),
			["b.txt", "10.txt", "9.txt", "a.txt"],
		);
	});
});

suite("FilesProvider.findNodeByUri", () => {
	test("parses each node URI at most once across reveals, and not on refresh", () => {
		const files = Array.from({ length: 200 }, (_, i) =>
			item(`common/dir_${i % 7}/file_${i}.txt`),
		);
		uriParse.mockClear();
		const provider = new FilesProvider(files);
		assert.strictEqual(
			uriParse.mock.calls.length,
			0,
			"building the tree must not parse node URIs",
		);

		const target = { toString: () => files[123].uri } as unknown as Uri;
		const node = provider.findNodeByUri(target);
		assert.ok(node, "node for an indexed file");
		assert.strictEqual(node.uri, files[123].uri);
		assert.strictEqual(node.fileName, "file_123.txt");
		const parsesAfterFirstReveal = uriParse.mock.calls.length;
		assert.ok(
			parsesAfterFirstReveal <= files.length,
			`first reveal may index every node once, got ${parsesAfterFirstReveal}`,
		);

		const other = { toString: () => files[7].uri } as unknown as Uri;
		assert.strictEqual(provider.findNodeByUri(other)?.fileName, "file_7.txt");
		const missing = { toString: () => "file:///ws/mod/nowhere.txt" } as Uri;
		assert.strictEqual(provider.findNodeByUri(missing), undefined);
		assert.strictEqual(
			uriParse.mock.calls.length,
			parsesAfterFirstReveal,
			"later reveals must not parse any node URI",
		);
	});

	test("refresh rebuilds the index", () => {
		const provider = new FilesProvider([item("a.txt")]);
		provider.refresh([item("b.txt")]);
		const gone = { toString: () => "file:///ws/mod/a.txt" } as Uri;
		const now = { toString: () => "file:///ws/mod/b.txt" } as Uri;
		assert.strictEqual(provider.findNodeByUri(gone), undefined);
		assert.strictEqual(provider.findNodeByUri(now)?.fileName, "b.txt");
	});
});
