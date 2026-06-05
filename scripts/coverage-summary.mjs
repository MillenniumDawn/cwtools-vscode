// Render coverage/coverage-summary.json (istanbul json-summary) as a markdown
// table. Appends to the GitHub Actions job summary when $GITHUB_STEP_SUMMARY is
// set, otherwise prints to stdout. Run after `npm run test:coverage`.

import { readFileSync, appendFileSync } from 'fs';
import { relative } from 'path';

const summaryPath = 'coverage/coverage-summary.json';

let data;
try {
	data = JSON.parse(readFileSync(summaryPath, 'utf8'));
} catch (err) {
	console.error(`No coverage summary at ${summaryPath}: ${err.message}`);
	process.exit(0); // don't fail the build over a missing report
}

const pct = (m) => (m && typeof m.pct === 'number' ? m.pct.toFixed(2) : '-');
const row = (name, m) => `| ${name} | ${pct(m.statements)} | ${pct(m.branches)} | ${pct(m.functions)} | ${pct(m.lines)} |`;

const files = Object.keys(data).filter((k) => k !== 'total').sort();

const lines = [
	'## Coverage (extension-host client)',
	'',
	'| File | % Stmts | % Branch | % Funcs | % Lines |',
	'| --- | ---: | ---: | ---: | ---: |',
	row('**All files**', data.total),
	...files.map((f) => row(relative(process.cwd(), f), data[f])),
	'',
];

const md = lines.join('\n');

if (process.env.GITHUB_STEP_SUMMARY) {
	appendFileSync(process.env.GITHUB_STEP_SUMMARY, md + '\n');
}
console.log(md);
