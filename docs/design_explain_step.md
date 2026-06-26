# Design: `explain()` — physical plan pretty-printing

Status: proposal. Not implemented.

## Problem

There is no way to inspect what physical plan the engine will execute for a
given traversal.  The existing `PhysicalPlan::fmt` (Debug) walks the linear
chain and outputs field-level Debug representations, but that output includes
runtime state (buffers, frontier cursors, current indices) and isn't a format
anyone would want to depend on being stable.  A dedicated `explain()` terminal
should return a clean, structured, user-facing tree of what the engine will do.

## Design

Add a single method to `CoreStep` — `explain()` → `ExplainNode` — threaded
through `GremlinStep` / `BufferedStep` the same way `reset()` and `upper()`
already are.  `ExplainNode` is a small structured tree (`{name, params,
children}`).  One recursive renderer walks the tree and prints proper
indentation with branch labels.  The output mirrors Postgres / Spark
`EXPLAIN`.

### Structured node

```rust
/// A node in the explain tree.  Name + key-value params + labelled sub-trees.
struct ExplainNode {
    name: &'static str,
    params: Vec<(&'static str, String)>,
    children: Vec<(String, ExplainNode)>,  // (branch_label, sub_tree)
}
```

`params` is a list of `(key, value)` pairs — flat, no nesting — e.g.
`("direction", "Out")`, `("labels", "[\"knows\"]")`.  `children` carries
sub-trees for branching operators: each entry has a label (empty for the
linear backbone, `"branch 0"` / `"branch 1"` for unions, `"body"` / `"until"`
for repeat) and a child `ExplainNode`.

### Trait method

One addition to `CoreStep`.  There is **no default impl** — every step author
must provide one explicitly.  This prevents a forgotten impl from silently
producing a blank node (which was exactly what happened when a throwaway test
walked a union branch's injection-point `VecSourceStep` through the default
no-op and got an empty name).  Steps that genuinely have nothing to say still
return a node with a name — `"VecSourceStep"` is never silent.

```rust
pub trait CoreStep: std::fmt::Debug {
    fn explain(&self) -> ExplainNode;
    // ... produce, reset, upper, add_upper unchanged
}
```

`GremlinStep` gains `fn explain(&self) -> ExplainNode;`, and
`BufferedStep<T>` delegates to `self.inner.borrow().core.explain()` — same
pattern as the existing `reset()` and `upper()` wiring.

### Step-specific impls — one per step file

Linear steps return `name` + `params`, no children:

```rust
// in_out.rs
fn explain(&self) -> ExplainNode {
    ExplainNode {
        name: "InOutStep",
        params: vec![("direction", format!("{:?}", self.direction))],
        children: vec![],
    }
}
```

Branching steps populate children from their nested `PhysicalPlan` fields:

```rust
// union.rs
fn explain(&self) -> ExplainNode {
    ExplainNode {
        name: "UnionStep",
        params: vec![],
        children: self.plans.iter().enumerate()
            .map(|(i, plan)| (format!("branch {}", i), plan.explain()))
            .collect(),
    }
}
```

Steps that carry sub-plans (`RepeatStep`, `CoalesceStep`, `ChooseStep`,
`WhereStep`, `NotStep`) all follow the same pattern — each nested
`PhysicalPlan` becomes a labelled child.

### `PhysicalPlan::explain()` — walks the backbone, stops at injection point

`upper()` gives the linear operator chain from tail to source.  The walk must
**stop at and exclude** `self.source` — every nested sub-plan (where branches,
union branches, repeat body/until, coalesce, not) is built via a fresh
recursive `build_steps` call that creates its own `VecSourceStep` injection
point, and `upper()` walks straight into it.  Without an explicit stop, branch
children silently include a blank source node.  Use `Rc::ptr_eq` against the
stored source:

```rust
impl PhysicalPlan {
    pub fn explain(&self) -> ExplainNode {
        let mut nodes = Vec::new();
        let mut current = Some(self.tail.clone());
        while let Some(step) = current {
            // Stop at the injection point — do not include it in the tree.
            if Rc::ptr_eq(&step, &(self.source.clone() as StepRef)) {
                break;
            }
            nodes.push(step.explain());
            current = step.upper();
        }
        nodes.reverse();
        ExplainNode {
            name: "PhysicalPlan",
            params: vec![],
            children: nodes.into_iter().map(|n| (String::new(), n)).collect(),
        }
    }
}
```

The result is a flat list of siblings under the root `PhysicalPlan` node —
each step at the same indent level.  Branching steps carry their own children
one level deeper.  This matches how Postgres `EXPLAIN` renders a linear
pipeline.

### Renderer — separate, pure function

```rust
fn render(node: &ExplainNode, depth: usize, prefix: &str) -> String {
    let indent = "  ".repeat(depth);
    let params_str = if node.params.is_empty() {
        String::new()
    } else {
        format!("({})", node.params.iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join(", "))
    };
    let mut out = format!("{}{}{}{}\n", indent, prefix, node.name, params_str);
    for (label, child) in &node.children {
        let child_prefix = if label.is_empty() {
            "  └─ ".to_string()
        } else {
            format!("    {}: ", label)
        };
        out.push_str(&render(child, depth + 1, &child_prefix));
    }
    out
}
```

### Traversal terminal

A new terminal method on both `ReadTraversal` and `WriteTraversal`:

```rust
pub fn explain(self) -> Result<String, StoreError> {
    // 1. Build logical plan (same as next / to_list / iter)
    // 2. Apply optimizer rules
    // 3. Build physical plan via PhysicalPlanBuilder
    // 4. Render instead of executing — no prefix, root node prints its own name
    Ok(render(&physical_plan.explain(), 0, ""))
}
```

The `""` prefix on the root call means the first line prints `PhysicalPlan`
without a leading tree glyph — it's the root node, not a child of anything.

### Example output

Single-path traversal — `g.V([1]).out("knows").both("knows").hasLabel("person").count()`:

```
PhysicalPlan
  └─ VStep(ids=[1])
  └─ InOutStep(direction=Out, labels=["knows"])
  └─ InOutStep(direction=Both, labels=["knows"])
  └─ HasLabelStep(labels=["person"])
  └─ CountStep()
```

Branching traversal — `g.V([1]).union([__.out("knows"), __.in("knows")]).count()`:

```
PhysicalPlan
  └─ VStep(ids=[1])
  └─ UnionStep
      branch 0: PhysicalPlan
        └─ InOutStep(direction=Out, labels=["knows"])
      branch 1: PhysicalPlan
        └─ InOutStep(direction=In, labels=["knows"])
  └─ CountStep()
```

The backbone is a flat list of siblings under `PhysicalPlan`.  Each branch
child is a nested `PhysicalPlan` (its own `explain()` call) containing that
branch's steps.  `UnionStep` itself is a backbone node; its children are the
branch sub-trees, which are rendered indented one level with a branch label.
`CountStep` follows `UnionStep` as the next backbone sibling — the union's
output flows into the count, not into the union's branches.

## Files changed

| File | Change |
|------|--------|
| `engine/volcano/steps/traits.rs` | `ExplainNode` struct; `CoreStep::explain()` (no default); `GremlinStep::explain()`; `BufferedStep` delegation |
| `engine/volcano/builder/mod.rs` | `PhysicalPlan::explain()` with `Rc::ptr_eq` stop-at-source; render function |
| `gremlin/traversal/mod.rs` | `explain()` terminal on `ReadTraversal` / `WriteTraversal` |
| `engine/volcano/steps/*.rs` (~30 files) | Per-step `explain()` impl — mechanical, compiler-enforced (no default) |

## Implementation notes

- **Label name resolution**: first-pass output uses numeric label IDs throughout.
  Steps store resolved `LabelId` values internally; label-name → ID mapping
  happens during plan building but the resolved strings are stored in the
  schema, not in the step structs.  `HasLabelStep` and `InOutStep` carry
  `SmallVec<[LabelId; …]>` fields — no string names.  A follow-up pass could
  thread a `&Schema` reference into `explain()` to resolve IDs to names in
  params.  Numeric IDs are acceptable for a first pass.
- `ExplainNode` is not `pub` — it lives in `engine::volcano::steps::traits`
  and is only consumed by the renderer and the traversal terminal.
- The `explain()` terminal does not execute the plan; it returns immediately
  after building it.  No traversers are injected into `VecSourceStep`.
- The `Debug` impl on `PhysicalPlan` is kept as-is (useful for internal
  debugging).  `explain()` is the user-facing path.

## Test plan

Tests live in `engine/volcano/builder/tests/` (or alongside the existing
`mod tests` in `builder/mod.rs`).  Each test builds a `LogicalPlan`, applies
optimizer rules, compiles to `PhysicalPlan`, calls `physical_plan.explain()`,
renders the result, and asserts that the output contains expected step names
in the expected order.  This is a direct upgrade of the existing
`builder::tests::debug_print` tests, which currently string-match against the
`Debug` output.

Every optimizer rule gets at least one explain-level regression test.
"Correct" for an optimizer rule means: a step the rule is supposed to
eliminate or transform must **not** appear in the explain tree (or must
appear in its transformed form), and the physical step that the rule folded
into must carry the expected params.

### Group 1 — `V + hasId` folding (`merge_v_id_filter`)

| Input | Expected tree (after optimizer) |
|-------|--------------------------------|
| `V([]).hasId([1])` | `VStep(ids=[1])` — no `HasIdStep` or `ScalarFilterStep` |
| `V([]).has("id", eq(1))` | `VStep(ids=[1])` — same, via `Key::Property("id")` path |
| `V([]).hasId([1]).hasId([2])` | `VStep(ids=[1])` — only first `hasId` folds; second stays as `HasIdStep` |
| `V([]).has("name", "alice").hasId([1])` | `HasPropertyStep` then `VStep(ids=[1])` — filter reorder pushes `hasId` up; verify order swapped |
| `V([]).has("id", within([1,2,3]))` | `ScalarFilterStep` — `Within` on id is **not** folded (only `Eq` and `Within` on small sets fold) |
| `V([42]).hasId([1])` | `VStep(ids=[42])` then `HasIdStep` — explicit `V(ids)` takes precedence; `hasId` is not folded |

### Group 2 — `outE/inE/bothE` + end-vertex filter folding (`merge_end_vertex_filter` + `extract_end_vertex_filter`)

| Input | Expected tree (after optimizer) |
|-------|--------------------------------|
| `V([1]).outE("knows").where(otherV().hasId([2]))` | `GetEStep(labels=["knows"], end_vertex_ids=[2])` — no `WhereStep`, no `OtherVStep`, no `HasIdStep` |
| `V([1]).bothE("knows").where(otherV().hasId([2]))` | `GetEStep(labels=["knows"], end_vertex_ids=[2])` — bothE also folds |
| `V([1]).inE("knows").where(otherV().hasId([2]))` | `GetEStep(labels=["knows"], end_vertex_ids=[2])` — inE also folds |
| `V([1]).outE("knows").where(otherV().has("age", eq(30)))` | `GetEStep(labels=["knows"])` then `EndVertexFilterStep` — property filter extracted but stays as separate step |
| `V([1]).outE("knows").where(otherV().hasId([2]).has("age", gt(30)))` | `GetEStep(labels=["knows"], end_vertex_ids=[2])` then `EndVertexFilterStep` — hasId folds into GetE, hasProperty stays as EndVertexFilter |
| `V([1]).outE("knows").has("rank", 0).where(otherV().hasId([2]))` | `GetEStep(labels=["knows"], end_vertex_ids=[2], rank=0)` — rank also folds |
| `V([1]).outE("knows").where(otherV().hasId([2])).where(otherV().hasId([3]))` | Both where-clauses extracted: `GetEStep(labels=["knows"], end_vertex_ids=[2,3])` |
| `V([1]).bothE("knows").where(otherV().hasId([2])).where(otherV().hasLabel("person"))` | hasId folds into `GetEStep(labels=["knows"], end_vertex_ids=[2])`, hasLabel stays as `EndVertexFilterStep` |
| `V([1]).outE().where(otherV().hasId([2]))` | `InOutStep` + `EndVertexFilterStep` — no label → edge scan, not point lookup; filter stays separate |
| `V([1]).outE("knows")` (no where) | `InOutStep` — no filter to fold; plan is unchanged |
| `V([1]).outE("knows").has("weight", gt(0.5))` | `GetEStep(labels=["knows"])`. has("weight",...) should **not** be optimized — it filters on the edge itself, not the other vertex. |

### Group 3 — `addV` + `property("id", N)` folding (`merge_addv_id`)

| Input | Expected tree |
|-------|--------------|
| `addV("person").property("id", 1)` | `AddVStep(label="person", id=Some(1))` — no `PropertyStep` for id |
| `addV("person").property("name", "alice").property("id", 1)` | `AddVStep(label="person", id=Some(1))` — trailing non-id properties preserved but the id property is consumed |
| `addV("person").property("id", 1).property("id", 2)` | Error: duplicate id property |
| `addV("person")` (no id property) | `AddVStep(label="person", id=None)` — unchanged |

### Group 4 — `addE` + `from`/`to`/`rank` folding (`merge_adde_ids`)

| Input | Expected tree |
|-------|--------------|
| `addE("knows").from(1).to(2)` | `AddEStep(label="knows", from=Some(1), to=Some(2))` — no separate From/To steps |
| `addE("knows").from(1)` | `AddEStep(label="knows", from=Some(1), to=None)` — from only merged |
| `addE("knows").to(2).from(1)` | `AddEStep(label="knows", from=Some(1), to=Some(2))` — order-independent |
| `addE("knows").from(1).to(2).property("rank", 0)` | `AddEStep(label="knows", from=Some(1), to=Some(2), rank=Some(0))` — rank also folds |
| `addE("knows").property("weight", 0.5).from(1).to(2)` | `AddEStep(...)` — non-rank properties preserved, from/to still merged |

### Group 5 — branching operators (no folding — verify tree shape)

| Input | Expected tree |
|-------|--------------|
| `V([1]).union([__.out("knows"), __.in("knows")])` | `UnionStep` with two children labelled `branch 0` and `branch 1`, each an `InOutStep` |
| `V([1]).coalesce([__.out("knows"), __.addV("person")])` | `CoalesceStep` with two children |
| `V([1]).where(__.out("knows").hasId([2]))` | `WhereStep` with one child `InOutStep` → `HasIdStep` |
| `V([1]).not(__.out("knows"))` | `NotStep` with one child `InOutStep` |
| `V([1]).choose(eq(1), __.out("knows"), __.in("knows"))` | `ChooseStep` with true/false branch children |
| `V([1]).repeat(__.out("knows")).times(3)` | `RepeatStep(times=3)` with one child `InOutStep` |
| `V([1]).repeat(__.out("knows")).until(__.hasId([2]))` | `RepeatStep` with body child + until child |

### Group 6 — multi-hop and combined rules

| Input | Expected tree |
|-------|--------------|
| `V([]).hasId([1]).out("knows").hasLabel("person")` | `VStep(ids=[1])` → `InOutStep` → `HasLabelStep` — V+hasId folds; out+label intact |
| `V([]).hasId([1]).outE("knows").where(otherV().hasId([2])).inV().hasLabel("person")` | `VStep(ids=[1])` → `GetEStep(labels=["knows"], end_vertex_ids=[2])` → `OtherVStep` → `HasLabelStep` — V+hasId + edge+filter both fold |
| `V([]).has("id", 1).out("knows").out("knows").hasLabel("person")` | `VStep(ids=[1])` → `InOutStep` → `InOutStep` → `HasLabelStep` — has("id") folded; two out() intact |
| `V([]).hasId([1]).bothE("knows").where(otherV().hasId([2]).has("age", gt(30)))` | `VStep(ids=[1])` → `GetEStep(labels=["knows"], end_vertex_ids=[2])` → `EndVertexFilterStep` — hasId folds into GetE; property filter stays |

### Group 7 — regression: explain output is stable and complete

| Scenario | Assertion |
|----------|-----------|
| Empty plan (no steps) | Renders as `PhysicalPlan` with no children (or a single empty source) |
| Plan with 10+ linear steps | All steps appear, indentation is consistent |
| Nested branching (union inside repeat) | Tree depth is correct, branch labels don't collide |
| Step with no params | `StepName()` — empty parens, no trailing comma |
| Step with params | `StepName(key1=val1, key2=val2)` — key-value pairs, no quotes around strings unless they contain spaces |

### Relationship to existing optimizer tests

The existing optimizer tests at `src/planner/optimizer/*.rs` test that the
*logical* plan is transformed correctly — they assert on `LogicalStep` variants
after `apply_rules`.  The explain tests above test the *physical* end of the
pipeline: did the optimizer's logical transformation actually result in the
expected physical operator, with the expected params?

Some of the existing `builder::tests::debug_print` tests (which assert on the
Debug string) can be replaced by explain tests.  The ones that cannot
(specifically, tests that just check step name ordering inside branch bodies)
should be preserved or ported to assert on `ExplainNode` children directly,
bypassing the renderer.
