# Graph webview regression tests

Run `npx --no-install vitest run extension/test/unit/graphLayout.test.ts` for the
headless graph suite, or `npm run test:node:coverage` for the full node report.

`extension/test/unit/support/graphHeadless.ts` drives the normal graph.ts message
dispatcher using real headless Cytoscape. Collections, connected components,
selectors, classes, scratch state and event propagation run unchanged. The
real canvas.ts extension also runs, against a small DOM/canvas boundary stub.
The asynchronous ELK and grid layout engines are recording adapters, so tests
can assert exactly which nodes and edges each layout receives without waiting
on external layout workers or coupling assertions to their coordinates.

The suite covers the isolated/connected split (including self-loops), the closed
neighborhood hover classes, bounded class-update work, drawExtra label caching
and shadow limits, canvas pixel ratios/resize/transforms, and lazy tooltip
creation, detail expansion, simple/expanded mouseout behavior and graph-replacement
cleanup. It complements graphWebview.test.ts,
whose fake Cytoscape boundary remains useful for import/export failure cases.

These tests measure graph correctness and application operations before layout.
They do not measure Electron rendering, ELK computation, or browser frame time.
Use the same deterministic inputs and procedure on both revisions for any
performance claim; keep operation counts separate from elapsed timings.
