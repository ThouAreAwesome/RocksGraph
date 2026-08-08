# Design: `<short name>` — `<one-line summary>`

> **Copy this file** for new designs, or refactor existing ones onto this template.
> Sections marked `[optional]` can be omitted when not applicable.

Status: proposal | in progress | implemented | deferred — update this line when the
implementation state changes, not just when the doc is first written. A design doc
that still says "proposal" after the feature has shipped is worse than no status line
at all.

## Problem

What problem does this design solve?  What's the current behaviour and why is it
insufficient?  Keep it concrete — reference file names, error messages, or
user-facing symptoms.

## Goals & non-goals

- **Goals:** bullet list of what this design must achieve.
- **Non-goals:** explicitly exclude things that are tempting but out of scope.

[optional] ## Existing code to touch

[optional] ## Design

### `<heading per logical component>`

Describe the change per logical component (e.g. data model, on-disk encoding,
new step, new builder method).  Prefer code snippets over prose.

### Constraints / invariants

Any invariants the implementation must maintain.  Examples:
- "Vertex labels must never be read through `get_all_props` on the store path".
- "The optimizer must not fold Property steps that aren't adjacent."

[optional] ## Files changed

Table listing every file touched and what changes in it.

[optional] ## Implementation plan

Ordered checklist of phases, each with a verification step.

[optional] ## Test plan

### `<Group name>`

What to test, and on which API surface (e2e via `gremlin/tests.rs`, physical
step unit test, or store-level test).  Reference the test plan item numbers.

[optional] ## Out of scope

