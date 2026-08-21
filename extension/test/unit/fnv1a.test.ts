import { suite, test } from "vitest";
import * as assert from "assert";
import { fnv1a } from "../../src/host/fnv1a";

suite("fnv1a", () => {
	test("is deterministic for a given input", () => {
		assert.strictEqual(fnv1a("hello"), fnv1a("hello"));
		assert.strictEqual(fnv1a("hello"), fnv1a("hello"));
	});

	test("empty string hashes to the FNV-1a offset basis", () => {
		assert.strictEqual(fnv1a(""), 0x811c9dc5);
	});

	test("matches known FNV-1a 32-bit vectors", () => {
		assert.strictEqual(fnv1a("a"), 0xe40c292c);
		assert.strictEqual(fnv1a("foobar"), 0xbf9cf968);
		assert.strictEqual(fnv1a("hello"), 0x4f9f2cab);
		assert.strictEqual(fnv1a("chongo was here!"), 0x448524fd);
	});

	test("different inputs collide only rarely (distinct for representative strings)", () => {
		const seen = new Set(["", "a", "b", "foobar", "hello", "goodbye"].map(fnv1a));
		assert.strictEqual(seen.size, 6);
	});

	test("returns an unsigned 32-bit value", () => {
		for (const s of ["", "a", "the quick brown fox jumps over the lazy dog"]) {
			const h = fnv1a(s);
			assert.ok(Number.isInteger(h), `hash ${h} should be an integer`);
			assert.ok(h >= 0 && h <= 0xffffffff, `hash ${h} out of unsigned 32-bit range`);
		}
	});
});
