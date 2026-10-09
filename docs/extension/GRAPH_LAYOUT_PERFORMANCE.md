# Linear graph layout partition

The layout grid receives nodes whose degree is zero. All remaining nodes and
all edges go to ELK. A self-loop gives its node nonzero degree, so the node and
loop edge remain together. This replaces component-by-component unions with
one degree filter and one collection difference.

`graphPartition.test.ts` executes the real graph.ts dispatcher with a real
headless Cytoscape harness in `support/graphHeadless.ts`. The existing
`graphWebview.test.ts` suite uses a separate hand-written Cytoscape fake. The normal tests
assert no unions and one degree check per node at sizes 100, 200 and 500, plus
complete edge/component/self-loop preservation. Timing is optional:

```sh
CWTOOLS_GRAPH_BENCH=1 CWTOOLS_GRAPH_BENCH_OUTPUT=/tmp/graph-partition.json \
  npx --no-install vitest run extension/test/unit/graphPartition.test.ts -t 'benchmark 500'
```

For a comparison, copy the same test and support/graphHeadless.ts into a clean
worktree of the base revision and run the command there. Each invocation warms
up five renders and measures 30 renders of the identical fixture: 500 nodes,
490 isolated nodes and a 10-node chain with nine edges. Timing starts after the
first fit and ends immediately before constructing the ELK layout. Recording
layout adapters exclude ELK/grid work. Both revisions instrument the same union,
difference and degree boundaries.

Measured on Linux with Node 26.11.1 and Cytoscape 3.34.3 against base
`f90c4a25a5414f3f4304213d2ac01a5eebeafb4e`, two alternating fixed/base runs:

| Run | Base median / p90 (ms) | Fixed median / p90 (ms) |
| --- | ---: | ---: |
| 1 | 73.332 / 196.444 | 1.116 / 2.302 |
| 2 | 149.264 / 192.778 | 3.000 / 13.918 |

Every base sample made 491 unions with 120,314 input element copies. Every fixed
sample made zero unions, 500 degree checks and one difference with 999 input
elements. The deterministic operation assertions establish the linear bound;
wall times vary on the shared machine. These numbers describe this instrumented
pre-layout path, not browser frame time or total graph opening/ELK time.
