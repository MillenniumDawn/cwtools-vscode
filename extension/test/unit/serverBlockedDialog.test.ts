import * as assert from "assert";
import { suite, test, vi } from "vitest";

const state = vi.hoisted(() => ({
	showErrorMessage: vi.fn(),
	executeCommand: vi.fn(),
	openExternal: vi.fn(),
}));

vi.mock("vscode", async (importOriginal) => ({
	...(await importOriginal<object>()),
	window: { showErrorMessage: state.showErrorMessage },
	commands: { executeCommand: state.executeCommand },
	env: { openExternal: state.openExternal },
}));

import { showServerBlockedDialog } from "../../src/host/serverBlockedDialog";

const blocked = (code: string) =>
	Object.assign(new Error(`spawn ${code}`), { code });

async function choose(button: string | undefined, serverPath?: string) {
	state.showErrorMessage.mockResolvedValue(button);
	const handled = showServerBlockedDialog(blocked("EPERM"), serverPath);
	await state.showErrorMessage.mock.results[0]?.value;
	await Promise.resolve();
	return handled;
}

suite("serverBlockedDialog", () => {
	test("leaves errors without an EPERM/EACCES code to the caller", () => {
		assert.strictEqual(showServerBlockedDialog(new Error("other"), "/srv"), false);
		assert.strictEqual(showServerBlockedDialog(undefined, "/srv"), false);
		assert.deepStrictEqual(state.showErrorMessage.mock.calls, []);
	});

	test.each(["EPERM", "EACCES"])("shows the blocked dialog for %s", (code) => {
		state.showErrorMessage.mockResolvedValue(undefined);

		assert.strictEqual(showServerBlockedDialog(blocked(code), "/srv"), true);

		const [message, ...buttons] = state.showErrorMessage.mock.calls[0] as string[];
		assert.ok(message.includes(`blocked from running (${code})`), message);
		assert.deepStrictEqual(buttons, ["Reveal Server Binary", "Antivirus Help"]);
	});

	test("reveals the server binary when that button is chosen", async () => {
		await choose("Reveal Server Binary", "/opt/cwtools/server");

		assert.deepStrictEqual(state.executeCommand.mock.calls, [
			["revealFileInOS", { fsPath: "/opt/cwtools/server" }],
		]);
		assert.deepStrictEqual(state.openExternal.mock.calls, []);
	});

	test("opens the antivirus help link when that button is chosen", async () => {
		await choose("Antivirus Help", "/opt/cwtools/server");

		assert.strictEqual(state.openExternal.mock.calls.length, 1);
		assert.deepStrictEqual(state.executeCommand.mock.calls, []);
	});

	test.each<[string, string | undefined, string | undefined]>([
		["the dialog is dismissed", undefined, "/opt/cwtools/server"],
		["no server path is known", "Reveal Server Binary", undefined],
	])("opens nothing when %s", async (_name, button, serverPath) => {
		await choose(button, serverPath);

		assert.deepStrictEqual(state.executeCommand.mock.calls, []);
		assert.deepStrictEqual(state.openExternal.mock.calls, []);
	});
});
