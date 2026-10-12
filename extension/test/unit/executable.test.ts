import { suite, test, afterAll, beforeAll } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { existAndIsExe } from "../../src/host/executable";

suite("executable — existAndIsExe", () => {
	let tmpDir: string;

	beforeAll(() => {
		tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cwtools-test-"));
	});

	afterAll(() => {
		fs.rmSync(tmpDir, { recursive: true, force: true });
	});

	function makeFile(name: string, mode: number): string {
		const file = path.join(tmpDir, name);
		fs.writeFileSync(file, "test", { mode });
		return file;
	}

	test.each([123, undefined, null])("rejects the non-string argument %j", async (value) => {
		await assert.rejects(() => existAndIsExe(value as unknown as string), {
			name: "TypeError",
			message: "Expected a string",
		});
	});

	test.each([
		["no-exec", 0o644, false],
		["owner-exec", 0o744, true],
		["group-exec", 0o710, true],
		["others-exec", 0o701, true],
	])("a %s file with mode %o is executable: %s", async (name, mode, expected) => {
		assert.strictEqual(await existAndIsExe(makeFile(name, mode)), expected);
	});

	test("returns false for a missing path and for a directory", async () => {
		assert.strictEqual(await existAndIsExe(path.join(tmpDir, "missing")), false);
		assert.strictEqual(await existAndIsExe(tmpDir), false);
	});

	test.each([
		["exec-target", 0o755, true],
		["plain-target", 0o644, false],
	])("follows a symlink to %s", async (name, mode, expected) => {
		const link = path.join(tmpDir, `link-to-${name}`);
		fs.symlinkSync(makeFile(name, mode), link);
		assert.strictEqual(await existAndIsExe(link), expected);
	});

	test("returns false for a symlink to a missing target", async () => {
		const link = path.join(tmpDir, "broken-link");
		fs.symlinkSync(path.join(tmpDir, "no-such-target"), link);
		assert.strictEqual(await existAndIsExe(link), false);
	});
});
