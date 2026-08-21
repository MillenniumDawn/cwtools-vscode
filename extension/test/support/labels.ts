/**
 * Pure, host-free test helpers. Nothing here may import `vscode`, because the
 * parity harness runs under plain mocha with no VS Code
 * host and pulls these helpers in directly.
 */

/**
 * Extract the display label from a CompletionItem, handling both the plain
 * string form and the { label: string } form used by some LSP responses.
 */
export function extractCompletionLabel(item: { label?: string | { label?: string } }): string {
  return typeof item.label === 'string' ? item.label : item.label?.label ?? '';
}
