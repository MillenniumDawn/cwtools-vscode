import assert from "node:assert/strict";
import { copyFile, mkdtemp, mkdir, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { test } from "node:test";

import { checkHostTestDiscovery } from "./host-test-discovery.mjs";

const execFileAsync = promisify(execFile);

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

test("runs the discovery guard when invoked through a directory symlink", async () => {
	const parent = await mkdtemp(path.join(os.tmpdir(), "cwtools-host-link-test-"));
	const root = path.join(parent, "repo");
	const linkedRoot = path.join(parent, "repo-link");
	const scriptDirectory = path.join(root, "scripts/build");
	const hostDirectory = path.join(root, "extension/test/host");

	try {
		await mkdir(scriptDirectory, { recursive: true });
		await mkdir(hostDirectory, { recursive: true });
		await copyFile(
			new URL("./host-test-discovery.mjs", import.meta.url),
			path.join(scriptDirectory, "host-test-discovery.mjs"),
		);
		await writeFile(path.join(hostDirectory, "unassigned.test.ts"), "", "utf8");
		await writeFile(path.join(root, ".vscode-test.mjs"), "export default { tests: [] };\n", "utf8");
		await symlink(root, linkedRoot, process.platform === "win32" ? "junction" : "dir");
		const linkedScript = path.join(linkedRoot, "scripts/build/host-test-discovery.mjs");
		await assert.rejects(
			execFileAsync(process.execPath, [linkedScript]),
			(error) =>
				error.code === 1 &&
				error.stderr.includes("extension/test/host/unassigned.test.ts"),
		);
	} finally {
		await rm(parent, { recursive: true, force: true });
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
