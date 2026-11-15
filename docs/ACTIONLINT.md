# Workflow validation

From the repository root on Linux x86-64, run:

```sh
python3 scripts/build/actionlint.py
```

The wrapper downloads the actionlint 1.7.12 and ShellCheck 0.11.0 Linux x86-64 releases, verifies each pinned SHA-256 before extracting an executable, and runs actionlint over the repository's workflows with that ShellCheck for embedded shell. Pyflakes is turned off, so the result does not depend on what the machine has installed. Download, checksum, archive, and checker failures fail the command. CI runs the same command. Updating either version requires independently verifying and updating its archive checksum as well. Existing behavioral workflow regression tests remain required.

`.github/actionlint.yaml` lists GitHub-hosted runner labels newer than the pinned actionlint, such as `ubuntu-26.04`.

Actionlint checks metadata and inputs for local actions referenced by workflows; it does not validate the steps inside composite action metadata, and action metadata files cannot be passed as workflow files. Consult the [actionlint checks documentation](https://github.com/rhysd/actionlint/blob/v1.7.12/docs/checks.md) for the supported surface. This check complements the existing behavioral tests and CodeQL.
