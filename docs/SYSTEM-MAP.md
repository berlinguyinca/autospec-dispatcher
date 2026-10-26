# How the pieces fit

Four planes, four questions. Nothing in one plane may answer another's question —
that rule is what keeps the boundaries honest, and every violation found so far
has been a duplication.

```mermaid
flowchart LR
    GH[("GitHub<br/>issues & PRs")]
    subgraph CTRL["Control — autospec"]
        C["<b>What work<br/>should happen?</b><br/>specs, review policy,<br/>agent topology"]
    end
    subgraph DISP["Dispatch — autospec-dispatcher"]
        D["<b>What runs next,<br/>and what did the<br/>result mean?</b><br/>queue, caps, judgement"]
    end
    subgraph EXEC["Execution — autospec-orchestrator"]
        E["<b>Where and how<br/>does it execute?</b><br/>worktrees, runtimes,<br/>cleanup"]
    end
    subgraph INFER["Inference — InferWeave"]
        I["<b>Where and how does<br/>inference execute?</b><br/>routing, GPUs, tokens"]
    end
    GH --> C --> D --> E --> I
    D -. "verdicts, PRs" .-> GH
    I -. "tokens/sec, capacity" .-> D
```

## One issue, end to end

The path a single issue takes. Note where the two kinds of decision live:
**code decides pass/fail, a model decides meaning.**

```mermaid
sequenceDiagram
    participant GH as GitHub
    participant DP as dispatcher
    participant OR as orchestrator
    participant WK as worker (Apptainer/Docker)
    participant IW as InferWeave

    DP->>GH: read ready issues (deps resolved)
    Note over DP: cap − in-flight = slots
    DP->>OR: TaskPacket{goal, criteria, roleSkill}
    OR->>OR: mirror → worktree → environment
    OR->>WK: provision (labelled, owned)
    WK->>IW: completions
    IW-->>WK: tokens
    WK-->>OR: diff + evidence
    OR->>OR: destroy by label
    OR-->>DP: patch, or AGENT-NEVER-STARTED
    DP->>DP: gates, then 3 reviewers
    DP->>GH: PR, or re-dispatch on another model
```

## The judgement split

This is the core of the dispatch plane. A model that could flip a failing gate
to passing could merge broken code by being confidently wrong once.

```mermaid
flowchart TD
    R[result returned] --> Z{"output empty<br/>or no session?"}
    Z -->|yes| NS["AGENT-NEVER-STARTED<br/><i>retry, different model</i>"]
    Z -->|no| G[run gates]
    G --> B{"does the merge base<br/>fail the same way?"}
    B -->|yes| PE["PRE-EXISTING<br/><i>overridable, with evidence</i>"]
    B -->|no| CB["CAUSED-BY-CHANGE<br/><i>never overridable</i>"]
    PE --> RV[3 independent reviewers]
    CB --> FIX[back to the queue<br/>with the constraint]
    RV --> Q{"corroborated<br/>across models?"}
    Q -->|"3 approve"| M[merge]
    Q -->|"2 concerns"| FIX
    Q -->|"1 approve + UNKNOWN"| NO[do not merge]

    classDef det fill:#1f6feb22,stroke:#1f6feb;
    classDef mod fill:#8957e522,stroke:#8957e5;
    class Z,G,B,PE,CB,Q det
    class RV mod
```

Blue is decided by code. Purple is the only step a model owns.

## Why UNKNOWN is its own state

Measured across three reviewers and four patches: **5 of 12 verdicts were
`UNKNOWN`**, every one `finish=length` — the model spent its whole token budget
reasoning and returned empty content.

```mermaid
flowchart LR
    RESP[model response] --> P{parses to a verdict?}
    P -->|yes| V["APPROVE / CONCERNS / REJECT"]
    P -->|"empty, truncated,<br/>no endpoint"| U["UNKNOWN"]
    U --> NP["never a pass"]
    U --> NF["never a fail"]
    U --> VIS["counted and shown<br/>in the PR record"]
```

Reading those as approvals would have merged unreviewed changes.

## Inference: registration and the request path

Workers register **upward**; nothing dials in. The registration deadlock that
kept the gateway at zero workers for hours is marked.

```mermaid
flowchart TD
    subgraph SL["Slurm — partition low"]
        W1["worker<br/>llama.cpp --alias qwen3.8-27b"]
        W2["worker"]
    end
    subgraph HI["Slurm — partition high"]
        GWY["gateway<br/>auth, routing, telemetry"]
    end
    AG["pi agents<br/>(CPU-only)"] -->|"/v1, client token"| GWY
    W1 -->|"register + heartbeat"| GWY
    W2 -->|register| GWY
    GWY -->|"relay, base URL"| W1

    X["<b>issue 47</b>: path rejected as invalid name (400);<br/>any other name rejected as mismatch (422).<br/>Fixed by --alias, so the server<br/>reports the canonical name."]
    X -.-> W1
```

Two things this diagram encodes that cost real time:

- the gateway **joins the request path onto the registered endpoint**, so registration must send the *base* URL — `…/v1` registered becomes `…/v1/v1/models` and 404s
- GPUs exist only in `low` (preemptible); `high` has none for this account, so agent and gateway jobs go to `high` and every GPU worker is evictable

## Repository map

```mermaid
flowchart TB
    AS["<b>autospec</b><br/>control"]
    AD["<b>autospec-dispatcher</b><br/>dispatch"]
    AO["<b>autospec-orchestrator</b><br/>execution"]
    IWF["<b>InferWeave</b> family<br/>inference"]
    AI["autospec-inferweave<br/><i>ownership crosswalk</i>"]

    AS --> AD --> AO --> IWF
    AI -.->|"decides component<br/>ownership"| IWF
    AI -.-> AS

    D1["executor_bridge.rs holds a<br/>2nd execution plane<br/>(autospec 3583)"]
    D1 -.-> AS
    D2["cluster bash is a<br/>3rd one — replace, don't port"]
    D2 -.-> AO
```

Component ownership across the program is decided in **`autospec-inferweave`**
(`docs/specs/2026-09-01-inferweave-integrated-program-crosswalk-design.md`), not
per-repo. Check it before building anything that touches inference, gateways,
dashboards or node protocol — three duplications have already come from not
doing so.

## Runtimes are interchangeable

The SIF is built **from the same Dockerfile**, so image content is identical
across runtimes and the conformance suite needs no per-runtime fixture.

```mermaid
flowchart LR
    DF["Dockerfile"] --> OCI["OCI image"]
    DF --> SIF["SIF (Apptainer)"]
    OCI --> RD["runtime-docker"]
    OCI --> RP["runtime-podman"]
    SIF --> RA["runtime-apptainer"]
    RD & RP & RA --> T["Runtime trait"]
    T --> CONF["shared conformance suite<br/><i>skips visibly when a<br/>runtime is absent</i>"]
```

Slurm has no Docker. A suite that *requires* a Docker daemon can never run where
this code actually executes, so tests select whatever runtime the host has and
skip — visibly — when none is present.
