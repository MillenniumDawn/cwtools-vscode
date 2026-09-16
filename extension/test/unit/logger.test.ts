import { suite, test, beforeEach } from "vitest";
import * as assert from "assert";
import type { LogOutputChannel } from "vscode";

const messages: Array<{ level: string; message: string }> = [];
const channel = {
	appendLine: (message: string) => messages.push({ level: "append", message }),
	info: (message: string) => messages.push({ level: "info", message }),
	warn: (message: string) => messages.push({ level: "warn", message }),
	error: (message: string) => messages.push({ level: "error", message }),
	show: () => undefined,
	dispose: () => undefined,
} as unknown as LogOutputChannel;

import {
	initializeLogger,
	logInfo,
	logWarn,
	logError,
	outputChannel,
} from "../../src/host/logger";

suite("logger", () => {
	beforeEach(() => {
		messages.length = 0;
		initializeLogger(channel);
	});

	// The channel is supplied by enabled activation, not created while this
	// module is imported.
	test("uses the initialized channel for helpers and direct output", () => {
		assert.strictEqual(outputChannel, channel);
		logInfo("via helper");
		outputChannel.appendLine("direct");
		assert.deepStrictEqual(messages, [
			{ level: "info", message: "via helper" },
			{ level: "append", message: "direct" },
		]);
	});

	test("logs info messages without a hand-written level prefix", () => {
		logInfo("hello world");
		assert.deepStrictEqual(messages, [
			{ level: "info", message: "hello world" },
		]);
	});

	test("logs warning messages through the warning level", () => {
		logWarn("something suspicious");
		assert.deepStrictEqual(messages, [
			{ level: "warn", message: "something suspicious" },
		]);
	});

	test("logs errors through the error level", () => {
		logError("something broke");
		assert.deepStrictEqual(messages, [
			{ level: "error", message: "something broke" },
		]);
	});

	test("preserves Error messages", () => {
		logError("operation failed", new Error("disk full"));
		assert.deepStrictEqual(messages, [
			{ level: "error", message: "operation failed: disk full" },
		]);
	});

	test("preserves stringified unknown errors", () => {
		logError("parse error", { code: 42, detail: "unexpected token" });
		assert.deepStrictEqual(messages, [
			{ level: "error", message: "parse error: [object Object]" },
		]);
	});

	test("omits null and undefined error suffixes", () => {
		logError("null error", null);
		logError("omitted error");
		assert.deepStrictEqual(messages, [
			{ level: "error", message: "null error" },
			{ level: "error", message: "omitted error" },
		]);
	});

	test("preserves string error suffixes", () => {
		logError("validation failed", "missing field");
		assert.deepStrictEqual(messages, [
			{ level: "error", message: "validation failed: missing field" },
		]);
	});
});
