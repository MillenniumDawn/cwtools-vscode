import { suite, test } from "vitest";
import * as assert from "assert";
import {
	diagnosticsSignature,
	DiagnosticsSignatureCache,
} from "../../src/host/diagnosticsSignature";
import type { DiagnosticLike } from "../../src/host/diagnosticsSignature";

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
		["relatedInformation", [diag({ relatedInformation: [{}] })]],
		["count", [diag(), diag({ message: "second" })]],
		["empty list", []],
	])("a changed %s changes the signature", (_field, changed) => {
		assert.notStrictEqual(diagnosticsSignature([diag()]), diagnosticsSignature(changed));
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

	// A restart drops the diagnostic collection, so the same payload has to go
	// out again.
	test("re-publishes the same payload after clear", () => {
		const cache = new DiagnosticsSignatureCache();
		cache.shouldPublish("file:///a.txt", [diag()]);
		cache.clear();
		assert.strictEqual(cache.shouldPublish("file:///a.txt", [diag()]), true);
	});

	test("retains a full workspace pass, then evicts the oldest entry", () => {
		const cache = new DiagnosticsSignatureCache();
		const uri = (i: number) => `file:///f${i}.txt`;
		for (let i = 0; i < 2_000; i++) cache.shouldPublish(uri(i), [diag()]);
		for (let i = 0; i < 2_000; i++) {
			assert.strictEqual(cache.shouldPublish(uri(i), [diag()]), false);
		}

		cache.shouldPublish(uri(2_000), [diag()]);
		assert.strictEqual(cache.shouldPublish(uri(1_999), [diag()]), false);
		assert.strictEqual(cache.shouldPublish(uri(0), [diag()]), true);
	});
});
