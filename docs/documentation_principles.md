# Principles for User-Facing Documentation

Status: reference — apply when writing or reviewing `docs/guides/*.md` and any
per-binding README (e.g. `bindings/python/README.md`, `rocksgraph/README.md`).
Created: 2026-08-08, distilled from the `docs/guides/` split-and-review pass.

This is not a style guide. It's the set of failure modes we actually hit while
writing and reviewing the user guides, generalized so the next round of docs
work (or the next reviewer) doesn't have to rediscover them.

This file itself is meta-documentation for contributors, not a user guide —
it lives at `docs/documentation_principles.md`, not under `docs/guides/`
(which is synced verbatim to the public GitHub Wiki) or `docs/design/`
(which holds feature design proposals, not documentation process). See
`CONTRIBUTING.md` for the pointer contributors will actually find.

---

## 1. Say what users need, skip what they don't

Two questions to ask of every sentence:

- **Does the user need this to use the system correctly?** If removing it
  would leave the user unable to predict behavior, or would let them hit an
  avoidable error, state it plainly.
- **Is this explaining an internal mechanism whose benefit the user gets
  automatically, with nothing to decide?** If the user's code is identical
  whether or not they know this, cut it — narrating an invisible, automatic
  optimization ("the optimizer rewrites X into Y") is implementation detail,
  not a user-facing contract.

The line isn't "never mention internals" — it's "mention internals only when
they change what the user should write or expect." A note that a feature is
*conditional* ("this fast path only applies when unlabeled and unfiltered")
is actionable and stays; a walkthrough of *how* the fast path is implemented
internally doesn't.

Applies to: `rocksgraph`-internal vocabulary (storage engine internals,
execution-model internals) generally doesn't belong in user docs even when
true — describe the observable contract instead (e.g. "the graph is read
from an immutable snapshot," not "`ReadSession` pins a RocksDB sequence
number" — the latter also breaks the moment a non-RocksDB backend exists).

## 2. Correctness first — verify against source, not memory

Every code sample must compile/run against the *current* public API.
"Looks like Rust/Python I've seen elsewhere" is not verification.

- Grep the actual type/method/error-variant names in source before writing
  an example around them. Don't assume a plausible-sounding type exists —
  check.
- A design doc describing a feature is not proof the feature shipped. Cite
  design docs for rationale, never as the source of truth for "does this API
  exist" — check the source directly, and if a design doc's own "Status:"
  line claims something is shipped, verify that claim too (it can be stale
  or simply wrong).
- When one inaccuracy is found, grep the whole guide set for the same
  *shape* of problem before calling the fix done — the same wrong phrase or
  the same category of mistake tends to repeat across guides that were
  drafted together.

  *Point-in-time example (2026-08): `GValue`/`Vector` (Rust) and
  `TransactionConflict` were assumed to exist and didn't; the `BulkSource`
  trait, an in-memory backend, and `Graph.ephemeral()` all appeared only in
  design docs, never in shipped code. These specific names will inevitably
  go stale as the crate evolves — the check is the durable part, not the
  list.*

## 3. Pin claims to a version

State which version of the crate a guide describes — one line near the top
(e.g. `Target: RocksGraph v0.2.0+`). An example that's correct today can
silently stop being correct after the next release; without a stated target,
a user on an older or newer version has no way to tell whether a mismatch is
their mistake or the guide's. This costs one line and zero ongoing
maintenance beyond bumping it on a breaking-change release.

## 4. Cite errors exactly, not approximately

If a guide says "you'll get a conflict error," a user grepping their actual
output for that phrase won't find it if the real message is
`StoreError::Conflict` with no message body, or a differently-named type
entirely. Either quote the exact user-facing string/variant, or reference
the error by its canonical path (`StoreError::Conflict`, not "a conflict
error" or "`TransactionConflict`"). This is the same failure mode as
Principle 2 applied specifically to error text — verify the identifier
against source, don't reconstruct it from what sounds plausible.

## 5. Anti-patterns must be real anti-patterns

An anti-pattern is something that *appears to work* but is silently costly
or wrong. A hard, deterministic, immediately-thrown error is not an
anti-pattern — it's a validation error, and framing it as a ❌/✅ narrative
overstates the risk (the user finds out the first time they run the code,
not in production).

- Document validation/hard-error behavior compactly — trigger → exact error
  type → one line — placed near the relevant API description, not as a full
  anti-pattern writeup.
- Reserve the ❌/✅ anti-pattern treatment for patterns that run to
  completion but cost you later (unnecessary scans, N+1 calls, lock
  contention, unbounded memory).

## 6. Never advise a fix that doesn't actually work

If the "correct" alternative you're about to suggest doesn't hold up against
the real system (e.g. "look it up with `.has()`" when there's no secondary
property index, so `.has()` is a full scan, not a lookup), don't suggest it.
Giving impractical or misleading advice to avoid calling something an
unsolved limitation is worse than admitting the limitation.

- If there's a genuinely better solution, give the concrete one (not just
  "don't do this") — vague prohibitions without a real path forward are not
  actionable.
- If there's no built-in solution, say so honestly and give the best
  available workaround (e.g. "maintain this mapping yourself outside the
  system"), rather than implying a capability that isn't there.

## 7. Numbers must be sourced and comparable

Don't state a benchmark figure, or especially a *computed ratio* between two
figures, without checking the actual conditions behind each one.

- Verify dataset size, hardware, and methodology before citing a number.
- If two numbers come from different conditions (e.g. one measured on a
  1M-edge database, the other on 69M edges), say so explicitly and drop any
  implied fixed multiplier — state each number with its own context instead
  of dividing them into a single "Nx faster" claim.

## 8. Trade-offs get a decision framework, not a flat rule

When a piece of advice has a real trade-off depending on context (batch size
vs. conflict probability under OCC, memory vs. recall for HNSW parameters),
document the trade-off and the variable that decides it — not a single
"always do X" number. Ground the trade-off in the actual mechanism (e.g.
*why* a bigger transaction conflicts more) so a reader can reason about
their own edge cases instead of pattern-matching a rule that doesn't fit
their situation.

## 9. Examples should be runnable, or clearly marked as excerpts

A snippet that silently assumes setup it never shows (`snap.g().V(1)...`
with no `let graph = Graph::open(...); let mut snap = graph.read();` in
sight) creates a "works on my machine" gap — the reader can't tell whether
it's incomplete on purpose or something's missing. Either show the full
setup, or state once, up front, what context every snippet in the guide
assumes (e.g. "all examples below assume `graph`/`snap` as opened in
[Getting Started](guides/getting_started.md)") so the omission is a
documented convention, not a mystery.

## 10. Cross-reference instead of duplicating

If a concept is explained in depth in one guide (e.g. the OCC conflict
matrix in `concurrency_and_tx.md`), link to it from other guides that need
the same fact instead of re-explaining it inline. Keeps guides shorter and
prevents them drifting out of sync when the underlying behavior changes.

## 11. Keep parallel documents aligned

When the same product is documented more than once for different audiences
(per-language READMEs, a landing-page README vs. a deep-dive guide), audit
them side by side. They should share the same architecture description,
feature list, and terminology — divergent wording for the same fact (e.g.
"Concurrency & Transactions" in one place, "Transactions & Concurrency" in
another) is a sign they were edited independently and drifted.

## 12. One clear diagram beats two redundant ones

Don't carry two diagrams that say the same thing in a single document. Pick
the one diagram that fits the document's actual purpose (a landing-page hero
vs. an architecture deep-dive) and either cut the other or replace it with a
link to where that level of detail actually lives.

---

## Applying these during a review

A useful review pass over `docs/guides/` (or equivalent) checks, per guide:

1. Does every code sample match the current public API? (grep the source)
2. Does the guide state which version it targets?
3. Is every cited error identified by its exact canonical name/message, not
   a paraphrase?
4. Is every "don't do X" backed by a real, verified alternative?
5. Is every anti-pattern something that *runs* — not something rejected at
   build/validate time?
6. Is every number sourced, and are any two numbers being compared actually
   comparable?
7. Is there an explanation of an automatic/invisible behavior that could be
   cut without losing anything the user needs to act on?
8. Can a reader tell what context each snippet assumes, even if it isn't
   fully self-contained?
9. Do parallel documents (other languages, README vs. guide) still agree?

See the `docs/guides/` guides themselves, and their git history on this
branch, for worked examples of each of these being caught and fixed.
