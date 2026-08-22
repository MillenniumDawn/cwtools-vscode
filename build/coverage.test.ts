import * as assert from "node:assert";
import { suite, test } from "vitest";
import { validateHostCoverageSummary } from "./coverage";
import { renderCoverageSection } from "./coverage-summary";

const metric = (total: number) => ({ total, covered: total, pct: 100 });
const fileCoverage = (totals = { lines: 1, statements: 1, branches: 1, functions: 1 }) => ({
	lines: metric(totals.lines),
	statements: metric(totals.statements),
	branches: metric(totals.branches),
	functions: metric(totals.functions),
});

suite("validateHostCoverageSummary", () => {
	test("accepts non-empty host and common source coverage", () => {
		assert.doesNotThrow(() =>
			validateHostCoverageSummary({
				total: fileCoverage(),
				"/repo/extension/src/host/fileExplorer.ts": fileCoverage(),
				"C:\\repo\\extension\\src\\common\\graphTypes.ts": fileCoverage(),
			}),
		);
	});

	test("rejects a report containing only dependencies", () => {
		assert.throws(
			() =>
				validateHostCoverageSummary({
					total: fileCoverage(),
					"/repo/node_modules/example/index.js": fileCoverage(),
				}),
			/no files under extension\/src\/host or extension\/src\/common/,
		);
	});

	for (const metricName of [
		"lines",
		"statements",
		"branches",
		"functions",
	] as const) {
		test(`rejects a zero ${metricName} total`, () => {
			assert.throws(
				() =>
					validateHostCoverageSummary({
						total: fileCoverage(),
						"/repo/extension/src/host/fileExplorer.ts": fileCoverage({
							lines: 1,
							statements: 1,
							branches: 1,
							functions: 1,
							[metricName]: 0,
						}),
					}),
				(error: unknown) =>
					error instanceof Error &&
					error.message === `host coverage has a zero ${metricName} total`,
			);
		});
	}
});

suite("renderCoverageSection", () => {
	const source = {
		title: "extension-host client",
		path: "coverage/coverage-summary.json",
		htmlArtifact: "coverage-html",
		labels: ["unit"],
		scopes: ["extension/src/host", "extension/src/common"],
		scopeNote: "Modules measured only by Vitest are excluded.",
	};

	test("renders the measured label and source scope", () => {
		const rendered = renderCoverageSection(
			source,
			{
				total: fileCoverage(),
				"/repo/extension/src/host/fileExplorer.ts": fileCoverage(),
				"/repo/node_modules/example/index.js": fileCoverage(),
			},
			"/repo",
		).join("\n");

		assert.match(rendered, /Measured labels: `unit`/);
		assert.match(
			rendered,
			/Source scope: `extension\/src\/host`, `extension\/src\/common`/,
		);
		assert.match(rendered, /extension\/src\/host\/fileExplorer\.ts/);
		assert.doesNotMatch(rendered, /node_modules/);
	});

	test("rejects an empty filtered report", () => {
		assert.throws(
			() =>
				renderCoverageSection(source, {
					total: fileCoverage(),
					"/repo/node_modules/example/index.js": fileCoverage(),
				}),
			/no source files/,
		);
	});
});
