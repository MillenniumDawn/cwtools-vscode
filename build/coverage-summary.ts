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
} from "node:fs";
import { relative } from "node:path";

interface Metric {
	pct?: number;
	covered?: number;
	total?: number;
}
interface FileCoverage {
	statements: Metric;
	branches: Metric;
	functions: Metric;
	lines: Metric;
}
type Summary = { total: FileCoverage } & Record<string, FileCoverage>;

interface Source {
	title: string;
	path: string;
	// Where the browsable HTML report lands as a CI artifact.
	htmlArtifact: string;
	// Extra path substrings to drop from this report (node_modules is always
	// dropped). vscode-test's c8 run can't reliably exclude specific files, so
	// the modules owned by the node report are filtered out of the host report
	// here instead — same belt-and-suspenders as node_modules.
	drop?: string[];
}

const sources: Source[] = [
	{
		title: "extension-host client",
		path: "coverage/coverage-summary.json",
		htmlArtifact: "coverage-html",
		drop: [
			"extension/src/host/engine.ts",
			"extension/src/host/executable.ts",
			"extension/src/host/games.ts",
			"extension/src/host/rulesManifest.ts",
			"extension/src/host/rulesSetup.ts",
		],
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

const num = (m: Metric) =>
	m && typeof m.pct === "number" ? m.pct.toFixed(1) : "-";
const cell = (m: Metric) => `${light(m?.pct)} ${num(m)}`;
const row = (name: string, m: FileCoverage) =>
	`| ${name} | ${cell(m.lines)} | ${cell(m.statements)} | ${cell(m.branches)} | ${cell(m.functions)} |`;

// Sum one metric across every source file. An empty set reports 100%.
function aggregate(
	files: string[],
	data: Summary,
	metric: keyof FileCoverage,
): Metric {
	const covered = files.reduce((n, f) => n + (data[f][metric].covered ?? 0), 0);
	const total = files.reduce((n, f) => n + (data[f][metric].total ?? 0), 0);
	return { covered, total, pct: total ? (100 * covered) / total : 100 };
}

function renderSection(source: Source): string[] {
	let data: Summary;
	try {
		data = JSON.parse(readFileSync(source.path, "utf8")) as Summary;
	} catch {
		// Report absent (e.g. host coverage is not collected in CI). Omit the
		// section rather than print a placeholder.
		return [];
	}

	// vscode-test instruments every file the extension loads, including its
	// dependencies, and its coverage `exclude` doesn't reliably drop them. So
	// filter node_modules (and any source-owned `drop` paths) out here and
	// recompute the total from the real client source, otherwise the
	// dependencies dwarf the numbers we care about.
	const drop = source.drop ?? [];
	const isSource = (k: string) =>
		k !== "total" &&
		!k.includes("node_modules") &&
		!drop.some((d) => k.includes(d));

	// Worst-covered files first, so the gaps are what you see.
	const files = Object.keys(data)
		.filter(isSource)
		.sort((a, b) => (data[a].lines.pct ?? 0) - (data[b].lines.pct ?? 0));

	const t: FileCoverage = {
		lines: aggregate(files, data, "lines"),
		statements: aggregate(files, data, "statements"),
		branches: aggregate(files, data, "branches"),
		functions: aggregate(files, data, "functions"),
	};
	const headline = `${light(t.lines.pct)} **${num(t.lines)}% lines** · ${num(t.functions)}% functions · ${num(t.branches)}% branches`;

	return [
		`## Coverage (${source.title})`,
		"",
		headline,
		"",
		"| File | % Lines | % Stmts | % Branch | % Funcs |",
		"| --- | --- | --- | --- | --- |",
		row("**All files**", t),
		...files.map((f) => row(relative(process.cwd(), f), data[f])),
		"",
		`<sub>🟢 ≥80% · 🟡 ≥50% · 🔴 <50%. Full HTML report is in the \`${source.htmlArtifact}\` artifact.</sub>`,
		"",
	];
}

const md = sources.flatMap(renderSection).join("\n").trimEnd();

mkdirSync("coverage", { recursive: true });
writeFileSync("coverage/summary.md", md + "\n");

if (process.env.GITHUB_STEP_SUMMARY) {
	appendFileSync(process.env.GITHUB_STEP_SUMMARY, md + "\n");
}
console.log(md);
