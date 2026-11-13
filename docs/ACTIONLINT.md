# Workflow validation

From the repository root on Linux x86-64, run:

```sh
python3 scripts/build/actionlint.py
```

The wrapper downloads the actionlint 1.7.12 Linux amd64 release, verifies its pinned SHA-256 before extracting the executable, and runs it over the repository's workflows. Download, checksum, archive, and checker failures fail the command. CI runs the same command. Updating the version requires independently verifying and updating the archive checksum as well. Existing behavioral workflow regression tests remain required.

Actionlint checks metadata and inputs for local actions referenced by workflows; it does not validate the steps inside composite action metadata, and action metadata files cannot be passed as workflow files. It checks embedded shell and Python only through ShellCheck and Pyflakes when those external tools are installed and discoverable. Their presence and versions depend on the environment; this command does not install or pin either tool. Consult the [actionlint checks documentation](https://github.com/rhysd/actionlint/blob/v1.7.12/docs/checks.md) for the supported surface. This check complements the existing behavioral tests and CodeQL.
