import * as os from 'os';
import * as path from 'path';
import { spawn } from 'child_process';
import type { ExtensionContext } from 'vscode';
import { existsSync as fsExistsSync } from 'fs';
import { logInfo, logError } from './logger';

export const LANGUAGE_REPOS: Record<string, string> = {
	stellaris: 'https://github.com/cwtools/cwtools-stellaris-config',
	eu4: 'https://github.com/cwtools/cwtools-eu4-config',
	hoi4: 'https://github.com/cwtools/cwtools-hoi4-config',
	ck2: 'https://github.com/cwtools/cwtools-ck2-config',
	imperator: 'https://github.com/cwtools/cwtools-ir-config',
	vic2: 'https://github.com/cwtools/cwtools-vic2-config',
	vic3: 'https://github.com/cwtools/cwtools-vic3-config',
	ck3: 'https://github.com/cwtools/cwtools-ck3-config',
	eu5: 'https://github.com/kaiser-chris/cwtools-eu5-config',
};

export const GAME_DISPLAY: Record<string, string> = {
	stellaris: 'Stellaris',
	hoi4: 'Hearts of Iron IV',
	eu4: 'Europa Universalis IV',
	ck2: 'Crusader Kings II',
	imperator: 'Imperator',
	vic2: 'Victoria II',
	vic3: 'Victoria 3',
	ck3: 'Crusader Kings III',
	eu5: 'Europa Universalis V',
};

export const GAME_FOLDER: Record<string, { id: string; subdir?: string }> = {
	'stellaris':              { id: 'stellaris' },
	'hearts of iron iv':      { id: 'hoi4' },
	'europa universalis iv':  { id: 'eu4' },
	'crusader kings ii':      { id: 'ck2' },
	'crusader kings iii':     { id: 'ck3',    subdir: 'game' },
	'victoria ii':            { id: 'vic2' },
	'victoria 2':             { id: 'vic2' },
	'victoria 3':             { id: 'vic3',   subdir: 'game' },
	'imperatorrome':          { id: 'imperator', subdir: 'game' },
	'imperator':              { id: 'imperator', subdir: 'game' },
	'europa universalis v':   { id: 'eu5',    subdir: 'game' },
};

export const FOLDER_HINTS: Array<[RegExp | string, string]> = [
	[/stellaris/, 'stellaris'],
	[/(hoi4|hearts)/, 'hoi4'],
	[/(eu4|europa)/, 'eu4'],
	[/(ck2|crusader kings ii)/, 'ck2'],
	[/(ck3|crusader kings iii)/, 'ck3'],
	[/(vic2|victoria (ii|2))/, 'vic2'],
	[/(vic3|victoria (iii|3))/, 'vic3'],
	[/(imperator|rome)/, 'imperator'],
	['eu5', 'eu5'],
];

export const CONTENT_HINTS: Array<[string, string]> = [
	['common/ai_strategy', 'hoi4'],
	['common/species_classes', 'stellaris'],
	['common/great_projects', 'eu4'],
	['common/dynasties', 'ck3'],
];

export function detectFromFolder(root: string, fileExists: (p: string) => boolean): string | null {
	const lower = root.toLowerCase();
	for (const [pattern, id] of FOLDER_HINTS) {
		if (typeof pattern === 'string' ? lower.includes(pattern) : pattern.test(lower)) {
			return id;
		}
	}
	for (const [sub, id] of CONTENT_HINTS) {
		if (fileExists(path.join(root, sub))) return id;
	}
	return null;
}

function serverPlatformDir(): string {
	switch (os.platform()) {
		case 'win32': return 'win-x64';
		case 'darwin': return os.arch() === 'arm64' ? 'osx-arm64' : 'osx-x64';
		default: return 'linux-x64';
	}
}

export function serverExe(
	context: ExtensionContext,
	exists: (p: string) => boolean = fsExistsSync
): string | undefined {
	const isWin = os.platform() === 'win32';
	const platform = serverPlatformDir();
	const exe = isWin ? 'cwtools-server.exe' : 'cwtools-server';
	// Dev and single-platform builds drop the binary straight in
	// cwtools-server/; the packaged multi-platform vsix nests one binary per
	// platform subdir. Check the flat layout first, then the per-platform one.
	const candidates = [
		path.join('bin', 'server', 'cwtools-server', exe),
		path.join('bin', 'server', 'cwtools-server', platform, exe),
	];
	for (const rel of candidates) {
		const full = context.asAbsolutePath(rel);
		if (exists(full)) return full;
	}
	return undefined;
}

export function runGit(
	args: string[],
	spawnFn: typeof spawn = spawn,
	timeoutMs = 60000
): Promise<void> {
	return new Promise((resolve, reject) => {
		const git = spawnFn('git', args, { stdio: ['ignore', 'pipe', 'pipe'] });
		let out = '';
		let err = '';
		let settled = false;
		// Don't let a hung git (auth prompt, dead network) block activation forever.
		const timer = setTimeout(() => {
			if (settled) return;
			settled = true;
			git.kill();
			reject(new Error(`git ${args.join(' ')} timed out after ${timeoutMs}ms`));
		}, timeoutMs);
		timer.unref?.();
		git.stdout?.on('data', d => { out += d.toString(); });
		git.stderr?.on('data', d => { err += d.toString(); });
		git.on('error', e => {
			if (settled) return;
			settled = true;
			clearTimeout(timer);
			logError(`git ${args.join(' ')} error`, e);
			reject(e);
		});
		git.on('close', (code, signal) => {
			if (settled) return;
			settled = true;
			clearTimeout(timer);
			if (out) logInfo(`git stdout: ${out.trimEnd()}`);
			if (err) logError(`git stderr: ${err.trimEnd()}`);
			if (code === 0 && !signal) resolve();
			else reject(new Error(`git exited with code ${code} (signal: ${signal || 'none'})`));
		});
	});
}
