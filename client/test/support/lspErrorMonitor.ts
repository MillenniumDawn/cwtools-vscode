import * as assert from 'assert';
import { defaultClient } from '../../extension/extension';

interface ErrorEntry {
    timestamp: number;
    message: string;
}

// Global state for error monitoring
let errorLog: ErrorEntry[] = [];
let testStartTime: number = 0;
let originalAppendLine: ((value: string) => void) | undefined;
let isMonitoringActive = false;
// The client's error detail arrives as the appendLine right after its frame.
let captureDetailLine = false;

/**
 * Sets up LSP error monitoring by intercepting the output channel
 * This should be called once at the start of each test
 */
export function setupLSPErrorMonitoring(): void {
    // Mark the start time for this test
    testStartTime = Date.now();

    // Only setup the interceptor once
    if (!isMonitoringActive && defaultClient && defaultClient.outputChannel) {
        originalAppendLine = defaultClient.outputChannel.appendLine.bind(defaultClient.outputChannel);

        defaultClient.outputChannel.appendLine = (message: string) => {
            const timestamp = Date.now();

            // The client writes "[Error - <time>] <msg>" then an unprefixed
            // detail line (stack / Message: / Code:); the extension's own logger
            // writes "[ERROR]". Matching a bare "error" substring instead would
            // catch server INFO like "0 errors" and fail healthy runs.
            const isErrorFrame =
                message.startsWith('[Error') || message.startsWith('[ERROR]');
            if (isErrorFrame || captureDetailLine) {
                errorLog.push({
                    timestamp,
                    message: message
                });
            }
            captureDetailLine = isErrorFrame;

            // Call the original method
            return originalAppendLine!(message);
        };

        isMonitoringActive = true;
    } else {
        // If already monitoring, just update the test start time
        testStartTime = Date.now();
    }
}

/**
 * Checks for LSP errors that occurred since the test started
 * Fails the test if any errors are found
 */
export function checkForLSPErrors(testName: string): void {
    // Filter errors to only those that occurred during or after this test started
    const testErrors = errorLog.filter(entry => entry.timestamp >= testStartTime);
    if (testErrors.length > 0) {
        const errorMessages = testErrors.map(entry =>
            `[${new Date(entry.timestamp).toISOString()}] ${entry.message}`
        ).join('\n');

        // Remove the errors we're reporting so they don't affect future tests
        errorLog = errorLog.filter(entry => entry.timestamp < testStartTime);

        assert.fail(`LSP Server errors detected during test "${testName}":\n${errorMessages}`);
    }
}

/**
 * Completely tears down error monitoring (call this at the very end of all tests)
 */
export function teardownLSPErrorMonitoring(): void {
    if (isMonitoringActive && defaultClient && defaultClient.outputChannel && originalAppendLine) {
        // Restore the original appendLine method
        defaultClient.outputChannel.appendLine = originalAppendLine;
        originalAppendLine = undefined;
        isMonitoringActive = false;
    }

    // Clear the error log
    errorLog = [];
    testStartTime = 0;
}