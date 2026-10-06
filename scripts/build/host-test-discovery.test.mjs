import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

import { checkHostTestDiscovery } from "./host-test-discovery.mjs";

test("rejects a new host test missing from the label file lists", async () => {
	const root = await mkdtemp(path.join(os.tmpdir(), "cwtools-host-discovery-"));
	const hostDirectory = path.join(root, "extension/test/host");
	await mkdir(hostDirectory, { recursive: true });
	await writeFile(path.join(hostDirectory, "assigned.test.ts"), "", "utf8");
	await writeFile(
		path.join(root, ".vscode-test.mjs"),
		'export default { tests: [{ files: ["./dist/extension/bin/client/test/host/assigned.test.js"] }] };\n',
		"utf8",
	);

	try {
		await checkHostTestDiscovery(root);
		await writeFile(path.join(hostDirectory, "extra.test.ts"), "", "utf8");
		await assert.rejects(
			checkHostTestDiscovery(root),
			/extension\/test\/host\/extra\.test\.ts/,
		);
	} finally {
		await rm(root, { recursive: true, force: true });
	}
});


test("rejects a configured host test whose source file is missing", async () => {
	const root = await mkdtemp(path.join(os.tmpdir(), "cwtools-host-stale-test-"));
	const hostDirectory = path.join(root, "extension/test/host");
	await mkdir(hostDirectory, { recursive: true });
	await writeFile(
		path.join(root, ".vscode-test.mjs"),
		'export default { tests: [{ files: ["./dist/extension/bin/client/test/host/removed.test.js"] }] };\n',
		"utf8",
	);

	try {
		await assert.rejects(
			checkHostTestDiscovery(root),
			/Configured host test files missing from extension\/test\/host: extension\/test\/host\/removed\.test\.ts/,
		);
	} finally {
		await rm(root, { recursive: true, force: true });
	}
});
