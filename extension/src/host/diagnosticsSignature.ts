import { fnv1a } from './fnv1a';

interface PositionLike { line: number; character: number }
interface RangeLike { start: PositionLike; end: PositionLike }

// Structural subset of vscode.Diagnostic — keeps this file vscode-free.
export interface DiagnosticLike {
	range: RangeLike;
	severity?: number;
	code?: string | number | { value: string | number };
	message: string;
	source?: string;
	relatedInformation?: readonly unknown[];
}

function codeString(code: DiagnosticLike['code']): string | number | null {
	if (code === undefined) { return null; }
	return typeof code === 'object' ? code.value : code;
}

// JSON (not raw separators): message is arbitrary server text that could
// otherwise inject a field/record boundary.
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

// Cleared on client restart: vscode-languageclient drops the
// DiagnosticCollection on stop, so a stale cache would suppress the
// re-publish and lose squiggles.
export class DiagnosticsSignatureCache {
	private readonly signatures = new Map<string, string>();
	// Bound the map so a session touching many files (or re-scan churn) can't
	// grow it unbounded until restart. Evicting the oldest entry only costs one
	// re-publish for that file, which is harmless.
	private readonly maxSize = 1000;

	shouldPublish(uriKey: string, diagnostics: readonly DiagnosticLike[]): boolean {
		const signature = diagnosticsSignature(diagnostics);
		if (this.signatures.get(uriKey) === signature) {
			return false;
		}
		this.signatures.set(uriKey, signature);
		// Map preserves insertion order, so the first key is the oldest.
		if (this.signatures.size > this.maxSize) {
			const oldest = this.signatures.keys().next().value;
			if (oldest !== undefined) {
				this.signatures.delete(oldest);
			}
		}
		return true;
	}

	clear(): void {
		this.signatures.clear();
	}
}
