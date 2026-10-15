# Invalid observer attempt — retained in Git history

The first full measurement used unequal observer initialization. The candidate
observer had queried SQLite before timed capture; the baseline observer had not.
Baseline capture therefore closed/checkpointed its WAL between CLI processes.
These samples are excluded from release conclusions. Their generated payloads
are no longer kept in the current source tree.

The complete original directory, including its README, is preserved in commit
`cd4a50579ca6016c117c7d8463a60ab8096bc95a`. With that commit available locally,
inspect the archive without changing the working tree:

```sh
archive=cd4a50579ca6016c117c7d8463a60ab8096bc95a
path=docs/baselines/v2-phase-e/final-gate/invalid-observer-attempt
git ls-tree -r --name-only "$archive" -- "$path/"
git show "$archive:$path/measurements.json"
```

The accepted evidence remains one directory above. Its protocol initializes
both observers equally and reruns every size/workload; no sample was
transplanted from this attempt.
