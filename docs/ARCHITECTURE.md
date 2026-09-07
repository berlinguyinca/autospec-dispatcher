# The judgement catalogue

Every entry is a decision that was actually required in one session of running
~20 agents against 202 issues. They are the requirements: if the dispatcher
cannot make these calls, a human still has to.

Each says which layer owns it, because the split between "code decides" and
"model decides" is the whole design.

---

## 1. Did the agent run at all?

**Layer: deterministic. Not a judgement.**

Observed: an agent exited `COMPLETED 0:0` after 35 minutes having written a
**0-byte** `agent.out`, with **no pi session directory** created. The pipeline
recorded:

```
status=NO-OUTPUT
agent_rc=0
changed_files=0
```

which is byte-identical to what a healthy agent produces when it reads an issue
and correctly concludes no change is needed. A sibling issue in the same batch
had that exact status with a 3,428-byte `agent.out` — same label, opposite
meaning.

**Rule:** empty output, or a missing session directory, is `AGENT-NEVER-STARTED`.
It is a hard failure, it is greppable, and it auto-retries on a **different
model**, because a never-started agent has produced nothing to conflict with.

`agent_rc=0` means the process exited zero. It is not evidence that work
happened.

---

## 2. Is this CI failure ours, or was it already broken?

**Layer: observation is deterministic, the consequence is a judgement.**

Four PRs, all failing `architecture-fitness`:

```
FAIL financial_no_f64 metric=forbidden_f64_occurrences observed=81 threshold=0
```

`main` fails identically. 81 pre-existing occurrences, untouched by any of the
four. Overridable, with the override stated in writing on the PR.

Two of the same four also failed `file-size-ratchet`:

```
FILE_SIZE:crates/autospec-core/src/claim/mod.rs: grew from 1955 to 2000 lines
FILE_SIZE:crates/autospec-cli/tests/cli_commands.rs: grew from 5731 to 5732 lines
```

**Caused by the PR.** Not overridable — not even the one-line case.

Both appear as a red X on a named check. The discriminator is mechanical —
*does the base fail the same way?* — so the dispatcher must always run it before
asking a model anything. The model's job begins after that fact is in hand.

---

## 3. Is this patch actually about its issue?

**Layer: deterministic detection, model explanation.**

Two patches, for unrelated issues, each ~121 KB across 49 files, overlapping on
**47**. Removing each one's own new module left ~94 KB that was **byte-identical
between them**.

Cause: no `rust-toolchain.toml`, no `rustfmt.toml`, CI floating on `@stable`, and
three live rustfmt versions. `main` was fmt-dirty in 52 files, so every agent's
`cargo fmt` rewrote the same 46 unrelated files into its own patch. The real
features were ~750 lines each.

**Signals, all cheap:** a patch touching files the issue never names; two patches
sharing a large identical diff; a patch whose file set is mostly files the base
already fails `fmt --check` on.

A patch that is 78% inherited churn must not reach a reviewer as if it were the
change.

---

## 4. Do the reviewers agree?

**Layer: model, then deterministic aggregation.**

Three reviewers, three different models, four patches:

| issue | GLM-Q6 | Flash-Next | Qwen-27B-Q8 | outcome |
| --- | --- | --- | --- | --- |
| A | APPROVE | APPROVE | APPROVE | merged |
| B | CONCERNS | CONCERNS | UNKNOWN | corroborated concerns |
| C | APPROVE | UNKNOWN | UNKNOWN | not merged |
| D | CONCERNS | UNKNOWN | UNKNOWN | not merged |

Five of twelve verdicts were `UNKNOWN`, all `finish=length` — reasoning consumed
the token budget and returned empty content.

**Rules:**
- corroboration across **independent models** is the bar; a single approval is not
- `UNKNOWN` is never a pass, and never a fail — it is absent judgement, and it must be visible in the tally
- a reviewer must never review a change implemented by the same model instance

Run-to-run variance on identical inputs has been large enough (35 vs 77
citations on one config) that a single reviewer's findings are not reproducible.
Corroboration is the only signal that has held.

---

## 5. Is the verification still true?

**Layer: deterministic.**

Patches carried `status=VERIFIED` from gates that ran against a base **13 commits
old**. That is a stale measurement, not a current one.

**Rule:** verification has a base SHA and an age. If the base has moved, the
gates re-run — on the PR, where CI is the live measurement. A stale `VERIFIED`
is reported as stale, never inherited.

---

## 6. Should these two run at the same time?

**Layer: deterministic prediction, model arbitration on ties.**

Wide changes collide. A 22,000-line extraction cannot share a repository with
sixteen concurrent agents, and two patches that both rewrite the same 46 files
cannot both merge.

The dispatcher must predict overlap before dispatching, and hold an exclusive
lock for changes whose blast radius is the tree. Ordering is dependency-aware:
never dispatch an issue with an unresolved blocker, treat one blocked only by
**closed** issues as ready, and report a dependency **cycle** as an error —
a cycle leaves the ready set empty, which is indistinguishable from a drained
queue.

---

## 7. Is the loop still producing anything?

**Layer: deterministic.**

The stop rule is *queue empty*. There is no failure-rate circuit breaker, which
puts the weight on visibility: every run reports what it dispatched, and what it
**skipped and why** — blocked, in flight, already patched, no worker available.

The operator's kill switch is stopping the schedule. That only works if a
systemic failure is loud before it consumes the backlog.
