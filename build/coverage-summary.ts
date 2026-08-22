// Render the istanbul json-summary coverage reports as a readable markdown
// table. Writes to coverage/summary.md (for the PR-comment step), appends to
// the GitHub Actions job summary when $GITHUB_STEP_SUMMARY is set, and prints
// to stdout. Run after the coverage test runs.
//
// Two reports are stitched together, one section each:
//   - extension-host client: vscode-test + c8 (coverage/coverage-summary.json)
//   - node unit: vitest + v8       (coverage-node/coverage-summary.json)
// They cover disjoint modules, so each gets its own headline and table.

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
	// Where the browsable HTML report lands as a CI artifact.
	htmlArtifact: string;
	// Extra path substrings to drop from this report (node_modules is always
	// dropped). vscode-test's c8 run can't reliably exclude specific files, so
	// the modules owned by the node report are filtered out of the host report
	// here instead — same belt-and-suspenders as node_modules.
	drop?: readonly string[];
	labels?: readonly string[];
	scopes?: readonly string[];
	scopeNote?: string;
}

const sources: CoverageSource[] = [
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

// Traffic light per percentage. Tuned low for now — coverage is a work in
// progress (see issue #7), so this informs rather than gates.
const GREEN = 80;
const AMBER = 50;
const light = (n: number | undefined) =>
	typeof n !== "number" ? "⚪" : n >= GREEN ? "🟢" : n >= AMBER ? "🟡" : "🔴";

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
	const isSource = (key: string) => {
		const normalized = key.split("\\").join("/");
		return (
			key !== "total" &&
			!normalized.includes("node_modules") &&
			!drop.some((item) => normalized.includes(item)) &&
			(scopes.length === 0 ||
				scopes.some((scope) => normalized.includes(`/${scope}/`)))
		);
	};

	// Worst-covered files first, so the gaps are what you see.
	const files = Object.keys(data)
		.filter(isSource)
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
	const labels = source.labels?.map((label) => `\`${label}\``).join(", ");
	const scope = source.scopes?.map((path) => `\`${path}\``).join(", ");
	const details = labels && scope
		? `Measured labels: ${labels}. Source scope: ${scope}. ${source.scopeNote ?? ""}`.trim()
		: undefined;

	return [
		`## Coverage (${source.title})`,
		"",
		...(details ? [details, ""] : []),
		headline,
		"",
		"| File | % Lines | % Stmts | % Branch | % Funcs |",
		"| --- | --- | --- | --- | --- |",
		row("**All files**", total),
		...files.map((file) => row(relative(cwd, file), data[file])),
		"",
		`<sub>🟢 ≥80% · 🟡 ≥50% · 🔴 <50%. Full HTML report is in the \`${source.htmlArtifact}\` artifact.</sub>`,
		"",
	];
}

function renderSection(source: CoverageSource): string[] {
	if (!existsSync(source.path)) {
		// Host coverage is intentionally absent from CI, which publishes only the
		// node report.
		return [];
	}
	let data: CoverageSummary;
	try {
		data = JSON.parse(readFileSync(source.path, "utf8")) as CoverageSummary;
	} catch (error) {
		const detail = error instanceof Error ? error.message : String(error);
		throw new Error(`could not read ${source.path}: ${detail}`, { cause: error });
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
	mkdirSync("coverage", { recursive: true });
	writeFileSync("coverage/summary.md", `${md}\n`);

	if (process.env.GITHUB_STEP_SUMMARY) {
		appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${md}\n`);
	}
	console.log(md);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	main();
}
