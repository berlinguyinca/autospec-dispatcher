# autospec-dispatcher

The **dispatch plane** for AutoSpec: it drains a backlog larger than capacity,
judges what came back, and repairs itself through the same review path it
enforces on everything else.

| Plane | System | Question it answers |
| --- | --- | --- |
| Control | [`autospec`](https://github.com/berlinguyinca/autospec) | What work should happen? |
| **Dispatch** | **`autospec-dispatcher`** | What runs next, and what did the result *mean*? |
| Execution | [`autospec-orchestrator`](https://github.com/berlinguyinca/autospec-orchestrator) | Where and how should it execute? |
| Inference | [InferWeave](https://github.com/InferWeave) | Where and how should inference execute? |

## Why this exists

Launching agents is the easy half. The hard half is judging what comes back,
and it is the half that has been done by a human.

From one real session, with 202 open issues and ~14 concurrent agents, these
judgements were all required and none of them were launching decisions:

- an agent exited `rc=0` with a **0-byte** output and no session directory. The pipeline recorded `NO-OUTPUT` — the *same* status a healthy agent gets when it correctly decides no change is needed. The work was silently lost.
- CI failed on four PRs with `architecture-fitness`. That gate **fails identically on `main`** (81 pre-existing occurrences), so it was overridable.
- CI also failed with `file-size-ratchet` on two of them — and that one was **caused by the PR** (`claim/mod.rs` grew 1955→2000 lines). Not overridable. Same red X, opposite meaning.
- two patches for unrelated features shared **94 KB of byte-identical diff** and collided on 47 of 49 files. Their features were ~750 lines each; the rest was inherited `cargo fmt` churn from an unpinned toolchain.
- three reviewers on four patches produced one 3/3 corroboration, two `CONCERNS`, and five `UNKNOWN` (reasoning consumed the token budget). Only the corroborated one was safe to merge.

A rule engine cannot make the second and third calls: both are a red X on the
same check name. Distinguishing them means asking whether `main` fails the same
way — and then deciding what that implies.

## The core separation

> **Deterministic checks decide pass/fail. The model decides what a failure
> means and what should happen next. The model never overrides a gate.**

This is not a stylistic preference. A model that can flip a failing gate to
passing is a model that can merge broken code by being confidently wrong once.

| layer | decided by | examples |
| --- | --- | --- |
| **Dispatch** | code | who is next, capacity, collisions, idempotence, dependency order |
| **Observe** | code | did it run at all, did gates pass, does `main` fail the same way |
| **Judge** | model | is this pre-existing or caused, retry on a different model, is this patch contaminated, do reviewers corroborate |
| **Act** | code, on the judgement | dispatch, re-dispatch, open a PR, merge, file an issue |

"Did it run at all" is deliberately in the deterministic layer: a 0-byte output
with no session directory is not a judgement call, and making it one was how the
failure stayed invisible.

## Self-repair, and its limits

When the dispatcher meets something it cannot handle, it files an issue and
dispatches an agent at **its own repository** — the loop that produced this
project.

Three rails, because a system that rewrites itself is exactly where "it seemed
fine" becomes expensive:

1. **Self-modification takes the same path as any other change.** A branch, a PR,
   independent review, CI. Never a direct write to its own running code.
2. **It may not weaken its own safety checks.** A change that removes a gate,
   raises a cap, or disables corroboration requires a human. The dispatcher does
   not get to widen its own authority.
3. **Absent judgement is never a pass.** An empty, malformed, or timed-out model
   response is `UNKNOWN`. It is never silently read as "no problems found".

## Status

Founding design. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the
judgement catalogue this is built from, and the open issues for the work.
