import { beforeEach, suite, test, vi } from "vitest";
import * as assert from "assert";

const mocks = vi.hoisted(() => ({
	findFiles: vi.fn(),
	existAndIsExe: vi.fn(),
}));

vi.mock("vscode", () => ({
	workspace: {
		findFiles: mocks.findFiles,
	},
	RelativePattern: class RelativePattern {
		constructor(
			public readonly root: unknown,
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
	const root = {
		uri: { fsPath: "/opaque-cwtools-workspace" },
		name: "opaque-cwtools-workspace",
		index: 0,
	} as never;

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
		assert.deepStrictEqual(await detectGameAndVanilla(root), {
			languageId: "hoi4",
		});
		assert.strictEqual(mocks.findFiles.mock.calls.length, GAMES.length);
	});

	test("searches for executables under the selected workspace root", async () => {
		const selectedRoot = {
			uri: { fsPath: "/selected-root" },
			name: "selected-root",
			index: 1,
		} as never;
		mocks.findFiles.mockResolvedValue([]);

		await detectGameAndVanilla(selectedRoot);

		assert.strictEqual(mocks.findFiles.mock.calls.length, GAMES.length);
		const calls = mocks.findFiles.mock.calls as unknown as Array<
			[{ root: unknown }]
		>;
		assert.ok(
			calls.every(([pattern]) => pattern.root === selectedRoot),
			"every executable search should use the selected root",
		);
	});
});
