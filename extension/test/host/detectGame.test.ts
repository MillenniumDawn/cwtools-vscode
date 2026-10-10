import * as assert from "assert";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import * as vscode from "vscode";
import type { WorkspaceFolder } from "vscode";
import sinon from "sinon";
import { detectGameAndVanilla } from "../../src/host/detectGame";

suite("game detection", function () {
	this.timeout(30_000);

	test("recognized folder hint skips executable discovery", async () => {
		const temp = await fs.mkdtemp(path.join(os.tmpdir(), "cwtools-detection-"));
		const folder = path.join(temp, "HOI4_Mod");
		await fs.mkdir(folder);
		const root = {
			uri: vscode.Uri.file(folder),
			name: "HOI4_Mod",
			index: 0,
		} as WorkspaceFolder;
		const findFiles = sinon.spy(vscode.workspace, "findFiles");
		try {
			assert.deepStrictEqual(await detectGameAndVanilla(root), {
				languageId: "hoi4",
			});
			assert.strictEqual(findFiles.callCount, 0);
		} finally {
			findFiles.restore();
			await fs.rm(temp, { recursive: true, force: true });
		}
	});

	test("unknown folder still searches for a game executable", async () => {
		const temp = await fs.mkdtemp(path.join(os.tmpdir(), "cwtools-detection-"));
		const folder = path.join(temp, "unknown-mod-root");
		await fs.mkdir(folder);
		const executable = path.join(
			folder,
			os.platform() === "win32" ? "hoi4.exe" : "hoi4",
		);
		await fs.writeFile(executable, "test executable");
		if (os.platform() !== "win32") await fs.chmod(executable, 0o755);
		const root = {
			uri: vscode.Uri.file(folder),
			name: "unknown-mod-root",
			index: 0,
		} as WorkspaceFolder;
		const findFiles = sinon.spy(vscode.workspace, "findFiles");
		try {
			assert.deepStrictEqual(await detectGameAndVanilla(root), {
				languageId: "hoi4",
			});
			assert.ok(findFiles.callCount > 0, "unknown roots should use executable search");
		} finally {
			findFiles.restore();
			await fs.rm(temp, { recursive: true, force: true });
		}
	});
});
