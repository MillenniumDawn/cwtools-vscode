// Render the istanbul json-summary coverage reports as a readable markdown
// table. Writes to coverage/summary.md (for the PR-comment step), appends to
// the GitHub Actions job summary when $GITHUB_STEP_SUMMARY is set, and prints
// to stdout. Run after the coverage test runs.
//
// Reports are stitched together, one section each, for whichever summaries
// exist:
//   - rust engine: cargo-llvm-cov (engine/target/coverage/coverage-summary.json)
//   - extension-host client: vscode-test + c8 (coverage/coverage-summary.json)
//   - node unit: vitest + v8 (coverage-node/coverage-summary.json)

import {
	readFileSync,
	writeFileSync,
	appendFileSync,
	mkdirSync,
	existsSync,
} from "node:fs";
import { relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
	COVERAGE_METRICS,
	HOST_COVERAGE_DROPS,
	HOST_COVERAGE_LABELS,
	HOST_COVERAGE_SCOPES,
	type CoverageMetric,
	type CoverageSummary,
	type FileCoverage,
} from "./coverage";

export interface CoverageSource {
	title: string;
	path: string;
	// Where the full report lands as a CI artifact.
	htmlArtifact: string;
	// Extra path substrings to drop from this report (node_modules is always
	// dropped). vscode-test's c8 run can't reliably exclude specific files, so
	// the modules owned by the node report are filtered out of the host report
	// here instead. Same belt-and-suspenders as node_modules.
	drop?: readonly string[];
	labels?: readonly string[];
	scopes?: readonly string[];
	scopeNote?: string;
	// Worst-covered files to list. Omit to list every source file.
	maxFiles?: number;
}

const sources: CoverageSource[] = [
	{
		title: "rust engine",
		path: "engine/target/coverage/coverage-summary.json",
		htmlArtifact: "rust-coverage",
		maxFiles: 20,
	},
	{
		title: "extension-host client",
		path: "coverage/coverage-summary.json",
		htmlArtifact: "coverage-html",
		drop: HOST_COVERAGE_DROPS,
		labels: HOST_COVERAGE_LABELS,
		scopes: HOST_COVERAGE_SCOPES,
		scopeNote: "Modules measured only by Vitest are excluded.",
	},
	{
		title: "node unit",
		path: "coverage-node/coverage-summary.json",
		htmlArtifact: "coverage-node",
	},
];

// Traffic light per percentage. Tuned low for now; coverage is a work in
// progress (see issue #7), so this informs rather than gates.
const GREEN = 80;
const AMBER = 50;

function light(n: number | undefined): string {
	if (typeof n !== "number") {
		return "⚪";
	}
	if (n >= GREEN) {
		return "🟢";
	}
	if (n >= AMBER) {
		return "🟡";
	}
	return "🔴";
}

const num = (m: CoverageMetric) =>
	m && typeof m.pct === "number" ? m.pct.toFixed(1) : "-";
const cell = (m: CoverageMetric) => `${light(m?.pct)} ${num(m)}`;
const row = (name: string, m: FileCoverage) =>
	`| ${name} | ${cell(m.lines)} | ${cell(m.statements)} | ${cell(m.branches)} | ${cell(m.functions)} |`;

function aggregate(
	files: string[],
	data: CoverageSummary,
	metric: keyof FileCoverage,
): CoverageMetric {
	const covered = files.reduce((n, f) => n + (data[f][metric].covered ?? 0), 0);
	const total = files.reduce((n, f) => n + (data[f][metric].total ?? 0), 0);
	return { covered, total, pct: total ? (100 * covered) / total : undefined };
}

function isSourceFile(
	key: string,
	drop: readonly string[],
	scopes: readonly string[],
): boolean {
	const normalized = key.split("\\").join("/");
	if (key === "total" || normalized.includes("node_modules")) {
		return false;
	}
	if (drop.some((item) => normalized.includes(item))) {
		return false;
	}
	return (
		scopes.length === 0 ||
		scopes.some((scope) => normalized.includes(`/${scope}/`))
	);
}

function listedFiles(
	files: string[],
	maxFiles?: number,
): { shown: string[]; note: string } {
	if (maxFiles === undefined || files.length <= maxFiles) {
		return { shown: files, note: "" };
	}
	return {
		shown: files.slice(0, maxFiles),
		note: ` Showing the ${maxFiles} worst-covered files of ${files.length}.`,
	};
}

function sectionDetails(source: CoverageSource): string | undefined {
	const labels = source.labels?.map((label) => `\`${label}\``).join(", ");
	const scope = source.scopes?.map((path) => `\`${path}\``).join(", ");
	if (!labels || !scope) {
		return undefined;
	}
	return `Measured labels: ${labels}. Source scope: ${scope}. ${source.scopeNote ?? ""}`.trim();
}

export function renderCoverageSection(
	source: CoverageSource,
	data: CoverageSummary,
	cwd = process.cwd(),
): string[] {
	// vscode-test instruments every file the extension loads, including its
	// dependencies, and its coverage `exclude` doesn't reliably drop them. So
	// filter node_modules (and any source-owned `drop` paths) out here and
	// recompute the total from the real client source, otherwise the
	// dependencies dwarf the numbers we care about.
	const drop = source.drop ?? [];
	const scopes = source.scopes ?? [];
	const files = Object.keys(data)
		.filter((key) => isSourceFile(key, drop, scopes))
		.sort((a, b) => (data[a].lines.pct ?? 0) - (data[b].lines.pct ?? 0));
	if (files.length === 0) {
		throw new Error(`${source.title} coverage contains no source files`);
	}

	const total: FileCoverage = {
		lines: aggregate(files, data, "lines"),
		statements: aggregate(files, data, "statements"),
		branches: aggregate(files, data, "branches"),
		functions: aggregate(files, data, "functions"),
	};
	for (const metric of COVERAGE_METRICS) {
		if (total[metric].total === 0) {
			throw new Error(`${source.title} coverage has a zero ${metric} total`);
		}
	}
	const headline = `${light(total.lines.pct)} **${num(total.lines)}% lines** · ${num(total.functions)}% functions · ${num(total.branches)}% branches`;
	const details = sectionDetails(source);
	const { shown, note } = listedFiles(files, source.maxFiles);

	return [
		`## Coverage (${source.title})`,
		"",
		...(details ? [details, ""] : []),
		headline,
		"",
		"| File | % Lines | % Stmts | % Branch | % Funcs |",
		"| --- | --- | --- | --- | --- |",
		row("**All files**", total),
		...shown.map((file) => row(relative(cwd, resolve(cwd, file)), data[file])),
		"",
		`<sub>🟢 ≥80% · 🟡 ≥50% · 🔴 <50%.${note} Full report is in the \`${source.htmlArtifact}\` artifact.</sub>`,
		"",
	];
}

function renderSection(source: CoverageSource): string[] {
	if (!existsSync(source.path)) {
		// Host coverage stays local (Electron never exits under xvfb). Rust and
		// node summaries are present in CI once their jobs upload them.
		return [];
	}
	let data: CoverageSummary;
	try {
		data = JSON.parse(readFileSync(source.path, "utf8")) as CoverageSummary;
	} catch (error) {
		const detail = error instanceof Error ? error.message : String(error);
		throw new Error(`could not read ${source.path}: ${detail}`, {
			cause: error,
		});
	}
	return renderCoverageSection(source, data);
}

export function renderCoverageSummary(
	selectedSources: CoverageSource[] = sources,
): string {
	return selectedSources.flatMap(renderSection).join("\n").trimEnd();
}

function main(): void {
	const selectedSources = process.argv.includes("--host-only")
		? sources.filter((source) => source.labels?.includes("unit"))
		: sources;
	const md = renderCoverageSummary(selectedSources);
	if (!md) {
		throw new Error("no coverage summaries found");
	}
	mkdirSync("coverage", { recursive: true });
	writeFileSync("coverage/summary.md", `${md}\n`);

	if (process.env.GITHUB_STEP_SUMMARY) {
		appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${md}\n`);
	}
	process.stdout.write(`${md}\n`);
}

if (
	process.argv[1] &&
	resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
	main();
}
