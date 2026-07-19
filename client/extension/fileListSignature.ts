// Pure change-guard for the updateFileList notification, split out so the node
// unit tests can exercise it without vscode (FileListItem is a type-only import,
// erased at runtime). The server re-sends the full ~1MiB file list on every
// scan; refreshing the TreeView rebuilds the whole tree, so skip it when the
// list is byte-for-byte the same as last time.

import type { FileListItem } from './fileExplorer';

// FNV-1a over the canonical string. A 32-bit non-crypto hash is plenty here:
// worst case is one skipped refresh on a ~1-in-4-billion collision, cheaper
// than storing the full serialized list.
function fnv1a(str: string): number {
	let h = 0x811c9dc5;
	for (let i = 0; i < str.length; i++) {
		h ^= str.charCodeAt(i);
		h = Math.imul(h, 0x01000193);
	}
	return h >>> 0;
}

// Order-sensitive signature over the fields the tree actually renders (scope,
// logicalpath, uri). Field/record separators keep two different lists from
// serializing to the same string; the length prefix means a count change can
// never collide with the hash alone.
export function fileListSignature(fileList: readonly FileListItem[]): string {
	let canonical = '';
	for (const f of fileList) {
		canonical += `\x1e${f.scope}\x1f${f.logicalpath}\x1f${f.uri}`;
	}
	return `${fileList.length}:${fnv1a(canonical).toString(16)}`;
}

// Refresh only when the signature changed. A missing previous signature (first
// notification) always refreshes.
export function shouldRefreshFileList(
	previousSignature: string | undefined,
	nextSignature: string,
): boolean {
	return previousSignature !== nextSignature;
}
