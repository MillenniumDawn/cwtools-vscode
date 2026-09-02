import { beforeEach, suite, test, vi } from "vitest";
import * as assert from "assert";

const mocks = vi.hoisted(() => ({
	findFiles: vi.fn(),
	existAndIsExe: vi.fn(),
	workspaceFolders: [{ uri: { fsPath: "/opaque-cwtools-workspace" } }],
}));

vi.mock("vscode", () => ({
	workspace: {
		workspaceFolders: mocks.workspaceFolders,
		findFiles: mocks.findFiles,
	},
	RelativePattern: class RelativePattern {
		constructor(
			_root: unknown,
			public readonly pattern: string,
		) {}
	},
	window: {
		createOutputChannel: () => ({ appendLine: vi.fn() }),
	},
}));

vi.mock("../../src/host/executable", () => ({
	existAndIsExe: mocks.existAndIsExe,
}));

import { detectGameAndVanilla } from "../../src/host/detectGame";
import { GAMES } from "../../src/host/games";

suite("detectGameAndVanilla", () => {
	beforeEach(() => {
		mocks.findFiles.mockReset();
		mocks.existAndIsExe.mockReset();
		mocks.findFiles.mockImplementation((pattern: { pattern: string }) =>
			Promise.resolve(
				pattern.pattern.includes("hoi4")
					? [{ fsPath: "/opaque-cwtools-workspace/hoi4" }]
					: [],
			),
		);
		mocks.existAndIsExe.mockResolvedValue(true);
	});

	test("pins generic detection to the discovered game executable", async () => {
		assert.deepStrictEqual(await detectGameAndVanilla(), {
			languageId: "hoi4",
		});
		assert.strictEqual(mocks.findFiles.mock.calls.length, GAMES.length);
	});
});
