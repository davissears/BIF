# Invalid observer attempt — excluded from release conclusions

The first full measurement used unequal observer initialization. The candidate
observer had queried SQLite before timed capture; the baseline observer had not.
Baseline capture therefore closed/checkpointed its WAL between CLI processes.
These raw samples are preserved for audit, not selected as release evidence.

The final protocol initializes both observers through `sqlite_schema` and runs
the same explicit initial checkpoint on both after setup. Every size/workload
is rerun under that protocol; no sample is transplanted from this attempt.
