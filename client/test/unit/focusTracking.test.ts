import { suite, test } from "vitest";
import * as assert from "assert";
import {
	shouldNotifyFocus,
	pendingProcessDelayMs,
} from "../../extension/focusTracking";

suite("focusTracking — shouldNotifyFocus", () => {
	test("notifies when no focus has been sent yet", () => {
		assert.strictEqual(shouldNotifyFocus("file:///a.txt", undefined), true);
	});

	test("notifies when the focused URI changed", () => {
		assert.strictEqual(shouldNotifyFocus("file:///b.txt", "file:///a.txt"), true);
	});

	test("skips a repeat of the last-sent URI", () => {
		assert.strictEqual(shouldNotifyFocus("file:///a.txt", "file:///a.txt"), false);
	});
});

suite("focusTracking — pendingProcessDelayMs", () => {
	test("backs off after a timeout", () => {
		assert.strictEqual(pendingProcessDelayMs(true, 2000), 2000);
	});

	test("drains immediately after a settled response", () => {
		assert.strictEqual(pendingProcessDelayMs(false, 2000), 0);
	});
});
