# Graph hover wiring and retained state

Tooltips use one delegated mouseover/mouseout pair. A node's tooltip handlers,
DOM content, reference and Tippy instance are created on first hover and kept
in node scratch state. Simple tips hide on exit; expanded interactive tips
retain their existing behavior. Repeated entry cancels the previous expansion
timer, and replacing the graph clears every pending expansion and destroys
all created tips.

Highlighting retains only the active closed neighborhood in core scratch state.
Each entry computes a transient complement, toggles its dim class, and discards
that collection. The neighborhood removes its dim class and adds its highlight. Exit clears both classes and active scratch state.
A late mouseout from a previous node does not clear a newer node's highlight.
There is no per-node neighborhood Map or retained complement collection.

Run `npx --no-install vitest run extension/test/unit/graphHoverScaling.test.ts`
for the correctness/scaling suite. It uses the real headless Cytoscape dispatcher
and asserts constant listener count, no complement caching, one active hood,
lazy tooltip lifecycle, cancellation and class cleanup. To reproduce operation
counts on identical inputs, copy that test and support/graphHeadless.ts into a
clean base worktree and run the same command on both revisions:

```sh
CWTOOLS_HOVER_BENCH=1 CWTOOLS_HOVER_BENCH_OUTPUT=/tmp/graph-hover.json \
  npx --no-install vitest run extension/test/unit/graphHoverScaling.test.ts -t 'record listener'
```

On Linux, Node 26.11.1 and Cytoscape 3.34.3, the base revision
`f90c4a25a5414f3f4304213d2ac01a5eebeafb4e` and this change used the identical
500-node/500-edge ring and hovered every node in order:

| Recorded operation/state | Base | Changed |
| --- | ---: | ---: |
| Per-node listener registrations | 1,000 | 0 |
| Core listener registrations | 6 | 8 |
| Retained complement collections | 500 | 0 |
| Elements held by those complements | 497,500 | 0 |

The changed path retains at most one neighborhood, bounded by graph size
(five elements for a ring), and releases it on exit. Tooltip state can grow to
one instance per actually hovered node, bounded by node count. Dimming still
touches the graph's elements; these counts establish listener/storage bounds,
not constant-time hover or a browser rendering speed claim. Transient collection
allocation remains linear per hover; no complement is retained between events.
