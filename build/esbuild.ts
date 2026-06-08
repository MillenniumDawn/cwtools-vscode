// Bundles the two client artifacts that ship in the vsix:
//   - the extension host (Node CommonJS, vscode kept external)
//   - the webview graph (browser IIFE, exposed as `cwtoolsgraph`)
// Both write into release/bin/client, overwriting the per-file tsc output for
// their entry points. Run via `tsx build/esbuild.ts` (see package.json).
//
// Flags: --watch (rebuild on change), --dev (NODE_ENV=development for the webview).

import { build, context, type BuildOptions } from 'esbuild';

const watch = process.argv.includes('--watch');
const dev = process.argv.includes('--dev') || watch;

const shared: BuildOptions = {
	bundle: true,
	sourcemap: true,
	// Ship unminified. Both bundles load locally inside VS Code, so minifying
	// buys nothing, and minified Cytoscape/ELK trips heuristic "FakeUpdate"
	// (SocGholish) AV signatures as a false positive. Readable output sidesteps it.
	minify: false,
	logLevel: 'info',
};

const extension: BuildOptions = {
	...shared,
	entryPoints: ['client/extension/extension.ts'],
	outfile: 'release/bin/client/extension/extension.js',
	platform: 'node',
	format: 'cjs',
	target: 'node18',
	// Provided by the VS Code runtime, never bundle it.
	external: ['vscode'],
};

const webview: BuildOptions = {
	...shared,
	entryPoints: ['client/webview/graph.ts'],
	outfile: 'release/bin/client/webview/graph.js',
	platform: 'browser',
	format: 'iife',
	globalName: 'cwtoolsgraph',
	// Cytoscape and friends sniff process.env; the old rollup build shimmed it too.
	define: { 'process.env.NODE_ENV': JSON.stringify(dev ? 'development' : 'production') },
	banner: { js: 'window.process = { env: { NODE_ENV: "production" } };' },
};

async function run() {
	if (watch) {
		const ctxs = await Promise.all([context(extension), context(webview)]);
		await Promise.all(ctxs.map((c) => c.watch()));
		console.log('[esbuild] watching extension + webview...');
		return;
	}
	await Promise.all([build(extension), build(webview)]);
}

run().catch((err) => {
	console.error(err);
	process.exit(1);
});
