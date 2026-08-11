import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { runPlatformPackaging } from "./build";

// runPlatformPackaging is the holding/restore dance behind packageAllVsixes.
// It shuffles real per-platform directories around a serverBinDir it does not
// own, so the tests stage real directories in os.tmpdir() and drive the
// orchestration with an injected packageOne. No fs mocking: the filesystem is
// the boundary, and a real tmpdir exercises the rename/copy/rm calls exactly
// as production does.

interface Call {
	platform: string | undefined;
	present: string[];
}

function mkdtemp(): string {
	return fs.mkdtempSync(path.join(os.tmpdir(), "cw-platform-pkg-"));
}

// Stage one marker file per platform under serverBinDir/<platform>/.
function stage(serverBinDir: string, platforms: string[]): void {
	for (const platform of platforms) {
		const dir = path.join(serverBinDir, platform);
		fs.mkdirSync(dir, { recursive: true });
		fs.writeFileSync(path.join(dir, "cwtools-server"), `binary-${platform}`);
	}
}

function dirsPresent(dir: string): string[] {
	if (!fs.existsSync(dir)) return [];
	return fs
		.readdirSync(dir, { withFileTypes: true })
		.filter((e) => e.isDirectory())
		.map((e) => e.name)
		.sort();
}

function hasBinary(dir: string, platform: string): boolean {
	return fs.existsSync(path.join(dir, platform, "cwtools-server"));
}

suite("runPlatformPackaging — staged binary shuffle and restore", () => {
	test("each per-platform pass sees only its own platform dir in serverBinDir", () => {
		const root = mkdtemp();
		const serverBinDir = path.join(root, "server/cwtools-server");
		const holding = path.join(root, "temp/server-staging");
		const platforms = ["linux-x64", "osx-arm64", "win32-x64"];
		stage(serverBinDir, platforms);

		const seen: Call[] = [];
		runPlatformPackaging(serverBinDir, holding, platforms, (platform) => {
			// During a per-platform pass only that platform's dir may be present,
			// so vsce cannot sweep the others into the vsix.
			seen.push({ platform, present: dirsPresent(serverBinDir) });
			return [platform ?? "universal"];
		});

		const perPlatform = seen.filter((c) => c.platform !== undefined);
		assert.strictEqual(perPlatform.length, platforms.length);
		for (const c of perPlatform) {
			assert.deepStrictEqual(c.present, [c.platform as string]);
		}
		fs.rmSync(root, { recursive: true, force: true });
	});

	test("restores the full staged set when a mid-platform pass throws (#129)", () => {
		const root = mkdtemp();
		const serverBinDir = path.join(root, "server/cwtools-server");
		const holding = path.join(root, "temp/server-staging");
		const platforms = ["linux-x64", "osx-arm64", "win32-x64"];
		stage(serverBinDir, platforms);

		let threw = false;
		try {
			runPlatformPackaging(serverBinDir, holding, platforms, (platform) => {
				if (platform === "osx-arm64") throw new Error("vsce boom");
				return [platform ?? "universal"];
			});
		} catch (e) {
			threw = true;
			assert.match((e as Error).message, /vsce boom/);
		}

		assert.ok(threw, "expected the mid-platform failure to propagate");
		// The full staged set must be back in serverBinDir, with the holding
		// dir (the only complete copy) cleaned up so nothing is left behind.
		assert.deepStrictEqual(dirsPresent(serverBinDir), platforms);
		for (const p of platforms) assert.ok(hasBinary(serverBinDir, p));
		assert.ok(!fs.existsSync(holding), "holding dir must be deleted in finally");
		fs.rmSync(root, { recursive: true, force: true });
	});

	test("leaves the full set intact when the universal fallback pass throws", () => {
		const root = mkdtemp();
		const serverBinDir = path.join(root, "server/cwtools-server");
		const holding = path.join(root, "temp/server-staging");
		const platforms = ["linux-x64", "osx-arm64", "win32-x64"];
		stage(serverBinDir, platforms);

		let threw = false;
		try {
			runPlatformPackaging(serverBinDir, holding, platforms, (platform) => {
				if (platform === undefined) throw new Error("universal boom");
				return [platform ?? "universal"];
			});
		} catch {
			threw = true;
		}

		assert.ok(threw, "expected the universal failure to propagate");
		// The full set is restored before the universal pass, so the failure
		// leaves it intact and the finally does not need to re-restore.
		assert.deepStrictEqual(dirsPresent(serverBinDir), platforms);
		for (const p of platforms) assert.ok(hasBinary(serverBinDir, p));
		assert.ok(!fs.existsSync(holding));
		fs.rmSync(root, { recursive: true, force: true });
	});

	test("deletes the holding dir and leaves the full set on full success", () => {
		const root = mkdtemp();
		const serverBinDir = path.join(root, "server/cwtools-server");
		const holding = path.join(root, "temp/server-staging");
		const platforms = ["linux-x64", "osx-arm64", "win32-x64"];
		stage(serverBinDir, platforms);

		const vsixes = runPlatformPackaging(
			serverBinDir,
			holding,
			platforms,
			(platform) => [platform ?? "universal"],
		);

		// One vsix per platform plus the universal fallback, in order.
		assert.deepStrictEqual(vsixes, [
			"linux-x64",
			"osx-arm64",
			"win32-x64",
			"universal",
		]);
		assert.deepStrictEqual(dirsPresent(serverBinDir), platforms);
		assert.ok(!fs.existsSync(holding));
		fs.rmSync(root, { recursive: true, force: true });
	});

	test("restores the single staged set on a one-platform mid-failure (boundary)", () => {
		const root = mkdtemp();
		const serverBinDir = path.join(root, "server/cwtools-server");
		const holding = path.join(root, "temp/server-staging");
		const platforms = ["linux-x64"];
		stage(serverBinDir, platforms);

		let threw = false;
		try {
			runPlatformPackaging(serverBinDir, holding, platforms, () => {
				throw new Error("vsce boom");
			});
		} catch {
			threw = true;
		}

		assert.ok(threw);
		assert.deepStrictEqual(dirsPresent(serverBinDir), ["linux-x64"]);
		assert.ok(hasBinary(serverBinDir, "linux-x64"));
		assert.ok(!fs.existsSync(holding));
		fs.rmSync(root, { recursive: true, force: true });
	});
});