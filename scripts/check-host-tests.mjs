import { readdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import testConfig from "../.vscode-test.mjs";

const repositoryRoot = fileURLToPath(new URL("..", import.meta.url));
const hostTestDirectory = path.join(repositoryRoot, "extension/test/host");
const sourceTests = (await readdir(hostTestDirectory, { withFileTypes: true }))
	.filter((entry) => entry.isFile() && entry.name.endsWith(".test.ts"))
	.map((entry) => entry.name)
	.sort();

const configuredTestsByLabel = new Map();
for (const { label, files = [] } of testConfig.tests) {
	for (const configuredFile of files) {
		const match = configuredFile.match(
			/^\.\/dist\/extension\/bin\/client\/test\/host\/(.+\.test)\.js$/,
		);
		if (!match) continue;

		const sourceTest = `${match[1]}.ts`;
		const labels = configuredTestsByLabel.get(sourceTest) ?? [];
		labels.push(label);
		configuredTestsByLabel.set(sourceTest, labels);
	}
}

const sourceTestSet = new Set(sourceTests);
const unlistedTests = sourceTests.filter(
	(sourceTest) => !configuredTestsByLabel.has(sourceTest),
);
const staleEntries = [...configuredTestsByLabel.keys()].filter(
	(configuredTest) => !sourceTestSet.has(configuredTest),
);

if (unlistedTests.length > 0 || staleEntries.length > 0) {
	const problems = [];
	if (unlistedTests.length > 0) {
		problems.push(
			`Host test files missing from .vscode-test.mjs labels: ${unlistedTests.join(", ")}`,
		);
	}
	if (staleEntries.length > 0) {
		problems.push(
			`Configured host test files missing from extension/test/host: ${staleEntries.join(", ")}`,
		);
	}
	console.error(problems.join("\n"));
	process.exitCode = 1;
} else {
	console.log(
		`All ${sourceTests.length} host test files appear in at least one .vscode-test.mjs label.`,
	);
}
