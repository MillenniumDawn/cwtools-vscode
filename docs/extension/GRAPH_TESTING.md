# Graph webview regression tests

Run `npx --no-install vitest run extension/test/unit/graphLayout.test.ts` for the
headless graph suite, or `npm run test:node:coverage` for the full node report.

The suite runs on the headless Cytoscape harness in
`extension/test/unit/support/graphHeadless.ts`.
[Graph layout partition performance](GRAPH_LAYOUT_PERFORMANCE.md) describes that
harness and covers the isolated/connected partition. Here the real canvas.ts
extension also runs, against a small DOM/canvas boundary stub.

The suite covers asynchronous layout ordering, stalled-layout reporting and
replacement cleanup, the closed neighborhood hover classes, bounded class-update
work, drawExtra label caching and shadow limits, canvas pixel ratios/resize/
transforms, lazy tooltip creation, detail expansion, simple/expanded mouseout
behavior, graph-replacement cleanup and edge building. Import validation is in
`graphImportMetadata.test.ts` on the same harness. Rejected image merges and
FileReader export errors are in `graphExport.test.ts` on that harness. Import
failures and successful image export run against the real webview in the `host`
label (`extension/test/host/extension.test.ts`).

These tests measure graph correctness and application operations before layout.
They do not measure Electron rendering, ELK computation, or browser frame time.
Use the same deterministic inputs and procedure on both revisions for any
performance claim; keep operation counts separate from elapsed timings.
