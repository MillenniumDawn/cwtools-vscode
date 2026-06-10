// Build orchestrator for the extension. Replaces the old FAKE/dotnet script.
// Run via `tsx build/build.ts <command>` (see package.json scripts and build.sh).
//
// Commands:
//   quick            Local dev build: Rust server + client into release/, ready to launch.
//   package          Clean, build the client, package a vsix into temp/ (no server build).
//   release-prebuilt Set the version, package the staged binaries into a vsix, draft the
//                    GitHub release, and publish to the Marketplace. Used by CI after the
//                    per-platform Rust binaries are staged under release/bin/server.
//   release          Tag the current CHANGELOG version, push, then run release-prebuilt.
//
// The Rust server (cwtools-rs) builds from a sibling checkout by default; set
// CWTOOLS_RUST_WORKSPACE to build from elsewhere (e.g. the submodule).

import { spawnSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const releaseDir = path.join(repoRoot, 'release');
const tempDir = path.join(repoRoot, 'temp');

const isWindows = process.platform === 'win32';

function run(cmd: string, args: string[], opts: { cwd?: string; env?: NodeJS.ProcessEnv } = {}): void {
	const display = [cmd, ...args].join(' ');
	console.log(`> ${display}`);
	const r = spawnSync(cmd, args, {
		cwd: opts.cwd ?? repoRoot,
		env: opts.env ?? process.env,
		stdio: 'inherit',
		shell: isWindows, // npx/cargo resolve via .cmd shims on Windows
	});
	if (r.status !== 0) {
		throw new Error(`command failed (${r.status ?? r.signal}): ${display}`);
	}
}

// --- Rust server -----------------------------------------------------------

function rustWorkspace(): string {
	const fromEnv = process.env.CWTOOLS_RUST_WORKSPACE;
	if (fromEnv && fromEnv.trim()) return path.resolve(repoRoot, fromEnv);
	return path.resolve(repoRoot, '../cwtools/cwtools-rs');
}

function buildAndDeployRustServer(): void {
	const workspace = rustWorkspace();
	run('cargo', ['build', '--release', '-p', 'cwtools_lsp'], { cwd: workspace });

	const binName = isWindows ? 'cwtools-server.exe' : 'cwtools-server';
	const built = path.join(workspace, 'target/release', binName);
	if (!fs.existsSync(built)) {
		throw new Error(
			`Rust server binary not found at '${built}' after build. Check the crate name/target, ` +
			`or point CWTOOLS_RUST_WORKSPACE at the right cwtools-rs checkout (currently '${workspace}').`,
		);
	}

	// Deploy to the path the client loads first. Clean it so stale binaries
	// don't linger next to the fresh one.
	const outDir = path.join(releaseDir, 'bin/server/cwtools-server');
	fs.rmSync(outDir, { recursive: true, force: true });
	fs.mkdirSync(outDir, { recursive: true });
	const dest = path.join(outDir, binName);
	fs.copyFileSync(built, dest);
	if (!isWindows) fs.chmodSync(dest, 0o755);
}

// --- Client + docs ---------------------------------------------------------

function buildClient(): void {
	run('npm', ['run', 'compile']);
}

function copyDocs(): void {
	for (const f of ['README.md', 'LICENSE.md']) {
		fs.copyFileSync(path.join(repoRoot, f), path.join(releaseDir, f));
	}
	fs.copyFileSync(path.join(repoRoot, 'CHANGELOG.md'), path.join(releaseDir, 'CHANGELOG.md'));
}

function copyDir(src: string, dest: string): void {
	fs.mkdirSync(dest, { recursive: true });
	for (const entry of fs.readdirSync(src, { withFileTypes: true })) {
		const s = path.join(src, entry.name);
		const d = path.join(dest, entry.name);
		if (entry.isDirectory()) copyDir(s, d);
		else fs.copyFileSync(s, d);
	}
}

function copyWebviewCss(): void {
	const dest = path.join(releaseDir, 'bin/client/webview');
	fs.mkdirSync(dest, { recursive: true });
	const webviewSrc = path.join(repoRoot, 'client/webview');
	for (const f of fs.readdirSync(webviewSrc)) {
		if (f.endsWith('.css')) fs.copyFileSync(path.join(webviewSrc, f), path.join(dest, f));
	}
}

function copyTestSamples(): void {
	copyDir(
		path.join(repoRoot, 'client/test/sample'),
		path.join(releaseDir, 'bin/client/test/sample'),
	);
}

function cleanReleaseBin(): void {
	fs.rmSync(path.join(releaseDir, 'bin'), { recursive: true, force: true });
}

function assembleClient(): void {
	buildClient();
	copyDocs();
	copyWebviewCss();
	copyTestSamples();
}

// --- Packaging -------------------------------------------------------------

function packageVsix(): void {
	// The client is bundled with esbuild, so node_modules is excluded from the
	// vsix (see release/.vscodeignore). --no-dependencies stops vsce from trying
	// to resolve/include them.
	run('npx', ['--yes', '@vscode/vsce', 'package', '--no-dependencies'], { cwd: releaseDir });
	fs.mkdirSync(tempDir, { recursive: true });
	for (const f of fs.readdirSync(releaseDir)) {
		if (f.endsWith('.vsix')) {
			fs.renameSync(path.join(releaseDir, f), path.join(tempDir, f));
		}
	}
}

function findVsix(): string {
	const files = fs.existsSync(tempDir) ? fs.readdirSync(tempDir).filter(f => f.endsWith('.vsix')) : [];
	if (files.length === 0) throw new Error('no .vsix found in temp/');
	return path.join(tempDir, files[0]);
}

// --- Release ---------------------------------------------------------------

// On a tag push CI sets TAG_RELEASE=true and the version comes from the tag.
// Manual/local runs fall back to the top CHANGELOG.md entry.
function resolveVersion(): { version: string; tag: string; preRelease: boolean } {
	const isTagRelease = /^(1|true)$/i.test(process.env.TAG_RELEASE ?? '');
	let tag = isTagRelease ? (process.env.GITHUB_REF_NAME ?? '').trim() : '';
	if (!tag) tag = topChangelogVersion();
	const version = tag.replace(/^v/, '');
	return { version, tag, preRelease: version.includes('-') };
}

// First "## [x.y.z]" (or "## x.y.z") heading in CHANGELOG.md.
function topChangelogVersion(): string {
	const changelog = fs.readFileSync(path.join(repoRoot, 'CHANGELOG.md'), 'utf8');
	const m = changelog.match(/^#+\s*\[?v?(\d+\.\d+\.\d+[^\]\s]*)\]?/m);
	if (!m) throw new Error('could not find a version heading in CHANGELOG.md');
	return m[1];
}

// The CHANGELOG section body for `version`, used as the GitHub release notes.
function changelogNotes(version: string): string {
	const changelog = fs.readFileSync(path.join(repoRoot, 'CHANGELOG.md'), 'utf8');
	const lines = changelog.split('\n');
	const headingRe = /^#+\s*\[?v?(\d+\.\d+\.\d+[^\]\s]*)\]?/;
	let start = -1;
	for (let i = 0; i < lines.length; i++) {
		const m = lines[i].match(headingRe);
		if (m && m[1] === version) { start = i + 1; break; }
	}
	if (start === -1) return '';
	const body: string[] = [];
	for (let i = start; i < lines.length; i++) {
		if (headingRe.test(lines[i])) break;
		body.push(lines[i]);
	}
	return body.join('\n').trim();
}

function setReleaseVersion(version: string): void {
	const manifestPath = path.join(releaseDir, 'package.json');
	const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
	manifest.version = version;
	fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + '\n');
	console.log(`set release/package.json version to ${version}`);
}

// Draft and publish the GitHub release with the vsix attached, via the gh CLI.
function publishGithubRelease(tag: string, version: string, preRelease: boolean, vsix: string): void {
	const notes = changelogNotes(version);
	const notesArgs = notes
		? (() => {
			const f = path.join(tempDir, 'release-notes.md');
			fs.writeFileSync(f, notes);
			return ['--notes-file', f];
		})()
		: ['--generate-notes'];
	const args = ['release', 'create', tag, vsix, '--title', tag, ...notesArgs];
	if (preRelease) args.push('--prerelease');
	run('gh', args);
}

function publishToMarketplace(vsix: string): void {
	const token = process.env['vsce-token'];
	if (!token || !token.trim()) {
		if (process.env.CI) {
			console.log('No vsce-token set; skipping VS Code Marketplace publish.');
			return;
		}
		throw new Error('vsce-token is not set; cannot publish to the Marketplace.');
	}
	run('npx', ['--yes', '@vscode/vsce', 'publish', '--pat', token, '--packagePath', vsix]);
}

// --- Commands --------------------------------------------------------------

function cmdQuick(): void {
	cleanReleaseBin();
	buildAndDeployRustServer();
	assembleClient();
}

function cmdPackage(): void {
	cleanReleaseBin();
	assembleClient();
	packageVsix();
}

function cmdReleasePrebuilt(): void {
	const { version, tag, preRelease } = resolveVersion();
	setReleaseVersion(version);
	// Build the client bundles (extension.js, webview/graph.js) into release/bin
	// so vsce finds the entrypoint. No cleanReleaseBin here: the per-platform
	// server binaries are already staged under release/bin/server and assembling
	// the client doesn't touch them.
	assembleClient();
	packageVsix();
	const vsix = findVsix();
	publishGithubRelease(tag, version, preRelease, vsix);
	publishToMarketplace(vsix);
}

function cmdRelease(): void {
	const { tag } = resolveVersion();
	run('git', ['tag', tag]);
	run('git', ['push', 'origin', tag]);
	cmdReleasePrebuilt();
}

const commands: Record<string, () => void> = {
	quick: cmdQuick,
	package: cmdPackage,
	'release-prebuilt': cmdReleasePrebuilt,
	release: cmdRelease,
};

// Aliases for the old FAKE target names, so existing muscle memory and any
// stale invocations keep working.
const aliases: Record<string, string> = {
	QuickBuild: 'quick',
	QuickBuildDebug: 'quick',
	DryRelease: 'package',
	BuildPackage: 'package',
	ReleasePrebuilt: 'release-prebuilt',
	Release: 'release',
};

const requested = process.argv[2] ?? 'quick';
const cmd = aliases[requested] ?? requested;
const handler = commands[cmd];
if (!handler) {
	console.error(`unknown command '${cmd}'. Known: ${Object.keys(commands).join(', ')}`);
	process.exit(1);
}
handler();
