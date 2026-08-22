import { readFileSync, rmSync } from "node:fs";
import * as path from "node:path";
import { spawn, spawnSync, execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { repoRoot } from "./paths";
import { validateHostCoverageSummary } from "./coverage";

const coverageDir = path.join(repoRoot, "coverage");
const summaryPath = path.join(coverageDir, "coverage-summary.json");

export const HOST_COVERAGE_TIMEOUT_MS = 5 * 60 * 1000;
export const HOST_COVERAGE_KILL_GRACE_MS = 5_000;

export interface RunWithTimeoutOptions {
	cwd: string;
	timeoutMs: number;
	graceMs: number;
	stdio?: "inherit" | "ignore";
}

function cleanCoverage(): void {
	rmSync(coverageDir, { recursive: true, force: true });
}

function parsePids(text: string): number[] {
	const pids: number[] = [];
	for (const part of text.trim().split(/\s+/)) {
		const value = Number(part);
		if (Number.isInteger(value) && value > 0) {
			pids.push(value);
		}
	}
	return pids;
}

function childPids(pid: number): number[] {
	try {
		return parsePids(readFileSync(`/proc/${pid}/task/${pid}/children`, "utf8"));
	} catch {
		// non-Linux or already gone
	}
	try {
		return parsePids(
			execFileSync("pgrep", ["-P", String(pid)], {
				encoding: "utf8",
				stdio: ["ignore", "pipe", "ignore"],
			}),
		);
	} catch {
		return [];
	}
}

export function killProcessTree(pid: number, signal: NodeJS.Signals): void {
	if (process.platform === "win32") {
		const args = ["/PID", String(pid), "/T"];
		if (signal === "SIGKILL") {
			args.push("/F");
		}
		spawnSync("taskkill", args, { stdio: "ignore" });
		return;
	}
	for (const child of childPids(pid)) {
		killProcessTree(child, signal);
	}
	try {
		process.kill(pid, signal);
	} catch {
		// already gone
	}
}

export function runWithTimeout(
	name: string,
	command: string,
	args: string[],
	options: RunWithTimeoutOptions,
): Promise<void> {
	const { cwd, timeoutMs, graceMs, stdio = "inherit" } = options;
	return new Promise((resolve, reject) => {
		const child = spawn(command, args, { cwd, stdio });
		if (child.pid === undefined) {
			reject(new Error(`${name} failed to start`));
			return;
		}
		const pid = child.pid;
		let timedOut = false;
		let killTimer: ReturnType<typeof setTimeout> | undefined;
		const timeoutTimer = setTimeout(() => {
			timedOut = true;
			killProcessTree(pid, "SIGTERM");
			killTimer = setTimeout(() => {
				killProcessTree(pid, "SIGKILL");
			}, graceMs);
		}, timeoutMs);

		child.on("error", (error) => {
			clearTimeout(timeoutTimer);
			if (killTimer !== undefined) {
				clearTimeout(killTimer);
			}
			reject(error);
		});
		child.on("exit", (code, signal) => {
			clearTimeout(timeoutTimer);
			if (killTimer !== undefined) {
				clearTimeout(killTimer);
			}
			if (code === 0 && signal === null) {
				resolve();
				return;
			}
			if (timedOut) {
				reject(new Error(`${name} timed out after ${timeoutMs}ms`));
				return;
			}
			const detail = signal
				? `signal ${signal}`
				: `exit code ${code ?? "unknown"}`;
			reject(new Error(`${name} failed with ${detail}`));
		});
	});
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

async function main(): Promise<void> {
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
		await runWithTimeout(
			"extension-host coverage",
			process.execPath,
			[vscodeTestCli, "--label", "unit", "--coverage"],
			{
				cwd: repoRoot,
				timeoutMs: HOST_COVERAGE_TIMEOUT_MS,
				graceMs: HOST_COVERAGE_KILL_GRACE_MS,
			},
		);

		validateHostCoverageSummary(
			JSON.parse(readFileSync(summaryPath, "utf8")) as unknown,
		);
	} catch (error) {
		cleanCoverage();
		throw error;
	}
}

if (
	process.argv[1] &&
	path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
	main().catch((error) => {
		const detail = error instanceof Error ? error.message : String(error);
		process.stderr.write(`${detail}\n`);
		process.exit(1);
	});
}
