import { suite, test, afterEach } from 'vitest';
import * as assert from 'assert';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { existAndIsExe } from '../../extension/executable';

suite('executable — existAndIsExe', () => {
	const tmpDir = os.tmpdir();

	function tempFile(name: string): string {
		return path.join(tmpDir, `cwtools-test-${Date.now()}-${name}`);
	}

	// Track files we create so cleanup always runs.
	const created: string[] = [];

	function makeFile(name: string, opts?: { executable?: boolean }): string {
		const p = tempFile(name);
		fs.writeFileSync(p, 'test', { mode: opts?.executable ? 0o755 : 0o644 });
		created.push(p);
		return p;
	}

	afterEach(() => {
		for (const p of created) {
			try { fs.unlinkSync(p); } catch { /* already gone */ }
		}
		created.length = 0;
	});

	test('throws TypeError when called with a non-string argument', async () => {
		await assert.rejects(
			() => existAndIsExe(123 as unknown as string),
			{ name: 'TypeError', message: 'Expected a string' }
		);
	});

	test('throws TypeError for undefined', async () => {
		await assert.rejects(
			() => existAndIsExe(undefined as unknown as string),
			{ name: 'TypeError', message: 'Expected a string' }
		);
	});

	test('returns false for a non-existent path', async () => {
		const result = await existAndIsExe(path.join(tmpDir, 'cwtools-no-such-file-does-not-exist'));
		assert.strictEqual(result, false);
	});

	test('returns false for a regular file without execute permission', async () => {
		const p = makeFile('noexec.txt', { executable: false });
		const result = await existAndIsExe(p);
		assert.strictEqual(result, false);
	});

	test('returns true for an executable file', async () => {
		const p = makeFile('executable.txt', { executable: true });
		const result = await existAndIsExe(p);
		assert.strictEqual(result, true);
	});

	test('returns false for a directory (not a file)', async () => {
		// Directories pass stat() but fail isFile().
		const result = await existAndIsExe(tmpDir);
		assert.strictEqual(result, false);
	});

	test('returns false for a symlink to a non-existent target', async () => {
		const p = tempFile('broken-symlink');
		fs.symlinkSync('/no/such/target', p);
		created.push(p);
		// stat() throws ENOENT for broken symlinks, caught by the outer catch.
		const result = await existAndIsExe(p);
		assert.strictEqual(result, false);
	});
});
