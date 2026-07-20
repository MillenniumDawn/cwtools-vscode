import { suite, test } from "vitest";
import * as assert from "assert";
import {
	diagnosticsSignature,
	DiagnosticsSignatureCache,
} from "../../extension/diagnosticsSignature";

interface Diag {
	range: { start: { line: number; character: number }; end: { line: number; character: number } };
	severity?: number;
	code?: string | number | { value: string | number };
	message: string;
	source?: string;
	relatedInformation?: readonly unknown[];
}

function diag(overrides: Partial<Diag> = {}): Diag {
	return {
		range: { start: { line: 1, character: 2 }, end: { line: 1, character: 8 } },
		severity: 0,
		code: "CW100",
		message: "undefined scope",
		source: "cwtools",
		...overrides,
	};
}

suite("diagnosticsSignature — diagnosticsSignature", () => {
	test("identical diagnostics produce the same signature", () => {
		assert.strictEqual(diagnosticsSignature([diag()]), diagnosticsSignature([diag()]));
	});

	test("a changed message changes the signature", () => {
		assert.notStrictEqual(
			diagnosticsSignature([diag()]),
			diagnosticsSignature([diag({ message: "different" })]),
		);
	});

	test("a changed range changes the signature", () => {
		assert.notStrictEqual(
			diagnosticsSignature([diag()]),
			diagnosticsSignature([diag({ range: { start: { line: 3, character: 0 }, end: { line: 3, character: 4 } } })]),
		);
	});

	test("a changed severity changes the signature", () => {
		assert.notStrictEqual(
			diagnosticsSignature([diag()]),
			diagnosticsSignature([diag({ severity: 1 })]),
		);
	});

	test("a changed code changes the signature", () => {
		assert.notStrictEqual(
			diagnosticsSignature([diag()]),
			diagnosticsSignature([diag({ code: "CW200" })]),
		);
	});

	test("a changed source changes the signature", () => {
		assert.notStrictEqual(
			diagnosticsSignature([diag()]),
			diagnosticsSignature([diag({ source: "other" })]),
		);
	});

	test("gaining relatedInformation changes the signature", () => {
		assert.notStrictEqual(
			diagnosticsSignature([diag()]),
			diagnosticsSignature([diag({ relatedInformation: [{}] })]),
		);
	});

	test("a changed count changes the signature", () => {
		assert.notStrictEqual(
			diagnosticsSignature([diag()]),
			diagnosticsSignature([diag(), diag({ message: "second" })]),
		);
	});

	test("a message containing separator bytes cannot collide with a real second diagnostic", () => {
		assert.notStrictEqual(
			diagnosticsSignature([diag({ message: "a\x1eb\x1fc" })]),
			diagnosticsSignature([diag(), diag()]),
		);
	});

	test("an empty list has a stable signature that differs from a non-empty one", () => {
		assert.strictEqual(diagnosticsSignature([]), diagnosticsSignature([]));
		assert.notStrictEqual(diagnosticsSignature([]), diagnosticsSignature([diag()]));
	});
});

suite("diagnosticsSignature — DiagnosticsSignatureCache", () => {
	test("publishes the first time a URI is seen", () => {
		const cache = new DiagnosticsSignatureCache();
		assert.strictEqual(cache.shouldPublish("file:///a.txt", [diag()]), true);
	});

	test("skips an identical repeat for the same URI", () => {
		const cache = new DiagnosticsSignatureCache();
		cache.shouldPublish("file:///a.txt", [diag()]);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", [diag()]), false);
	});

	test("publishes when the diagnostics changed", () => {
		const cache = new DiagnosticsSignatureCache();
		cache.shouldPublish("file:///a.txt", [diag()]);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", [diag({ message: "changed" })]), true);
	});

	test("publishes an empty-after-nonempty payload", () => {
		const cache = new DiagnosticsSignatureCache();
		cache.shouldPublish("file:///a.txt", [diag()]);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", []), true);
	});

	test("passes an empty publish for a never-seen URI, then skips the repeat", () => {
		const cache = new DiagnosticsSignatureCache();
		assert.strictEqual(cache.shouldPublish("file:///new.txt", []), true);
		assert.strictEqual(cache.shouldPublish("file:///new.txt", []), false);
	});

	test("tracks URIs independently", () => {
		const cache = new DiagnosticsSignatureCache();
		cache.shouldPublish("file:///a.txt", [diag()]);
		assert.strictEqual(cache.shouldPublish("file:///b.txt", [diag()]), true);
	});

	test("re-publishes the same payload after clear (restart drops the collection)", () => {
		const cache = new DiagnosticsSignatureCache();
		cache.shouldPublish("file:///a.txt", [diag()]);
		assert.strictEqual(cache.shouldPublish("file:///a.txt", [diag()]), false);
		cache.clear();
		assert.strictEqual(cache.shouldPublish("file:///a.txt", [diag()]), true);
	});
});
