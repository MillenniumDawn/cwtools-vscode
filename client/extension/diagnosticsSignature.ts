// Pure per-URI de-dupe for publishDiagnostics, split out so the node unit tests
// can exercise it without vscode. The server re-publishes diagnostics for all
// ~7,400 files on every scan even when identical; applying each to the
// DiagnosticCollection churns the host thread and repaints the Problems panel.
// Signature the fields that actually render and skip a set that matches the
// last one for that URI.

interface PositionLike { line: number; character: number }
interface RangeLike { start: PositionLike; end: PositionLike }

// Structural subset of vscode.Diagnostic — just the fields we signature. The
// real Diagnostic[] from the handleDiagnostics middleware is assignable to this.
export interface DiagnosticLike {
	range: RangeLike;
	severity?: number;
	code?: string | number | { value: string | number };
	message: string;
	source?: string;
	relatedInformation?: readonly unknown[];
}

function fnv1a(str: string): number {
	let h = 0x811c9dc5;
	for (let i = 0; i < str.length; i++) {
		h ^= str.charCodeAt(i);
		h = Math.imul(h, 0x01000193);
	}
	return h >>> 0;
}

function codeString(code: DiagnosticLike['code']): string | number | null {
	if (code === undefined) { return null; }
	return typeof code === 'object' ? code.value : code;
}

// FNV-1a over a JSON canonical form. JSON is used (rather than raw separators)
// because message is arbitrary server text that could otherwise inject a field
// or record boundary; JSON escapes control bytes, so two different lists can't
// serialize to the same string. relatedInformation is reduced to presence.
export function diagnosticsSignature(diagnostics: readonly DiagnosticLike[]): string {
	const normalized = diagnostics.map(d => [
		d.range.start.line,
		d.range.start.character,
		d.range.end.line,
		d.range.end.character,
		d.severity ?? null,
		codeString(d.code),
		d.source ?? null,
		(d.relatedInformation?.length ?? 0) > 0,
		d.message,
	]);
	return `${diagnostics.length}:${fnv1a(JSON.stringify(normalized)).toString(16)}`;
}

// Records the last-published signature per URI key and reports whether an
// incoming publish differs (and so should reach the DiagnosticCollection).
// Kept vscode-free so skip/clear are unit-testable; lspClient supplies
// uri.toString() as the key and clears it when the client leaves Running (the
// client drops the collection on stop, so a stale cache would suppress the
// re-publish after restart and lose squiggles).
export class DiagnosticsSignatureCache {
	private readonly signatures = new Map<string, string>();

	shouldPublish(uriKey: string, diagnostics: readonly DiagnosticLike[]): boolean {
		const signature = diagnosticsSignature(diagnostics);
		if (this.signatures.get(uriKey) === signature) {
			return false;
		}
		this.signatures.set(uriKey, signature);
		return true;
	}

	clear(): void {
		this.signatures.clear();
	}
}
