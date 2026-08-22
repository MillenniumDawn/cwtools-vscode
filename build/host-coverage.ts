import { readFileSync, rmSync } from "node:fs";
import * as path from "node:path";
import { spawnSync } from "node:child_process";
import { repoRoot } from "./paths";
import { validateHostCoverageSummary } from "./coverage";

const coverageDir = path.join(repoRoot, "coverage");
const summaryPath = path.join(coverageDir, "coverage-summary.json");

function cleanCoverage(): void {
	rmSync(coverageDir, { recursive: true, force: true });
}

function run(name: string, command: string, args: string[]): void {
	const result = spawnSync(command, args, {
		cwd: repoRoot,
		stdio: "inherit",
	});
	if (result.error) {
		throw result.error;
	}
	if (result.status !== 0) {
		const detail = result.signal
			? `signal ${result.signal}`
			: `exit code ${result.status ?? "unknown"}`;
		throw new Error(`${name} failed with ${detail}`);
	}
}

function main(): void {
	cleanCoverage();
	try {
		const npmExecPath = process.env.npm_execpath;
		if (!npmExecPath) {
			throw new Error("host coverage must run through npm run test:coverage");
		}
		run("extension compilation", process.execPath, [
			npmExecPath,
			"run",
			"compile",
		]);

		const vscodeTestCli = path.join(
			repoRoot,
			"node_modules",
			"@vscode",
			"test-cli",
			"out",
			"bin.mjs",
		);
		run("extension-host coverage", process.execPath, [
			vscodeTestCli,
			"--label",
			"unit",
			"--coverage",
		]);

		validateHostCoverageSummary(
			JSON.parse(readFileSync(summaryPath, "utf8")) as unknown,
		);
	} catch (error) {
		cleanCoverage();
		throw error;
	}
}

main();
