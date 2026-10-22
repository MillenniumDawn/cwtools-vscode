# Issue #1097: folder move watcher investigation

## Finding

No production change is justified yet. The server-side hypothesis is plausible, but the required VS Code reproduction could not run, so it is not known whether moving a populated folder emits child-file events under the final #1107 watcher configuration.

## Source trace

`engine/crates/lsp/src/scan/watched.rs` queues `CREATED` watched URIs. During batch processing, resource-class URIs go to `sync_watched_resources`, which accepts only paths for which `Path::is_file()` is true. A directory URI by itself therefore does not add the resources beneath it to the file index. If child-file `CREATED` events arrive, those file URIs can be indexed through the existing path. When the index changes, open filepath dependents are revalidated; that is the path expected to clear CW113 without a workspace reindex.

## Reproduction status

The VS Code host test could not start because `hosttest.py` could not resolve `update.code.visualstudio.com` (`EAI_AGAIN`). No cached VS Code installation or local VS Code binary was available. A request to the official update endpoint was also blocked by the environment's CONNECT tunnel (HTTP 403). Consequently, this investigation has **no observed event counts, URIs, or event types**, and makes no claim about whether VS Code emits the directory, its children, or both.

## Reproduction needed

Run in an extension host using the final #1107 watcher configuration, where the server retains its dynamic `**/*` watched-file registration and the client keeps the `.cwt` watcher for rule reload and lint:

1. Open a workspace with an already-indexed resource set and an open script that references a texture path which is currently missing, so CW113 is present.
2. Outside the workspace, prepare one directory containing a texture at that referenced relative path and a script file. Move the directory into the workspace with a single folder move.
3. Capture the exact watched-file notifications forwarded from the extension to the server, including each URI and `CREATED`/`CHANGED`/`DELETED` type. Record counts separately for the moved directory URI, the texture URI, and the script URI.
4. Without running a workspace reindex, wait for the open dependent's CW113 to clear. Record whether it clears and which resource event preceded the change.
5. Keep the fixture inside an ordinary served workspace directory. Confirm the test path does not cross an access boundary or an excluded path, so those guards remain unchanged.

If child-file creation events are observed and the open dependent clears CW113 without reindex, close the issue without adding traversal. If only the directory event is forwarded and CW113 remains, implement bounded traversal only after preserving the existing workspace authorization, ignore and exclusion filters, and symlink/access boundaries; add a regression test using the measured event shape.
