import { suite, test } from 'vitest';
import * as assert from 'assert';
import * as path from 'path';
import { EventEmitter } from 'events';
import type { ExtensionContext } from 'vscode';
import {
	LANGUAGE_REPOS,
	GAME_DISPLAY,
	GAME_FOLDER,
	detectFromFolder,
	resolveRulesFolder,
	serverExe,
	runGit,
} from '../../extension/engine';

suite('engine — LANGUAGE_REPOS', () => {
	test('covers every supported game with a github URL', () => {
		const expected = ['stellaris', 'eu4', 'hoi4', 'ck2', 'imperator', 'vic2', 'vic3', 'ck3', 'eu5'];
		for (const id of expected) {
			assert.ok(LANGUAGE_REPOS[id], `missing repo URL for ${id}`);
			assert.match(LANGUAGE_REPOS[id], /^https:\/\/github\.com\//);
		}
		assert.strictEqual(Object.keys(LANGUAGE_REPOS).length, expected.length);
	});
});

suite('engine — GAME_DISPLAY', () => {
	test('has a human-readable name for every supported language id', () => {
		for (const id of Object.keys(LANGUAGE_REPOS)) {
			assert.ok(GAME_DISPLAY[id], `missing display name for ${id}`);
			assert.ok(GAME_DISPLAY[id].length > 0);
		}
	});
});

suite('engine — GAME_FOLDER', () => {
	test('maps vanilla Steam folder names to language ids', () => {
		assert.deepStrictEqual(GAME_FOLDER['stellaris'], { id: 'stellaris' });
		assert.deepStrictEqual(GAME_FOLDER['hearts of iron iv'], { id: 'hoi4' });
		assert.deepStrictEqual(GAME_FOLDER['victoria ii'], { id: 'vic2' });
		assert.deepStrictEqual(GAME_FOLDER['victoria 2'], { id: 'vic2' });
	});

	test('flags games whose vanilla install needs a /game subdir', () => {
		assert.strictEqual(GAME_FOLDER['crusader kings iii'].subdir, 'game');
		assert.strictEqual(GAME_FOLDER['victoria 3'].subdir, 'game');
		assert.strictEqual(GAME_FOLDER['imperator'].subdir, 'game');
		assert.strictEqual(GAME_FOLDER['imperatorrome'].subdir, 'game');
		assert.strictEqual(GAME_FOLDER['europa universalis v'].subdir, 'game');
		assert.strictEqual(GAME_FOLDER['stellaris'].subdir, undefined);
	});

	test('handles alternate Imperator folder names', () => {
		assert.strictEqual(GAME_FOLDER['imperator'].id, 'imperator');
		assert.strictEqual(GAME_FOLDER['imperatorrome'].id, 'imperator');
	});
});

suite('engine — detectFromFolder', () => {
	const noopExists = () => false;

	test('returns null for an unrecognised folder', () => {
		assert.strictEqual(detectFromFolder('/home/user/mymod', noopExists), null);
	});

	test('matches by folder name substring for every supported game', () => {
		assert.strictEqual(detectFromFolder('/mods/Stellaris_v3', noopExists), 'stellaris');
		assert.strictEqual(detectFromFolder('/mods/HOI4_Mod', noopExists), 'hoi4');
		assert.strictEqual(detectFromFolder('/mods/Hearts of Iron', noopExists), 'hoi4');
		assert.strictEqual(detectFromFolder('/mods/Europa Universalis IV', noopExists), 'eu4');
		assert.strictEqual(detectFromFolder('/mods/CK2_whatever', noopExists), 'ck2');
		assert.strictEqual(detectFromFolder('/mods/CK3_whatever', noopExists), 'ck3');
		assert.strictEqual(detectFromFolder('/mods/CK3_Mod', noopExists), 'ck3');
		assert.strictEqual(detectFromFolder('/mods/Vic2', noopExists), 'vic2');
		assert.strictEqual(detectFromFolder('/mods/Victoria 2', noopExists), 'vic2');
		assert.strictEqual(detectFromFolder('/mods/Vic3', noopExists), 'vic3');
		assert.strictEqual(detectFromFolder('/mods/Imperator', noopExists), 'imperator');
		assert.strictEqual(detectFromFolder('/mods/Rome Total War', noopExists), 'imperator');
		assert.strictEqual(detectFromFolder('/mods/EU5', noopExists), 'eu5');
	});

	test('falls back to file content markers when the folder name is opaque', () => {
		const files: Record<string, boolean> = {
			['/x/common/ai_strategy']: true,
		};
		const exists = (p: string) => files[p] === true;
		assert.strictEqual(detectFromFolder('/x', exists), 'hoi4');
		assert.strictEqual(detectFromFolder('/y', noopExists), null);
	});

	test('prefers folder-name hint over content hint', () => {
		const exists = () => true;
		assert.strictEqual(detectFromFolder('/mods/HOI4', exists), 'hoi4');
	});
});

suite('engine — resolveRulesFolder', () => {
	test('returns undefined for an unset value', () => {
		const r = resolveRulesFolder(undefined, { exists: () => true });
		assert.strictEqual(r.path, undefined);
		assert.strictEqual(r.existed, false);
	});

	test('returns undefined for an empty/whitespace value', () => {
		const r = resolveRulesFolder('   ', { exists: () => true });
		assert.strictEqual(r.path, undefined);
		assert.strictEqual(r.existed, false);
	});

	test('regression: a Linux path that exists is returned verbatim and unaltered', () => {
		const raw = '/home/user/cwtools-hoi4-config';
		const r = resolveRulesFolder(raw, {
			platform: 'linux',
			exists: p => p === raw,
		});
		assert.strictEqual(r.path, raw);
		assert.strictEqual(r.existed, true);
	});

	test('regression: a Linux path containing backslashes is never separator-rewritten', () => {
		// Backslash is a legal filename char on Linux; the raw value must win as-is.
		const raw = '/home/user/odd\\dir/config';
		const r = resolveRulesFolder(raw, {
			platform: 'linux',
			exists: p => p === raw,
		});
		assert.strictEqual(r.path, raw);
		assert.strictEqual(r.existed, true);
	});

	test('win32: a JSON-escaped backslash path resolves once normalized', () => {
		// settings.json value "C:\\Users\\me\\config" arrives as this string;
		// only the normalized form exists on disk.
		const raw = 'C:\\Users\\me\\config';
		const normalized = path.win32.normalize(raw);
		const r = resolveRulesFolder(raw, {
			platform: 'win32',
			exists: p => p === normalized,
		});
		assert.strictEqual(r.path, normalized);
		assert.strictEqual(r.existed, true);
	});

	test('expands a leading ~ to the home directory', () => {
		const home = '/home/user';
		const expanded = path.join(home, 'cwtools-config');
		const r = resolveRulesFolder('~/cwtools-config', {
			platform: 'linux',
			home,
			exists: p => p === expanded,
		});
		assert.strictEqual(r.path, expanded);
		assert.strictEqual(r.existed, true);
	});

	test('expands $VAR / ${VAR} env vars on non-win32', () => {
		const env = { RULES: '/opt/rules' };
		const r = resolveRulesFolder('$RULES/hoi4', {
			platform: 'linux',
			env,
			exists: p => p === '/opt/rules/hoi4',
		});
		assert.strictEqual(r.path, '/opt/rules/hoi4');
		assert.strictEqual(r.existed, true);
	});

	test('expands %VAR% env vars on win32', () => {
		const env = { RULES: 'C:\\rules' };
		const expected = path.win32.normalize('C:\\rules\\hoi4');
		const r = resolveRulesFolder('%RULES%\\hoi4', {
			platform: 'win32',
			env,
			exists: p => p === expected,
		});
		assert.strictEqual(r.path, expected);
		assert.strictEqual(r.existed, true);
	});

	test('trims surrounding whitespace and quotes', () => {
		const raw = '  "/home/user/config"  ';
		const r = resolveRulesFolder(raw, {
			platform: 'linux',
			exists: p => p === '/home/user/config',
		});
		assert.strictEqual(r.path, '/home/user/config');
		assert.strictEqual(r.existed, true);
	});

	test('resolves a relative path against the workspace root', () => {
		const r = resolveRulesFolder('rules', {
			platform: 'linux',
			workspaceRoot: '/mods/mymod',
			exists: p => p === path.resolve('/mods/mymod', 'rules'),
		});
		assert.strictEqual(r.path, path.resolve('/mods/mymod', 'rules'));
		assert.strictEqual(r.existed, true);
	});

	test('set-but-missing returns existed:false with a defined best-effort path', () => {
		const r = resolveRulesFolder('  "C:\\Users\\me\\missing"  ', {
			platform: 'win32',
			exists: () => false,
		});
		assert.strictEqual(r.existed, false);
		assert.ok(r.path, 'best-effort path must be defined so the caller can warn');
		assert.ok(!r.path!.includes('"'), 'best-effort path should be trimmed of quotes');
	});
});

suite('engine — serverExe', () => {
	const fakeContext = (abs: string): ExtensionContext =>
		({ asAbsolutePath: (_p: string) => abs } as unknown as ExtensionContext);

	test('returns the server binary path when it exists', () => {
		const ctx = fakeContext('/ext/bin/server/cwtools-server/cwtools-server');
		const out = serverExe(ctx, () => true);
		assert.strictEqual(out, '/ext/bin/server/cwtools-server/cwtools-server');
	});

	test('returns undefined when the binary is not deployed', () => {
		const ctx = fakeContext('/ext/bin/server/cwtools-server/cwtools-server');
		assert.strictEqual(serverExe(ctx, () => false), undefined);
	});

	test('falls back to the per-platform subdir of a packaged vsix', () => {
		const original = Object.getOwnPropertyDescriptor(process, 'platform');
		Object.defineProperty(process, 'platform', { value: 'linux' });
		try {
			const ctx = { asAbsolutePath: (p: string) => '/ext/' + p } as unknown as ExtensionContext;
			const nested = '/ext/' + path.join('bin', 'server', 'cwtools-server', 'linux-x64', 'cwtools-server');
			// Flat path absent (no single-platform binary), nested one present.
			const out = serverExe(ctx, p => p === nested);
			assert.strictEqual(out, nested);
		} finally {
			if (original) Object.defineProperty(process, 'platform', original);
		}
	});

	test('uses the .exe extension on Windows', () => {
		const originalPlatform = Object.getOwnPropertyDescriptor(process, 'platform');
		Object.defineProperty(process, 'platform', { value: 'win32' });
		try {
			const ctx = fakeContext('C:\\ext\\bin\\server\\cwtools-server\\cwtools-server.exe');
			const out = serverExe(ctx, () => true);
			assert.ok(out!.endsWith('cwtools-server.exe'));
		} finally {
			if (originalPlatform) Object.defineProperty(process, 'platform', originalPlatform);
		}
	});
});

suite('engine — runGit', () => {
	function makeChild(opts: { code: number | null; signal: NodeJS.Signals | null; stdout?: string; stderr?: string; error?: Error }): EventEmitter & { stdout: EventEmitter; stderr: EventEmitter } {
		const child = new EventEmitter() as EventEmitter & { stdout: EventEmitter; stderr: EventEmitter };
		child.stdout = new EventEmitter();
		child.stderr = new EventEmitter();
		queueMicrotask(() => {
			if (opts.stdout) child.stdout.emit('data', Buffer.from(opts.stdout));
			if (opts.stderr) child.stderr.emit('data', Buffer.from(opts.stderr));
			if (opts.error) child.emit('error', opts.error);
			else child.emit('close', opts.code, opts.signal);
		});
		return child;
	}

	test('resolves when git exits with code 0', async () => {
		const fakeSpawn = () => makeChild({ code: 0, signal: null, stdout: 'ok\n' });
		await runGit(['status'], fakeSpawn as never);
	});

	test('rejects when git exits non-zero', async () => {
		const fakeSpawn = () => makeChild({ code: 128, signal: null, stderr: 'fatal: not a repo\n' });
		await assert.rejects(
			() => runGit(['pull'], fakeSpawn as never),
			/git exited with code 128/
		);
	});

	test('rejects when git is killed by a signal', async () => {
		const fakeSpawn = () => makeChild({ code: null, signal: 'SIGTERM' });
		await assert.rejects(
			() => runGit(['fetch'], fakeSpawn as never),
			/signal: SIGTERM/
		);
	});

	test('rejects when git fails to spawn', async () => {
		const fakeSpawn = () => makeChild({ code: null, signal: null, error: new Error('ENOENT') });
		await assert.rejects(
			() => runGit(['clone'], fakeSpawn as never),
			/ENOENT/
		);
	});
});
