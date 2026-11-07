import { suite, test } from "vitest";
import * as assert from "assert";
import {
	diagnosticsSignature,
	DiagnosticsSignatureCache,
} from "../../src/host/diagnosticsSignature";
import type { DiagnosticLike } from "../../src/host/diagnosticsSignature";

type RelatedInformation = NonNullable<DiagnosticLike["relatedInformation"]>[number];

function diag(overrides: Partial<DiagnosticLike> = {}): DiagnosticLike {
	return {
		range: { start: { line: 1, character: 2 }, end: { line: 1, character: 8 } },
		severity: 0,
		code: "CW100",
		message: "undefined scope",
		source: "cwtools",
		...overrides,
	};
}

function related(): RelatedInformation {
	return {
		location: {
			uri: { toString: () => "file:///declaration.txt" },
			range: { start: { line: 5, character: 3 }, end: { line: 5, character: 9 } },
		},
		message: "originally declared here",
	};
}

suite("diagnosticsSignature", () => {
	test("identical diagnostics produce the same signature", () => {
		assert.strictEqual(diagnosticsSignature([diag()]), diagnosticsSignature([diag()]));
		assert.strictEqual(diagnosticsSignature([]), diagnosticsSignature([]));
	});

	test.each<[string, DiagnosticLike[]]>([
		["message", [diag({ message: "different" })]],
		["range", [diag({ range: { start: { line: 3, character: 0 }, end: { line: 3, character: 4 } } })]],
		["severity", [diag({ severity: 1 })]],
		["code", [diag({ code: "CW200" })]],
		["source", [diag({ source: "other" })]],
		["relatedInformation", [diag({ relatedInformation: [related()] })]],
		["count", [diag(), diag({ message: "second" })]],
		["empty list", []],
	])("a changed %s changes the signature", (_field, changed) => {
		assert.notStrictEqual(diagnosticsSignature([diag()]), diagnosticsSignature(changed));
	});

	test("missing and empty relatedInformation stay equal", () => {
		assert.strictEqual(
			diagnosticsSignature([diag()]),
			diagnosticsSignature([diag({ relatedInformation: [] })]),
		);
	});

	test("a message containing separator bytes cannot collide with a real second diagnostic", () => {
		assert.notStrictEqual(
			diagnosticsSignature([diag({ message: "a\x1eb\x1fc" })]),
			diagnosticsSignature([diag(), diag()]),
		);
	});
});

suite("DiagnosticsSignatureCache", () => {
	test("publishes a URI's first and changed payloads, skips an identical repeat", () => {
		const cache = new DiagnosticsSignatureCache();
		assert.strictEqual(cache.shouldPublish("file:///a.txt", [diag()]), true);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", [diag()]), false);
		assert.strictEqual(cache.shouldPublish("file:///b.txt", [diag()]), true);
		assert.strictEqual(
			cache.shouldPublish("file:///a.txt", [diag({ message: "changed" })]),
			true,
		);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", []), true);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", []), false);
	});

	test("passes an empty publish for a never-seen URI", () => {
		const cache = new DiagnosticsSignatureCache();
		assert.strictEqual(cache.shouldPublish("file:///new.txt", []), true);
	});

	test.each<[string, (info: RelatedInformation) => void]>([
		["message", (info) => { info.message = "declared elsewhere"; }],
		["URI", (info) => { info.location.uri = { toString: () => "file:///other.txt" }; }],
		["start line", (info) => { info.location.range.start.line += 1; }],
		["start character", (info) => { info.location.range.start.character += 1; }],
		["end line", (info) => { info.location.range.end.line += 1; }],
		["end character", (info) => { info.location.range.end.character += 1; }],
	])("publishes when only the related %s changes", (_field, change) => {
		const cache = new DiagnosticsSignatureCache();
		const payload = (info: RelatedInformation) => [diag({ relatedInformation: [info] })];
		assert.strictEqual(cache.shouldPublish("file:///a.txt", payload(related())), true);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", payload(related())), false);
		const changed = related();
		change(changed);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", payload(changed)), true);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", payload(changed)), false);
	});

	test("includes every related item and detects removals", () => {
		const cache = new DiagnosticsSignatureCache();
		const payload = (items: RelatedInformation[]) => [diag({ relatedInformation: items })];
		assert.strictEqual(cache.shouldPublish("file:///a.txt", payload([related(), related()])), true);
		const second = related();
		second.message = "a different declaration";
		assert.strictEqual(cache.shouldPublish("file:///a.txt", payload([related(), second])), true);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", payload([related()])), true);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", payload([])), true);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", [diag()]), false);
	});

	// A restart drops the diagnostic collection, so the same payload has to go
	// out again.
	test("re-publishes the same payload after clear", () => {
		const cache = new DiagnosticsSignatureCache();
		const payload = () => [diag({ relatedInformation: [related()] })];
		cache.shouldPublish("file:///a.txt", payload());
		cache.clear();
		assert.strictEqual(cache.shouldPublish("file:///a.txt", payload()), true);
	});

	// #1070: scans publish every closed file in order, so a pass past the old
	// 2,000-file budget must still dedupe on the next pass.
	test("retains a full workspace pass larger than the old budget", () => {
		const cache = new DiagnosticsSignatureCache();
		const uri = (i: number) => `file:///f${i}.txt`;
		for (let i = 0; i < 5_000; i++) cache.shouldPublish(uri(i), [diag()]);
		for (let i = 0; i < 5_000; i++) {
			assert.strictEqual(cache.shouldPublish(uri(i), [diag()]), false);
		}
	});
});
