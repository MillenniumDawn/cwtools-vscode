import { beforeEach, suite, test, vi } from "vitest";
import * as assert from "assert";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";

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

	test("descriptor-bearing HOI4 content survives relocation beneath game-named ancestors", async () => {
		const temp = await fs.mkdtemp(path.join(os.tmpdir(), "cwtools-detection-"));
		mocks.findFiles.mockResolvedValue([]);
		try {
			for (const parent of ["neutral", "stellaris", "europa"]) {
				const folder = path.join(temp, parent, "Millennium-Dawn");
				await fs.mkdir(path.join(folder, "common", "ai_strategy"), { recursive: true });
				await fs.writeFile(path.join(folder, "descriptor.mod"), 'name="MD fixture"');
				mocks.findFiles.mockClear();
				assert.deepStrictEqual(await detectGameAndVanilla({
					uri: { fsPath: folder }, name: "Millennium-Dawn", index: 0,
				} as never), { languageId: "hoi4" });
				assert.strictEqual(mocks.findFiles.mock.calls.length, 1);
				const [pattern] = mocks.findFiles.mock.calls[0] as [{ pattern: string }];
				assert.match(pattern.pattern, /hoi4/);
			}
		} finally {
			await fs.rm(temp, { recursive: true, force: true });
		}
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
