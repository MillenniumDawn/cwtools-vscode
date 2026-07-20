// Type-only import — erased at runtime, so this file stays vscode-free.
import type { FileListItem } from './fileExplorer';

// 32-bit FNV-1a is plenty here: worst case is one skipped refresh on a
// ~1-in-4-billion collision.
function fnv1a(str: string): number {
	let h = 0x811c9dc5;
	for (let i = 0; i < str.length; i++) {
		h ^= str.charCodeAt(i);
		h = Math.imul(h, 0x01000193);
	}
	return h >>> 0;
}

// Field/record separators and the length prefix stop two different lists
// from serializing to the same signature.
export function fileListSignature(fileList: readonly FileListItem[]): string {
	let canonical = '';
	for (const f of fileList) {
		canonical += `\x1e${f.scope}\x1f${f.logicalpath}\x1f${f.uri}`;
	}
	return `${fileList.length}:${fnv1a(canonical).toString(16)}`;
}

export function shouldRefreshFileList(
	previousSignature: string | undefined,
	nextSignature: string,
): boolean {
	return previousSignature !== nextSignature;
}
