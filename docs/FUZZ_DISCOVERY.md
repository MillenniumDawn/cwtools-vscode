# Fuzz discovery campaigns

The scheduled or manually dispatched workflow runs each existing fuzz target for at most 60 seconds of libFuzzer time, with a ten-second input timeout, 2 GiB RSS limit and 1 MiB maximum input. The helper stops the process group after 20 minutes. Setup and discovery steps have separate deadlines, with a 60-minute job limit reserving time for artifact upload. The existing PR seed replay is unchanged.

Each target's corpus is kept in the Actions cache between runs, so a run starts from what earlier runs found plus the committed seeds. A failed run prints the tail of its log and the replay commands in the step log and the job summary.

Download the matching `fuzz-<target>-<run>` artifact into `engine/fuzz-campaign` at the recorded source revision. `metadata.json` records the source and toolchain, `campaign.log` contains the run output, and `replay.txt` contains a command for each retained crash input. Run those commands from `engine/` with the recorded toolchain and cargo-fuzz version. Inspect and minimize useful inputs, then submit them as reviewed additions to the target's committed seed corpus; the uploaded corpus is retained for 30 days and is not automatically trusted or committed.
