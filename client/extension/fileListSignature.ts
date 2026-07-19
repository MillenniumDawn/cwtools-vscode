// Type-only import — erased at runtime, so this file stays vscode-free.
import type { FileListItem } from './fileExplorer';
import { fnv1a } from './fnv1a';

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
