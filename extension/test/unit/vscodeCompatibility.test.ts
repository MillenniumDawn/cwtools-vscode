import { readFileSync } from "node:fs";
import path from "node:path";
import { expect, test } from "vitest";

const repoRoot = process.cwd();
type Manifest = { engines: { vscode: string } };
type Lockfile = {
	packages: Record<string, { engines?: { vscode?: string } }>;
};

function readJson<T>(file: string): T {
	return JSON.parse(readFileSync(path.join(repoRoot, file), "utf8")) as T;
}

const rootPackage = readJson<Manifest>("package.json");
const extensionPackage = readJson<Manifest>("extension/package/package.json");
const packageLock = readJson<Lockfile>("package-lock.json");

function minimumVersion(range: string): [number, number, number] {
	const match = /^\^(\d+)\.(\d+)\.(\d+)$/.exec(range);
	if (!match) throw new Error(`unsupported VS Code engine range: ${range}`);
	return [Number(match[1]), Number(match[2]), Number(match[3])];
}

function compareVersions(
	left: [number, number, number],
	right: [number, number, number],
): number {
	for (let i = 0; i < left.length; i++) {
		if (left[i] !== right[i]) return left[i] - right[i];
	}
	return 0;
}

test("declared VS Code floor covers the locked language client requirement", () => {
	const declared = rootPackage.engines.vscode;
	const clientRequirement = packageLock.packages[
		"node_modules/vscode-languageclient"
	].engines?.vscode;
	const lockRootEngine = packageLock.packages[""]?.engines?.vscode;
	if (!clientRequirement) {
		throw new Error("locked vscode-languageclient has no VS Code engine range");
	}
	if (!lockRootEngine) throw new Error("lockfile root has no VS Code engine range");

	expect(declared).toBe("^1.91.0");
	expect(extensionPackage.engines.vscode).toBe(declared);
	expect(lockRootEngine).toBe(declared);
	expect(
		compareVersions(minimumVersion(declared), minimumVersion(clientRequirement)),
	).toBeGreaterThanOrEqual(0);
});
