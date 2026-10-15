# Invalid validation attempt — retained in Git history

These observations are superseded as acceptance evidence because of two
validator gaps: incomplete required-table fixture digest coverage (including the
omitted requester/project allocator) and missing command-semantic validation.
The generated payloads and obsolete harness copies are no longer kept in the
current source tree.

The complete original directory, including its README and SHA256 inventory,
is preserved in commit `cd4a50579ca6016c117c7d8463a60ab8096bc95a`. With that
commit available locally, inspect it without changing the working tree:

```sh
archive=cd4a50579ca6016c117c7d8463a60ab8096bc95a
path=docs/baselines/v2-phase-e/final-gate/invalid-validation-attempt
git ls-tree -r --name-only "$archive" -- "$path/"
git show "$archive:$path/inventory.json"
```

The original inventory maps 16 repository-relative artifact paths to SHA256.
Its historical claims describe the superseded attempt, not the accepted
evidence one directory above. No old hashes or samples were patched into the
accepted run.
