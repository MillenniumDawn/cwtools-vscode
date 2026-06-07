// Render coverage/coverage-summary.json (istanbul json-summary) as a readable
// markdown report. Writes it to coverage/summary.md (for the PR-comment step),
// appends to the GitHub Actions job summary when $GITHUB_STEP_SUMMARY is set,
// and prints to stdout. Run after `npm run test:coverage`.

import { readFileSync, writeFileSync, appendFileSync, mkdirSync } from 'node:fs';
import { relative } from 'node:path';

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

const summaryPath = 'coverage/coverage-summary.json';

let data: Summary;
try {
	data = JSON.parse(readFileSync(summaryPath, 'utf8')) as Summary;
} catch (err) {
	console.error(`No coverage summary at ${summaryPath}: ${(err as Error).message}`);
	process.exit(0); // don't fail the build over a missing report
}

// Traffic light per percentage. Tuned low for now — coverage is a work in
// progress (see issue #7), so this informs rather than gates.
const GREEN = 80;
const AMBER = 50;
const light = (n: number | undefined) =>
	typeof n !== 'number' ? '⚪' : n >= GREEN ? '🟢' : n >= AMBER ? '🟡' : '🔴';

const num = (m: Metric) => (m && typeof m.pct === 'number' ? m.pct.toFixed(1) : '-');
const cell = (m: Metric) => `${light(m?.pct)} ${num(m)}`;
const row = (name: string, m: FileCoverage) =>
	`| ${name} | ${cell(m.lines)} | ${cell(m.statements)} | ${cell(m.branches)} | ${cell(m.functions)} |`;

// vscode-test instruments every file the extension loads, including its
// dependencies, and its coverage `exclude` doesn't drop them from the report.
// So filter node_modules out here and recompute the total from the real client
// source, otherwise the dependencies dwarf the numbers we care about.
const isSource = (k: string) => k !== 'total' && !k.includes('node_modules');

// Worst-covered files first, so the gaps are what you see.
const files = Object.keys(data)
	.filter(isSource)
	.sort((a, b) => (data[a].lines.pct ?? 0) - (data[b].lines.pct ?? 0));

const metrics = ['lines', 'statements', 'branches', 'functions'] as const;
const t: FileCoverage = Object.fromEntries(
	metrics.map((m) => {
		const covered = files.reduce((n, f) => n + (data[f][m].covered ?? 0), 0);
		const total = files.reduce((n, f) => n + (data[f][m].total ?? 0), 0);
		return [m, { covered, total, pct: total ? (100 * covered) / total : 100 }];
	}),
) as FileCoverage;
const headline = `${light(t.lines.pct)} **${num(t.lines)}% lines** · ${num(t.functions)}% functions · ${num(t.branches)}% branches`;

const lines = [
	'## Coverage (extension-host client)',
	'',
	headline,
	'',
	'| File | % Lines | % Stmts | % Branch | % Funcs |',
	'| --- | --- | --- | --- | --- |',
	row('**All files**', t),
	...files.map((f) => row(relative(process.cwd(), f), data[f])),
	'',
	'<sub>🟢 ≥80% · 🟡 ≥50% · 🔴 <50%. Full HTML report is in the `coverage-html` artifact.</sub>',
];

const md = lines.join('\n');

mkdirSync('coverage', { recursive: true });
writeFileSync('coverage/summary.md', md + '\n');

if (process.env.GITHUB_STEP_SUMMARY) {
	appendFileSync(process.env.GITHUB_STEP_SUMMARY, md + '\n');
}
console.log(md);
