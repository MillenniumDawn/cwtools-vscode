import { suite, test, beforeEach, vi } from "vitest";
import * as assert from "assert";

// The logger creates its output channel at module scope, so we must mock
// vscode.window.createOutputChannel before the first import.
const lines: string[] = [];

vi.mock("vscode", () => ({
	window: {
		createOutputChannel: (_name: string) => ({
			appendLine: (msg: string) => {
				lines.push(msg);
			},
		}),
	},
}));

// Static import is safe because vi.mock is hoisted above all imports.
import { logInfo, logWarn, logError } from "../../extension/logger";

suite("logger — logInfo", () => {
	beforeEach(() => {
		lines.length = 0;
	});

	test("writes a plain message to the output channel", () => {
		logInfo("hello world");
		assert.strictEqual(lines.length, 1);
		assert.strictEqual(lines[0], "hello world");
	});

	test("writes multiple messages in order", () => {
		logInfo("first");
		logInfo("second");
		assert.strictEqual(lines.length, 2);
		assert.strictEqual(lines[0], "first");
		assert.strictEqual(lines[1], "second");
	});

	test("handles an empty string", () => {
		logInfo("");
		assert.strictEqual(lines.length, 1);
		assert.strictEqual(lines[0], "");
	});
});

suite("logger — logWarn", () => {
	beforeEach(() => {
		lines.length = 0;
	});

	test("prefixes the message with [WARN]", () => {
		logWarn("something suspicious");
		assert.strictEqual(lines.length, 1);
		assert.strictEqual(lines[0], "[WARN] something suspicious");
	});
});

suite("logger — logError", () => {
	beforeEach(() => {
		lines.length = 0;
	});

	test("prefixes the message with [ERROR] and no suffix when err is omitted", () => {
		logError("something broke");
		assert.strictEqual(lines.length, 1);
		assert.strictEqual(lines[0], "[ERROR] something broke");
	});

	test("appends the Error message when an Error is passed", () => {
		logError("operation failed", new Error("disk full"));
		assert.strictEqual(lines.length, 1);
		assert.strictEqual(lines[0], "[ERROR] operation failed: disk full");
	});

	test("appends the stringified value when a non-Error is passed", () => {
		logError("parse error", { code: 42, detail: "unexpected token" });
		assert.strictEqual(lines.length, 1);
		assert.strictEqual(lines[0], "[ERROR] parse error: [object Object]");
	});

	test("appends nothing extra when null is passed as err", () => {
		logError("something broke", null);
		assert.strictEqual(lines.length, 1);
		assert.strictEqual(lines[0], "[ERROR] something broke");
	});

	test("appends the string when a string is passed as err", () => {
		logError("validation failed", "missing field");
		assert.strictEqual(lines.length, 1);
		assert.strictEqual(lines[0], "[ERROR] validation failed: missing field");
	});
});
