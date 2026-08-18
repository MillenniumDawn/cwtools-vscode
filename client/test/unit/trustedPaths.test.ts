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

vi.mock("../../extension/logger", () => ({
	logWarn: vi.fn(),
}));

import {
	confirmOpen,
	isTrustedPath,
	setTrustedRoots,
} from "../../extension/trustedPaths";

const fileUri = (fsPath: string): Uri =>
	({
		scheme: "file",
		fsPath,
		toString: () => `file://${fsPath}`,
	}) as Uri;

suite("isTrustedPath", () => {
	test("rejects a relative path", () => {
		assert.strictEqual(
			isTrustedPath("mod/a.txt", ["/roots/mod"], "linux"),
			false,
		);
	});

	test("accepts a file inside a root", () => {
		assert.strictEqual(
			isTrustedPath("/roots/mod/events/a.txt", ["/roots/mod"], "linux"),
			true,
		);
	});

	test("accepts the root itself", () => {
		assert.strictEqual(
			isTrustedPath("/roots/mod", ["/roots/mod"], "linux"),
			true,
		);
	});

	test("rejects a sibling that shares the root's prefix", () => {
		assert.strictEqual(
			isTrustedPath("/roots/mod-evil/a.txt", ["/roots/mod"], "linux"),
			false,
		);
	});

	test("rejects a path that climbs out of the root", () => {
		assert.strictEqual(
			isTrustedPath("/roots/mod/../../etc/passwd", ["/roots/mod"], "linux"),
			false,
		);
	});

	test("accepts a path that climbs but stays inside", () => {
		assert.strictEqual(
			isTrustedPath("/roots/mod/events/../a.txt", ["/roots/mod"], "linux"),
			true,
		);
	});

	test("rejects everything when there are no roots", () => {
		assert.strictEqual(isTrustedPath("/roots/mod/a.txt", [], "linux"), false);
	});

	test("ignores a relative root", () => {
		assert.strictEqual(
			isTrustedPath("/roots/mod/a.txt", ["mod"], "linux"),
			false,
		);
	});

	test("matches a root case-insensitively on win32", () => {
		assert.strictEqual(
			isTrustedPath("c:\\mods\\mine\\a.txt", ["C:\\Mods\\Mine"], "win32"),
			true,
		);
	});

	test("matches forward slashes against a backslash root on win32", () => {
		assert.strictEqual(
			isTrustedPath("C:/Mods/Mine/a.txt", ["C:\\Mods\\Mine"], "win32"),
			true,
		);
	});

	test("rejects a UNC path when the roots are local on win32", () => {
		assert.strictEqual(
			isTrustedPath("\\\\attacker\\share\\a.txt", ["C:\\Mods"], "win32"),
			false,
		);
	});

	test("accepts a file under a UNC root on win32", () => {
		assert.strictEqual(
			isTrustedPath(
				"\\\\server\\share\\mod\\a.txt",
				["\\\\server\\share\\mod"],
				"win32",
			),
			true,
		);
	});
});

suite("confirmOpen", () => {
	beforeEach(() => {
		showWarningMessage.mockReset();
		state.folders = [{ uri: { fsPath: "/roots/mod" } }];
		setTrustedRoots(["/cache/.cwtools", undefined, "  "]);
	});

	test("opens a workspace file without prompting", async () => {
		assert.strictEqual(await confirmOpen(fileUri("/roots/mod/a.txt")), true);
		assert.strictEqual(showWarningMessage.mock.calls.length, 0);
	});

	test("opens a file under a configured root without prompting", async () => {
		assert.strictEqual(
			await confirmOpen(fileUri("/cache/.cwtools/hoi4/rules.cwt")),
			true,
		);
		assert.strictEqual(showWarningMessage.mock.calls.length, 0);
	});

	test("refuses a non-file uri without prompting", async () => {
		const uri = {
			scheme: "http",
			fsPath: "/roots/mod/a.txt",
			toString: () => "http://evil/a.txt",
		} as Uri;
		assert.strictEqual(await confirmOpen(uri), false);
		assert.strictEqual(showWarningMessage.mock.calls.length, 0);
	});

	test("refuses a relative file path without prompting", async () => {
		assert.strictEqual(await confirmOpen(fileUri("a.txt")), false);
		assert.strictEqual(showWarningMessage.mock.calls.length, 0);
	});

	test("opens a path outside every root once the user confirms", async () => {
		showWarningMessage.mockResolvedValue("Open");
		assert.strictEqual(await confirmOpen(fileUri("/etc/passwd")), true);
		assert.strictEqual(showWarningMessage.mock.calls.length, 1);
	});

	test("leaves a path outside every root closed when the prompt is dismissed", async () => {
		showWarningMessage.mockResolvedValue(undefined);
		assert.strictEqual(await confirmOpen(fileUri("/etc/passwd")), false);
	});

	test("tracks a workspace folder added after activation", async () => {
		state.folders = [
			{ uri: { fsPath: "/roots/mod" } },
			{ uri: { fsPath: "/roots/other" } },
		];
		assert.strictEqual(await confirmOpen(fileUri("/roots/other/a.txt")), true);
		assert.strictEqual(showWarningMessage.mock.calls.length, 0);
	});
});
