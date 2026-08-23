import { spawnSync } from "node:child_process";

const args = process.argv.slice(2);
if (args.length === 0) {
	process.stderr.write(
		"usage: node scripts/build/python.mjs <script> [args...]\n",
	);
	process.exit(2);
}

const candidates =
	process.platform === "win32"
		? [["py", "-3"], ["python"], ["python3"]]
		: [["python3"], ["python"]];

for (const [command, ...prefix] of candidates) {
	const result = spawnSync(command, [...prefix, ...args], { stdio: "inherit" });
	if (result.error?.code === "ENOENT") {
		continue;
	}
	if (result.error) {
		process.stderr.write(`${result.error.message}\n`);
		process.exit(1);
	}
	process.exit(result.status ?? 1);
}

process.stderr.write("Python 3 is not installed or is not on PATH\n");
process.exit(1);
