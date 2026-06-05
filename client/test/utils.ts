import * as vscode from 'vscode';

export async function activate() {
  const ext = vscode.extensions.getExtension('tboby.cwtools-vscode')!;
  try {
    await ext.activate();
    return ext.exports;
  } catch (error) {
    // Extension activation might fail due to missing language server in test environment
    // But we can still test other aspects of the extension
    console.warn('Extension activation had issues (expected in test environment):', error);
    return ext.exports;
  }
}

/**
 * Shared small test utilities to reduce duplication across suites
 */
export async function wait(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}

export async function retryAsync(fn: () => Promise<boolean>, maxRetries = 3, delayMs = 500): Promise<boolean> {
  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    try {
      const result = await fn();
      if (result === true) {
        return true;
      }
    } catch (err) {
      if (attempt === maxRetries) {
        throw err;
      }
    }
    if (attempt < maxRetries) {
      await wait(delayMs);
    }
  }
  return false;
}

export async function openDocumentAndShow(uri: vscode.Uri): Promise<vscode.TextDocument> {
  const doc = await vscode.workspace.openTextDocument(uri);
  await vscode.window.showTextDocument(doc);
  return doc;
}

/**
 * Wait for the language server to respond to completion requests. This
 * indicates the server has finished its first pass and is providing LSP
 * features (not just text fallback).
 */
export async function waitForLSP(uri: vscode.Uri, maxRetries = 60, delayMs = 500): Promise<void> {
  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    try {
      vscode.languages.getDiagnostics(uri);
      const completions = await vscode.commands.executeCommand<vscode.CompletionList>(
        'vscode.executeCompletionItemProvider',
        uri,
        new vscode.Position(12, 0)
      );
      if (completions?.items?.length) {
        const hasLspCompletions = completions.items.some(item => (item.kind || 0) !== 0);
        if (hasLspCompletions) {
          console.log(`LSP ready after ${attempt} attempts (${attempt * delayMs}ms) — found ${completions.items.length} completions`);
          return;
        }
      }
    } catch (error) {
      console.log(`LSP check attempt ${attempt} failed:`, error instanceof Error ? error.message : String(error));
    }
    if (attempt < maxRetries) {
      await wait(delayMs);
    }
  }
  throw new Error(`LSP not ready after ${maxRetries} attempts (${maxRetries * delayMs}ms total)`);
}

/**
 * Wait for the language server to respond to hover requests at any position.
 * Cheaper than waitForLSP; use when a test only needs hover, not completions.
 */
export async function waitForLanguageServer(uri: vscode.Uri, maxRetries = 30, delayMs = 500): Promise<boolean> {
  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    try {
      const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
        'vscode.executeHoverProvider',
        uri,
        new vscode.Position(0, 0)
      );
      if (hovers !== undefined) {
        console.log(`Language server ready after ${attempt} attempts (${attempt * delayMs}ms)`);
        return true;
      }
    } catch (error) {
      console.log(`LSP check attempt ${attempt} failed:`, error instanceof Error ? error.message : error);
    }
    if (attempt < maxRetries) {
      await wait(delayMs);
    }
  }
  return false;
}

/**
 * The currently active language server engine, as configured in
 * cwtools.engine. Defaults to 'rust'.
 */
export function currentEngine(): 'rust' | 'fsharp' {
  const value = vscode.workspace.getConfiguration('cwtools').get<string>('engine');
  return value === 'fsharp' ? 'fsharp' : 'rust';
}

/**
 * Set the engine and reload the window so the next activation picks it up.
 * Used by the parity suite to test both engines in one run.
 */
export async function setEngineAndReload(engine: 'rust' | 'fsharp'): Promise<void> {
  await vscode.workspace.getConfiguration('cwtools').update('engine', engine, vscode.ConfigurationTarget.Global);
  await vscode.commands.executeCommand('workbench.action.reloadWindow');
}
