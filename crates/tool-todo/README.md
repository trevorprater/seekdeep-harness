# seekdeep-tool-todo

Model-facing whole-list `todo_write` tool and `todos` session projection.

- Every call appends a `todo/write` snapshot to the calling agent's session; replay is last-write-wins. The item shape is `{ content, status }` with statuses `pending`, `in_progress`, `completed`. A non-agent caller is rejected.
- `allowParallelInProgress` selects whether several todos may be `in_progress` at once; when false, a call marking more than one is rejected.
- The `todos` projection folds the latest whole list, clearing to null on each `turn/start` (standing plan) and keeping the finished checklist on `turn/end`; `stateVersion` is 2.

## Rendering

The canonical result is `{ todos, counts: { pending, inProgress, completed } }`; the Native renderer returns the compact update acknowledgement.

## Model experience

Fixed schema cost; append-only, prefix-stable. Stable failures name empty/duplicate content, the active-item policy, or the missing owning agent session.

## Limitations

Single-owner scope only; deliberately minimal item shape; whole-list replacement is the only operation.
