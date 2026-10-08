import { readdir } from "node:fs/promises";
import { realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

/**
 * Convert a vscode-test config's emitted JavaScript paths to source test
 * basenames.
 * @param {{ files: string[] }[]} testConfigs Configs returned by vscode-test.
 * @returns {Set<string>} Configured host test basenames, including `.test.ts`.
 */
function configuredHostTests(testConfigs) {
	return new Set(
		testConfigs
			.flatMap((config) => config.files)
			.flatMap((file) => {
				const match = file.match(
					/^\.\/dist\/extension\/bin\/client\/test\/host\/(.+\.test)\.js$/,
				);
				return match ? [`${match[1]}.ts`] : [];
			}),
	);
}

/**
 * Return host test source names that are not referenced by a vscode-test
 * config's file lists. Adding a source test without a label is a check failure.
 * @param {string[]} sourceNames Host test basenames, including `.test.ts`.
 * @param {{ files: string[] }[]} testConfigs Configs returned by vscode-test.
 * @returns {string[]} Unassigned host test basenames.
 */
export function findUnassignedHostTests(sourceNames, testConfigs) {
	const assigned = configuredHostTests(testConfigs);
	return sourceNames.filter((name) => !assigned.has(name)).sort();
}

/**
 * Return configured host test names that no longer have a source file.
 * @param {string[]} sourceNames Host test basenames, including `.test.ts`.
 * @param {{ files: string[] }[]} testConfigs Configs returned by vscode-test.
 * @returns {string[]}
 */
export function findStaleHostTests(sourceNames, testConfigs) {
	const sources = new Set(sourceNames);
	return [...configuredHostTests(testConfigs)]
		.filter((name) => !sources.has(name))
		.sort();
}

/**
 * Read the repository's host suite inventory and fail if a test has no label,
 * or if a `.vscode-test.mjs` entry has no source file in `extension/test/host`.
 * @param {string} root Repository root (injectable for fixture tests).
 */
export async function checkHostTestDiscovery(root = repoRoot) {
	const hostDirectory = path.join(root, "extension/test/host");
	const sourceNames = (await readdir(hostDirectory)).filter((name) =>
		name.endsWith(".test.ts"),
	);
	const configUrl = pathToFileURL(path.join(root, ".vscode-test.mjs"));
	const { default: config } = await import(configUrl.href);
	const unassigned = findUnassignedHostTests(sourceNames, config.tests);
	const stale = findStaleHostTests(sourceNames, config.tests);
	const problems = [];
	if (unassigned.length > 0) {
		problems.push(
			`Host test files missing from .vscode-test.mjs labels: ${unassigned.map((name) => `extension/test/host/${name}`).join(", ")}`,
		);
	}
	if (stale.length > 0) {
		problems.push(
			`Configured host test files missing from extension/test/host: ${stale.map((name) => `extension/test/host/${name}`).join(", ")}`,
		);
	}
	if (problems.length > 0) {
		throw new Error(problems.join("\n"));
	}
}

if (
	process.argv[1] &&
	realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))
) {
	try {
		await checkHostTestDiscovery();
	} catch (error) {
		console.error(error instanceof Error ? error.message : String(error));
		process.exitCode = 1;
	}
}
