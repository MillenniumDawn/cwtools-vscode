import * as assert from "assert";
import { beforeEach, suite, test, vi } from "vitest";

const state = vi.hoisted(() => ({
	showErrorMessage: vi.fn(),
	executeCommand: vi.fn(),
	openExternal: vi.fn(),
}));

vi.mock("vscode", () => ({
	window: { showErrorMessage: state.showErrorMessage },
	commands: { executeCommand: state.executeCommand },
	env: { openExternal: state.openExternal },
	Uri: {
		file: (fsPath: string) => ({ fsPath }),
		parse: (value: string) => ({ toString: () => value }),
	},
	l10n: {
		t(message: string, ...args: Array<string | number | boolean>): string {
			return message.replace(/\{(\d+)\}/g, (placeholder, index: string) => {
				const arg = args[Number(index)];
				return arg === undefined ? placeholder : String(arg);
			});
		},
	},
}));

import { showServerBlockedDialog } from "../../src/host/serverBlockedDialog";

suite("serverBlockedDialog", () => {
	beforeEach(() => {
		vi.clearAllMocks();
		// clearAllMocks only wipes call history, not the implementation set by
		// mockResolvedValue, so every test starts from the same "no button
		// chosen" default and opts in to a specific choice where it matters.
		state.showErrorMessage.mockResolvedValue(undefined);
	});

	test("does not handle an error without an EPERM/EACCES code", () => {
		const handled = showServerBlockedDialog(
			new Error("something else"),
			"/opt/cwtools/server",
		);
		assert.strictEqual(handled, false);
		assert.strictEqual(state.showErrorMessage.mock.calls.length, 0);
	});

	test("does not handle an error with no code at all", () => {
		const handled = showServerBlockedDialog(undefined, "/opt/cwtools/server");
		assert.strictEqual(handled, false);
	});

	for (const code of ["EPERM", "EACCES"]) {
		test(`shows the blocked dialog for ${code}`, () => {
			const err = Object.assign(new Error(`spawn ${code}`), { code });

			const handled = showServerBlockedDialog(err, "/opt/cwtools/server");

			assert.strictEqual(handled, true);
			assert.strictEqual(state.showErrorMessage.mock.calls.length, 1);
			const [message, reveal, help] = state.showErrorMessage.mock.calls[0] as [
				string,
				string,
				string,
			];
			assert.match(message, new RegExp(`blocked from running \\(${code}\\)`));
			assert.strictEqual(reveal, "Reveal Server Binary");
			assert.strictEqual(help, "Antivirus Help");
		});
	}

	test("reveals the server binary when that button is chosen", async () => {
		state.showErrorMessage.mockResolvedValue("Reveal Server Binary");
		const err = Object.assign(new Error("spawn EPERM"), { code: "EPERM" });

		showServerBlockedDialog(err, "/opt/cwtools/server");

		await vi.waitFor(() => {
			assert.strictEqual(state.executeCommand.mock.calls.length, 1);
		});
		assert.deepStrictEqual(state.executeCommand.mock.calls[0], [
			"revealFileInOS",
			{ fsPath: "/opt/cwtools/server" },
		]);
		assert.strictEqual(state.openExternal.mock.calls.length, 0);
	});

	test("does nothing when the reveal button is chosen but no server path is known", async () => {
		state.showErrorMessage.mockResolvedValue("Reveal Server Binary");
		const err = Object.assign(new Error("spawn EPERM"), { code: "EPERM" });

		showServerBlockedDialog(err, undefined);

		await state.showErrorMessage.mock.results[0]?.value;
		assert.strictEqual(state.executeCommand.mock.calls.length, 0);
	});

	test("opens the antivirus help link when that button is chosen", async () => {
		state.showErrorMessage.mockResolvedValue("Antivirus Help");
		const err = Object.assign(new Error("spawn EACCES"), { code: "EACCES" });

		showServerBlockedDialog(err, "/opt/cwtools/server");

		await vi.waitFor(() => {
			assert.strictEqual(state.openExternal.mock.calls.length, 1);
		});
		assert.strictEqual(state.executeCommand.mock.calls.length, 0);
	});

	test("dismissing the dialog opens nothing", async () => {
		state.showErrorMessage.mockResolvedValue(undefined);
		const err = Object.assign(new Error("spawn EPERM"), { code: "EPERM" });

		showServerBlockedDialog(err, "/opt/cwtools/server");

		await state.showErrorMessage.mock.results[0]?.value;
		assert.strictEqual(state.executeCommand.mock.calls.length, 0);
		assert.strictEqual(state.openExternal.mock.calls.length, 0);
	});
});
