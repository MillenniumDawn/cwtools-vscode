# Issue #1097: folder move watcher investigation

## Finding

A real VS Code host reproduction showed that moving a populated folder into a served workspace can forward only the directory `CREATED` event. The server previously treated that directory as a resource URI, so it did not index the contained texture and the open dependent kept CW113. The server now expands a bounded directory event into eligible descendants, preserving ignore/exclusion and workspace/symlink boundaries; oversized folders use the existing workspace-scan fallback.

## Observed reproduction

The regression ran in a Linux Xvfb extension host with VS Code 1.135.0 and the final #1107 watcher setup. The test used an explicit local filepath rule, a synthetic vanilla texture resource, an open reference to `events/assets/moved.tga`, and a populated folder containing `moved.dds` plus a script.

- Before the runtime fix, the open document had exactly one CW113 for `events/assets/moved.tga`; the synthetic vanilla control resolved. On moving the folder, VS Code reported `create:<events/assets>` only: no texture or script events. No texture event was forwarded to the server, and CW113 remained after 15 seconds.
- After the runtime fix, VS Code again reported only the directory create event; the texture/script child event lists remained empty. The server received one forwarded directory event, expanded the directory, and CW113 cleared without an explicit reindex or background-reindex pass. All three tests in the watched host label passed in 12 seconds.

This is an observed Linux/VS Code 1.135.0 result; Windows watcher behavior was not measured. The Rust regression also sends a directory-only event, verifies texture indexing and script validation, checks an excluded `.git` start and descendant, and proves symlinked resources are not indexed. A bounded-overflow regression exercises a lost-scan-CAS retry.

## Implementation boundaries

Only a directory that arrived as `CREATED` is expanded; a `CHANGED` directory is not walked. Expansion starts only beneath the canonical editable workspace root, which decides containment alone: descendant URIs keep the spelling of the event's own path, so they match the index and open-document keys when the workspace path is not canonical. It checks the directory itself and its ancestors for symlinks, does not follow symlink entries, skips ignored/excluded directory descendants, preserves the root-only `resources` exclusion, and has a 10,000-entry limit. If the limit is exceeded while another scan owns the scan slot, the original resource-class folder event is requeued so the existing scan fallback can retry it after the slot is released. Resource-only ordinary batches still bypass the watched batch cap.

## Validation record

The parent-run diagnostics guards matched their captured scratch baselines exactly: MD 12,796 records and vanilla 5 records; committed baselines were not changed. Those guard inputs included the Millennium Dawn corpus at `af2626cda8` with dirty corpus state and rules at `40a563c`; the run reported no CLI diagnostic movement. No Windows host run is claimed.
