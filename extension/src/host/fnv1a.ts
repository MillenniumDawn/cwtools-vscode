// 32-bit FNV-1a is plenty for change-detection signatures: worst case is one
// skipped refresh on a ~1-in-4-billion collision.
export function fnv1a(str: string): number {
	let h = 0x811c9dc5;
	for (let i = 0; i < str.length; i++) {
		h ^= str.charCodeAt(i);
		h = Math.imul(h, 0x01000193);
	}
	return h >>> 0;
}
