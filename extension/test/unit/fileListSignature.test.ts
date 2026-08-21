import { suite, test } from "vitest";
import * as assert from "assert";
import { fileListSignature } from "../../src/host/fileListSignature";

interface FileListItem {
	scope: string;
	uri: string;
	logicalpath: string;
}

const base: FileListItem[] = [
	{ scope: "mod", uri: "file:///a.txt", logicalpath: "common/a.txt" },
	{ scope: "mod", uri: "file:///b.txt", logicalpath: "common/b.txt" },
	{ scope: "vanilla", uri: "file:///c.txt", logicalpath: "events/c.txt" },
];

suite("fileListSignature — fileListSignature", () => {
	test("identical lists (same values, fresh copy) produce the same signature", () => {
		const copy = base.map(f => ({ ...f }));
		assert.strictEqual(fileListSignature(base), fileListSignature(copy));
	});

	test("an added file changes the signature", () => {
		const added = [
			...base,
			{ scope: "mod", uri: "file:///d.txt", logicalpath: "common/d.txt" },
		];
		assert.notStrictEqual(fileListSignature(base), fileListSignature(added));
	});

	test("a removed file changes the signature", () => {
		const removed = base.slice(0, 2);
		assert.notStrictEqual(fileListSignature(base), fileListSignature(removed));
	});

	test("reordering the same files changes the signature (order-sensitive)", () => {
		const reordered = [base[1], base[0], base[2]];
		assert.notStrictEqual(fileListSignature(base), fileListSignature(reordered));
	});

	test("renaming a file (uri/logicalpath) changes the signature", () => {
		const renamed = base.map((f, i) =>
			i === 0 ? { ...f, uri: "file:///a2.txt", logicalpath: "common/a2.txt" } : f,
		);
		assert.notStrictEqual(fileListSignature(base), fileListSignature(renamed));
	});

	test("an empty list has a stable signature", () => {
		assert.strictEqual(fileListSignature([]), fileListSignature([]));
	});
});
