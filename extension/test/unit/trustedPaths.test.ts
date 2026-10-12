import { beforeEach, suite, test, vi } from "vitest";
import * as assert from "assert";
import type { Uri } from "vscode";

const { showWarningMessage, state } = vi.hoisted(() => ({
	showWarningMessage: vi.fn(),
	state: { folders: [] as { uri: { fsPath: string } }[] },
}));

vi.mock("vscode", async (importOriginal) => ({
	...(await importOriginal<object>()),
	window: { showWarningMessage },
	workspace: {
		get workspaceFolders() {
			return state.folders;
		},
	},
}));

vi.mock("../../src/host/logger", async () =>
	(await import("./support/loggerMock")).mockLogger(),
);

import {
	confirmOpen,
	isTrustedPath,
	setTrustedRoots,
} from "../../src/host/trustedPaths";

const uri = (fsPath: string, scheme = "file"): Uri =>
	({ scheme, fsPath, toString: () => `${scheme}://${fsPath}` }) as Uri;

suite("isTrustedPath", () => {
	test.each<[string, string, string[], NodeJS.Platform, boolean]>([
		["a file inside a root", "/roots/mod/events/a.txt", ["/roots/mod"], "linux", true],
		["the root itself", "/roots/mod", ["/roots/mod"], "linux", true],
		["a path that climbs but stays inside", "/roots/mod/events/../a.txt", ["/roots/mod"], "linux", true],
		["a relative path", "mod/a.txt", ["/roots/mod"], "linux", false],
		["a sibling that shares the root's prefix", "/roots/mod-evil/a.txt", ["/roots/mod"], "linux", false],
		["a path that climbs out of the root", "/roots/mod/../../etc/passwd", ["/roots/mod"], "linux", false],
		["anything when there are no roots", "/roots/mod/a.txt", [], "linux", false],
		["a file under a relative root", "/roots/mod/a.txt", ["mod"], "linux", false],
		["a differently cased win32 path", "c:\\mods\\mine\\a.txt", ["C:\\Mods\\Mine"], "win32", true],
		["forward slashes against a win32 root", "C:/Mods/Mine/a.txt", ["C:\\Mods\\Mine"], "win32", true],
		["a file under a UNC root", "\\\\server\\share\\mod\\a.txt", ["\\\\server\\share\\mod"], "win32", true],
		["a UNC path when the roots are local", "\\\\attacker\\share\\a.txt", ["C:\\Mods"], "win32", false],
	])("%s: %s", (_name, file, roots, platform, trusted) => {
		assert.strictEqual(isTrustedPath(file, roots, platform), trusted);
	});
});

suite("confirmOpen", () => {
	beforeEach(() => {
		showWarningMessage.mockReset();
		state.folders = [{ uri: { fsPath: "/roots/mod" } }];
		setTrustedRoots(["/cache/.cwtools", undefined, "  "]);
	});

	test.each([
		["a workspace file", "/roots/mod/a.txt"],
		["a file under a configured root", "/cache/.cwtools/hoi4/rules.cwt"],
	])("opens %s without prompting", async (_name, file) => {
		assert.strictEqual(await confirmOpen(uri(file)), true);
		assert.deepStrictEqual(showWarningMessage.mock.calls, []);
	});

	test("tracks a workspace folder added after activation", async () => {
		state.folders.push({ uri: { fsPath: "/roots/other" } });
		assert.strictEqual(await confirmOpen(uri("/roots/other/a.txt")), true);
		assert.deepStrictEqual(showWarningMessage.mock.calls, []);
	});

	test.each([
		["a non-file uri", uri("/roots/mod/a.txt", "http")],
		["a relative file path", uri("a.txt")],
	])("refuses %s without prompting", async (_name, target) => {
		assert.strictEqual(await confirmOpen(target), false);
		assert.deepStrictEqual(showWarningMessage.mock.calls, []);
	});

	test.each([
		["Open", true],
		[undefined, false],
	])("a path outside every root follows the prompt answer %j", async (answer, opened) => {
		showWarningMessage.mockResolvedValue(answer);
		assert.strictEqual(await confirmOpen(uri("/etc/passwd")), opened);
		assert.strictEqual(showWarningMessage.mock.calls.length, 1);
	});
});
