//! Self-repair: the dispatcher meets something it cannot handle, files an
//! issue with the evidence, and dispatches a repair agent at **its own
//! repository** — the loop that produced this project.
//!
//! A system that rewrites itself is where "it seemed fine" becomes expensive,
//! so the rails are part of the feature:
//!
//! 1. a self-targeted change takes the same path as any other change — branch,
//!    PR, independent review, CI — and is **never auto-merged**, whatever the
//!    reviewers conclude ([`SelfRepair::route`], [`MergeRoute`]);
//! 2. a change that touches a gate, a cap, corroboration or merge authority is
//!    **refused and escalated**, not reviewed ([`SafetySurface`]);
//! 3. a running instance never executes its own uncommitted changes — the plan
//!    is cut from the commit it was built from, and an uncommitted self-change
//!    is refused ([`ChangeSource::WorkingTree`]).
//!
//! Plus the loop guard: repeated attempts at the same condition stop and
//! escalate instead of burning the backlog ([`LoopGuard`]).

use std::collections::HashMap;

/// Label on every issue, branch and PR the dispatcher opens against its own
/// repository. Self-modification has to be identifiable at a glance by whoever
/// reviews it.
pub const SELF_CHANGE_LABEL: &str = "self-change";

/// Label marking something that cannot move without a person.
pub const NEEDS_HUMAN_LABEL: &str = "needs-human";

/// Label on an issue filed straight from an unhandled condition.
pub const UNHANDLED_LABEL: &str = "unhandled";

/// How many times a condition may be self-repaired before the loop guard
/// stops. The third sighting of the same condition is a systemic failure, not
/// a repair, and retrying it consumes the backlog (see the judgement
/// catalogue, "Is the loop still producing anything?").
pub const DEFAULT_SELF_REPAIR_ATTEMPTS: usize = 2;

/// A condition the dispatcher met and could not handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnhandledCondition {
    /// Stable identity of the condition. Two reports of the same condition
    /// must agree on it, otherwise the loop guard cannot see the repetition
    /// and the guard degrades into a no-op.
    pub key: String,
    /// One line, in terms of what happened rather than what was tried.
    pub summary: String,
    /// The observation verbatim — log lines, statuses, SHAs. The evidence
    /// travels with the issue so a human can decide without re-running it.
    pub evidence: Vec<String>,
}

impl UnhandledCondition {
    pub fn new(key: impl Into<String>, summary: impl Into<String>, evidence: Vec<String>) -> Self {
        Self {
            key: key.into(),
            summary: summary.into(),
            evidence,
        }
    }
}

/// The issue filed for an unhandled condition, with its evidence attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueDraft {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
}

/// What the dispatcher does about an unhandled condition: the issue it filed
/// and the repair agent it dispatched against this repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchPlan {
    /// The dispatcher's own repository — that is what makes this self-repair.
    pub repo: String,
    /// The commit the running instance was built from. The agent works from
    /// this committed base, never from the running instance's working tree.
    pub base_sha: String,
    pub issue: IssueDraft,
}

/// Where a proposed change came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeSource {
    /// A commit on a branch — reviewable, and what CI runs against.
    Branch,
    /// Uncommitted edits in a running instance's working tree. Never executed
    /// by that instance: the version under review and the version running stay
    /// distinct until a human-visible merge.
    WorkingTree,
}

/// A change proposed against some repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedChange {
    /// Repository the change targets. Equality with [`SelfRepair::repo`] is
    /// what makes it self-targeted; there is no other signal, so it is not
    /// inferred from branch names.
    pub repo: String,
    /// The condition this change was dispatched to fix, for the escalation.
    pub condition_key: String,
    pub source: ChangeSource,
    /// Paths the change touches.
    pub changed_paths: Vec<String>,
    /// Unified diff, scanned line by line for edits to the rails.
    pub diff: String,
    /// Why the agent wanted this change, verbatim from its issue or PR body,
    /// so the escalation is actionable without opening the diff.
    pub rationale: String,
}

impl ProposedChange {
    /// A committed change on a branch.
    pub fn on_branch(
        repo: impl Into<String>,
        condition_key: impl Into<String>,
        changed_paths: Vec<String>,
        diff: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            repo: repo.into(),
            condition_key: condition_key.into(),
            source: ChangeSource::Branch,
            changed_paths,
            diff: diff.into(),
            rationale: rationale.into(),
        }
    }
}

/// A safety property the dispatcher is not allowed to lower about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rail {
    /// A deterministic pass/fail check.
    Gate,
    /// A numeric bound: caps, thresholds, panel sizes.
    Cap,
    /// The requirement that independent models agree before anything merges.
    Corroboration,
    /// What may merge, and who gets to say so.
    MergeAuthority,
}

impl Rail {
    pub fn label(self) -> &'static str {
        match self {
            Rail::Gate => "GATE",
            Rail::Cap => "CAP",
            Rail::Corroboration => "CORROBORATION",
            Rail::MergeAuthority => "MERGE_AUTHORITY",
        }
    }
}

impl std::fmt::Display for Rail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// What a rule matches against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    /// A substring of a changed file path, case-insensitive.
    Path(String),
    /// A substring of a changed diff line (an added or removed line),
    /// case-insensitive. Catches a rename of a protected constant to a file
    /// the path rules never named.
    DiffLine(String),
}

/// One protected surface: what counts as touching a rail, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceRule {
    pub matcher: Matcher,
    pub rail: Rail,
    /// Printed into the escalation. A human should be able to agree or
    /// disagree with it without reading code.
    pub why: String,
}

impl SurfaceRule {
    fn path(pattern: &str, rail: Rail, why: &str) -> Self {
        Self {
            matcher: Matcher::Path(pattern.to_owned()),
            rail,
            why: why.to_owned(),
        }
    }

    fn token(token: &str, rail: Rail, why: &str) -> Self {
        Self {
            matcher: Matcher::DiffLine(token.to_owned()),
            rail,
            why: why.to_owned(),
        }
    }
}

/// A rail a change touched, as detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RailTouch {
    pub rail: Rail,
    /// What matched: the changed path, or the changed diff line.
    pub matched: String,
    /// Why that is a rail, from the rule.
    pub why: String,
}

impl std::fmt::Display for RailTouch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {} — {}", self.rail.label(), self.matched, self.why)
    }
}

/// The surfaces a self-change may not adjust on its own.
///
/// Matching is deliberately broad: a false positive costs a human a look at a
/// diff, a false negative costs the rails. Detection is mechanical — no model
/// decides whether its own authority is being widened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetySurface {
    pub rules: Vec<SurfaceRule>,
}

impl Default for SafetySurface {
    fn default() -> Self {
        Self {
            rules: vec![
                SurfaceRule::path(
                    "self_repair",
                    Rail::MergeAuthority,
                    "the self-repair rails are defined here; editing them is editing what the dispatcher may merge",
                ),
                SurfaceRule::path(
                    "self-repair",
                    Rail::MergeAuthority,
                    "the self-repair rails are defined here; editing them is editing what the dispatcher may merge",
                ),
                SurfaceRule::path(
                    "corroboration",
                    Rail::Corroboration,
                    "the bar that independent reviewers must agree",
                ),
                SurfaceRule::path(
                    "review",
                    Rail::Corroboration,
                    "the review panel and how its verdicts are counted",
                ),
                SurfaceRule::path(
                    "gate",
                    Rail::Gate,
                    "a deterministic pass/fail gate",
                ),
                SurfaceRule::path(
                    "capacity",
                    Rail::Cap,
                    "a concurrency or retry cap",
                ),
                SurfaceRule::token(
                    "MIN_",
                    Rail::Cap,
                    "a minimum threshold, e.g. the approvals required to merge",
                ),
                SurfaceRule::token(
                    "MAX_",
                    Rail::Cap,
                    "a maximum bound, e.g. attempts or blast radius",
                ),
                SurfaceRule::token(
                    "DEFAULT_REVIEWERS",
                    Rail::Corroboration,
                    "the size of the independent review panel",
                ),
                SurfaceRule::token(
                    "is_mergeable",
                    Rail::MergeAuthority,
                    "the predicate that decides whether a change may merge",
                ),
                SurfaceRule::token(
                    "auto_merge",
                    Rail::MergeAuthority,
                    "the exemption of a change from human merge",
                ),
                SurfaceRule::token(
                    "needs-human",
                    Rail::MergeAuthority,
                    "the label that forces a person into the path",
                ),
            ],
        }
    }
}

impl SafetySurface {
    pub fn new(rules: Vec<SurfaceRule>) -> Self {
        Self { rules }
    }

    /// Every rule the change trips, in rule order, deduplicated.
    pub fn detect(&self, change: &ProposedChange) -> Vec<RailTouch> {
        let mut touches: Vec<RailTouch> = Vec::new();
        let changed_lines = changed_diff_lines(&change.diff);
        for rule in &self.rules {
            match &rule.matcher {
                Matcher::Path(pattern) => {
                    let pattern = pattern.to_lowercase();
                    for path in &change.changed_paths {
                        if path.to_lowercase().contains(&pattern) {
                            push_unique(
                                &mut touches,
                                RailTouch {
                                    rail: rule.rail,
                                    matched: path.clone(),
                                    why: rule.why.clone(),
                                },
                            );
                        }
                    }
                }
                Matcher::DiffLine(token) => {
                    let token = token.to_lowercase();
                    for line in &changed_lines {
                        if line.to_lowercase().contains(&token) {
                            push_unique(
                                &mut touches,
                                RailTouch {
                                    rail: rule.rail,
                                    matched: (*line).to_owned(),
                                    why: rule.why.clone(),
                                },
                            );
                        }
                    }
                }
            }
        }
        touches
    }
}

fn push_unique(touches: &mut Vec<RailTouch>, touch: RailTouch) {
    if !touches.contains(&touch) {
        touches.push(touch);
    }
}

/// The added and removed lines of a unified diff, excluding the `+++`/`---`
/// file headers. Context lines are not changes and are not scanned.
fn changed_diff_lines(diff: &str) -> Vec<&str> {
    diff.lines()
        .filter(|line| {
            !line.starts_with("+++")
                && !line.starts_with("---")
                && (line.starts_with('+') || line.starts_with('-'))
        })
        .collect()
}

/// Why something stopped and went to a human.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationReason {
    /// The change edits the rails, a cap, or merge authority.
    SafetyRails { rails: Vec<Rail> },
    /// It would have executed code the running instance has not committed.
    UncommittedSelfChange,
    /// The same condition has been self-repaired too many times.
    AttemptsExhausted { limit: usize },
}

impl std::fmt::Display for EscalationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EscalationReason::SafetyRails { rails } => {
                let rails: Vec<&str> = rails.iter().map(|r| r.label()).collect();
                write!(f, "it edits its own safety checks: {}", rails.join(", "))
            }
            EscalationReason::UncommittedSelfChange => write!(
                f,
                "it is uncommitted code in the running instance's working tree"
            ),
            EscalationReason::AttemptsExhausted { limit } => {
                write!(f, "the self-repair attempt limit ({limit}) is exhausted")
            }
        }
    }
}

/// A stop that names what the dispatcher wanted to change and why, so a human
/// can act on it without reconstructing the intent from the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escalation {
    pub reason: EscalationReason,
    /// The condition the change was dispatched for.
    pub condition_key: String,
    /// One line on what happened.
    pub summary: String,
    /// What it wanted to change — paths, or the action it wanted to take.
    pub wanted: Vec<String>,
    /// Why it wanted to, in the agent's own words.
    pub rationale: String,
    pub evidence: Vec<String>,
    /// How many self-repair attempts have been made at this condition.
    pub attempts: usize,
}

impl Escalation {
    /// Labels for the issue this escalation is filed under.
    pub fn labels(&self) -> Vec<String> {
        vec![SELF_CHANGE_LABEL.to_owned(), NEEDS_HUMAN_LABEL.to_owned()]
    }

    /// The human-facing report: the refusal, what was wanted, why, and the
    /// evidence. Written so it can be pasted into an issue and acted on.
    pub fn report(&self) -> String {
        let mut out = String::new();
        out.push_str("ESCALATED TO A HUMAN — not applied, not merged\n\n");
        out.push_str(&format!("reason: {}\n", self.reason));
        out.push_str(&format!("condition: {}\n", self.condition_key));
        out.push_str(&format!("what happened: {}\n", self.summary));
        out.push_str(&format!(
            "self-repair attempts at this condition: {}\n",
            self.attempts
        ));
        out.push_str("\nwhat it wanted to change:\n");
        for wanted in &self.wanted {
            out.push_str(&format!("  - {wanted}\n"));
        }
        out.push_str(&format!("\nwhy it wanted to: {}\n", self.rationale));
        if !self.evidence.is_empty() {
            out.push_str("\nevidence:\n");
            for line in &self.evidence {
                out.push_str(&format!("  - {line}\n"));
            }
        }
        out.push_str(
            "\naction required: a human decides. The dispatcher does not widen \
             its own authority, and does not retry this condition.\n",
        );
        out
    }
}

/// Whether a condition may still be self-repaired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// Under the limit; go ahead.
    Allow,
    /// At or past the limit. Stop, escalate, leave the condition visible.
    Stop { limit: usize, attempts: usize },
}

/// Counts self-repair attempts per condition.
///
/// The loop guard exists because a repair that fixes nothing is invisible:
/// the condition recurs, another agent is dispatched, and the backlog is
/// consumed. Stopping is the only signal that repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopGuard {
    pub limit: usize,
    attempts: HashMap<String, usize>,
}

impl Default for LoopGuard {
    fn default() -> Self {
        Self::new(DEFAULT_SELF_REPAIR_ATTEMPTS)
    }
}

impl LoopGuard {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            attempts: HashMap::new(),
        }
    }

    /// Attempts so far for a condition, without counting a new one.
    pub fn attempts(&self, key: &str) -> usize {
        self.attempts.get(key).copied().unwrap_or(0)
    }

    /// Record one attempt and say whether the loop may continue.
    pub fn note(&mut self, key: &str) -> Guard {
        let attempts = self.attempts.entry(key.to_owned()).or_insert(0);
        *attempts += 1;
        if *attempts > self.limit {
            Guard::Stop {
                limit: self.limit,
                attempts: *attempts,
            }
        } else {
            Guard::Allow
        }
    }
}

/// What applies to a proposed change, given where it points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeRoute {
    /// Not aimed at this repository. The ordinary gates apply, including the
    /// corroboration bar.
    Ordinary,
    /// Aimed at this repository: branch, PR, independent review, CI — exactly
    /// like everyone else, and then a human merges. No reviewer verdict
    /// changes that; the review path is a floor, not a substitute.
    HumanMergeRequired { reason: String },
    /// Refused before review: it edits the rails, or it would run code that is
    /// not committed. Escalated as written.
    Refused {
        touches: Vec<RailTouch>,
        escalation: Escalation,
    },
}

impl MergeRoute {
    pub fn is_refused(&self) -> bool {
        matches!(self, MergeRoute::Refused { .. })
    }

    /// Whether the change may merge on machine verdicts alone.
    ///
    /// `corroborated` is the output of the review gate (#4): did independent
    /// reviewers agree. For a self-targeted change the answer is always no,
    /// whatever that gate returned — which is the point: the dispatcher cannot
    /// merge its own code because three models said yes.
    pub fn permits_auto_merge(&self, corroborated: bool) -> bool {
        match self {
            MergeRoute::Ordinary => corroborated,
            MergeRoute::HumanMergeRequired { .. } | MergeRoute::Refused { .. } => false,
        }
    }

    /// Labels this route requires on the PR.
    pub fn labels(&self) -> Vec<String> {
        match self {
            MergeRoute::Ordinary => Vec::new(),
            MergeRoute::HumanMergeRequired { .. } | MergeRoute::Refused { .. } => {
                vec![SELF_CHANGE_LABEL.to_owned(), NEEDS_HUMAN_LABEL.to_owned()]
            }
        }
    }
}

/// What the dispatcher does about an unhandled condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Filed with the evidence, and dispatched against this repository.
    Dispatch(DispatchPlan),
    /// Not filed and not dispatched: the loop guard stopped it and a human
    /// has to look. The earlier issue for the condition already carries the
    /// evidence, so nothing is lost by not filing a duplicate.
    Escalate(Escalation),
}

/// The self-repair loop, with its rails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfRepair {
    /// Slug of the repository this dispatcher runs against. A change is
    /// self-targeted iff it names this repository.
    pub repo: String,
    /// Commit the running instance was built from, and the only base a repair
    /// agent may work from.
    pub running_base: String,
    pub surface: SafetySurface,
    pub loop_guard: LoopGuard,
}

impl SelfRepair {
    pub fn new(repo: impl Into<String>, running_base: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            running_base: running_base.into(),
            surface: SafetySurface::default(),
            loop_guard: LoopGuard::default(),
        }
    }

    /// Whether a change points at this dispatcher's own repository.
    pub fn is_self_targeted(&self, change: &ProposedChange) -> bool {
        change.repo == self.repo
    }

    /// Handle an unhandled condition: file the evidence as an issue and
    /// dispatch a repair agent here — unless the loop guard has run out.
    pub fn handle(&mut self, condition: &UnhandledCondition) -> Decision {
        let attempts = match self.loop_guard.note(&condition.key) {
            Guard::Allow => self.loop_guard.attempts(&condition.key),
            Guard::Stop { limit, attempts } => {
                return Decision::Escalate(Escalation {
                    reason: EscalationReason::AttemptsExhausted { limit },
                    condition_key: condition.key.clone(),
                    summary: format!(
                        "stopped self-repairing this condition after {attempts} attempts; \
                         it is still unhandled"
                    ),
                    wanted: vec![format!(
                        "dispatch a further self-repair agent at {} for {}",
                        self.repo, condition.key
                    )],
                    rationale: condition.summary.clone(),
                    evidence: condition.evidence.clone(),
                    attempts,
                });
            }
        };

        Decision::Dispatch(DispatchPlan {
            repo: self.repo.clone(),
            base_sha: self.running_base.clone(),
            issue: IssueDraft {
                title: format!("unhandled: {}", condition.summary),
                body: self.issue_body(condition, attempts),
                labels: vec![SELF_CHANGE_LABEL.to_owned(), UNHANDLED_LABEL.to_owned()],
            },
        })
    }

    fn issue_body(&self, condition: &UnhandledCondition, attempts: usize) -> String {
        let mut out = String::new();
        out.push_str(
            "The dispatcher met a condition it cannot handle. It is filing this \
                      against its own repository and dispatching a repair agent here.\n\n",
        );
        out.push_str(&format!("condition: {}\n", condition.key));
        out.push_str(&format!("attempt: {attempts}\n\n"));
        out.push_str("evidence:\n");
        for line in &condition.evidence {
            out.push_str(&format!("- {line}\n"));
        }
        out.push_str(
            "\nThis is a self-targeted change: branch, PR, independent review and CI apply, \
             and it is never auto-merged. If it touches a gate, a cap, corroboration or merge \
             authority it is refused and escalated to a human instead.\n",
        );
        out
    }

    /// Decide what applies to a proposed change. Called before the change is
    /// executed or merged; a refused change is never handed to a reviewer,
    /// because review is the mechanism the change would be using.
    pub fn route(&self, change: &ProposedChange) -> MergeRoute {
        if !self.is_self_targeted(change) {
            return MergeRoute::Ordinary;
        }

        // Rail 3: the version under review and the version running are
        // distinct until a human-visible merge.
        if change.source == ChangeSource::WorkingTree {
            return MergeRoute::Refused {
                touches: Vec::new(),
                escalation: self.escalation(
                    change,
                    EscalationReason::UncommittedSelfChange,
                    format!(
                        "it would have run uncommitted changes from the working tree of the \
                         instance built from {}",
                        self.running_base
                    ),
                    change.changed_paths.clone(),
                ),
            };
        }

        let touches = self.surface.detect(change);
        if touches.is_empty() {
            return MergeRoute::HumanMergeRequired {
                reason: "self-targeted change: branch, PR, independent review and CI, and a \
                         human merges regardless of reviewer verdicts"
                    .to_owned(),
            };
        }

        let mut rails: Vec<Rail> = Vec::new();
        for touch in &touches {
            if !rails.contains(&touch.rail) {
                rails.push(touch.rail);
            }
        }
        // What it wanted to change, for a human triaging: the touched paths
        // first, then the specific lines that tripped a rule.
        let mut wanted: Vec<String> = change.changed_paths.clone();
        for touch in &touches {
            if !wanted.contains(&touch.matched) {
                wanted.push(touch.matched.clone());
            }
        }
        MergeRoute::Refused {
            escalation: self.escalation(
                change,
                EscalationReason::SafetyRails { rails },
                "it edits a gate, a cap, corroboration or merge authority; the dispatcher does \
                 not widen its own authority"
                    .to_owned(),
                wanted,
            ),
            touches,
        }
    }

    fn escalation(
        &self,
        change: &ProposedChange,
        reason: EscalationReason,
        summary: String,
        wanted: Vec<String>,
    ) -> Escalation {
        Escalation {
            reason,
            condition_key: change.condition_key.clone(),
            summary,
            wanted,
            rationale: change.rationale.clone(),
            evidence: vec![
                format!("repo: {}", change.repo),
                format!("source: {:?}", change.source),
            ],
            attempts: self.loop_guard.attempts(&change.condition_key),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: &str = "berlinguyinca/autospec-dispatcher";
    const OTHER: &str = "berlinguyinca/autospec";

    fn dispatcher() -> SelfRepair {
        SelfRepair::new(OWN, "deadbeef")
    }

    fn condition() -> UnhandledCondition {
        UnhandledCondition::new(
            "judge/no-endpoint",
            "the judge endpoint was unreachable and the run was recorded as OK",
            vec![
                "judge request failed: connection refused (127.0.0.1:8123)".to_owned(),
                "status=NO-OUTPUT agent_rc=0 changed_files=0".to_owned(),
            ],
        )
    }

    fn own_change(paths: &[&str], diff: &str) -> ProposedChange {
        ProposedChange::on_branch(
            OWN,
            "judge/no-endpoint",
            paths.iter().map(|p| (*p).to_owned()).collect(),
            diff,
            "treat a refused connection as UNKNOWN instead of swallowing it",
        )
    }

    // --- unhandled condition -> issue + dispatch at this repository ------

    #[test]
    fn unhandled_condition_is_filed_with_its_evidence() {
        let mut sr = dispatcher();
        let Decision::Dispatch(plan) = sr.handle(&condition()) else {
            panic!("the first sighting of a condition should dispatch");
        };
        assert!(plan.issue.body.contains("connection refused"));
        assert!(plan.issue.body.contains("status=NO-OUTPUT agent_rc=0"));
        assert!(plan.issue.title.contains("unhandled:"));
    }

    #[test]
    fn self_repair_is_dispatched_against_this_repository() {
        let mut sr = dispatcher();
        let Decision::Dispatch(plan) = sr.handle(&condition()) else {
            panic!("expected a dispatch");
        };
        assert_eq!(plan.repo, OWN);
        assert_eq!(plan.base_sha, "deadbeef");
        assert!(plan.issue.labels.contains(&SELF_CHANGE_LABEL.to_owned()));
    }

    // --- rail 1: same path as everyone else, never auto-merged -----------

    #[test]
    fn a_self_change_is_labelled_as_self_change() {
        let sr = dispatcher();
        let route = sr.route(&own_change(&["crates/dispatcher-core/src/judge.rs"], ""));
        assert!(!route.is_refused());
        match &route {
            MergeRoute::HumanMergeRequired { reason } => {
                assert!(reason.contains("human merges"), "{reason}");
                assert!(reason.contains("review"), "{reason}");
            }
            other => panic!("expected a human-merge route, got {other:?}"),
        }
        assert!(route.labels().contains(&SELF_CHANGE_LABEL.to_owned()));
        assert!(route.labels().contains(&NEEDS_HUMAN_LABEL.to_owned()));
    }

    #[test]
    fn a_self_change_never_auto_merges_even_at_full_corroboration() {
        let sr = dispatcher();
        let route = sr.route(&own_change(&["crates/dispatcher-core/src/judge.rs"], ""));
        // Three independent approvals, a green CI, a 3/3 panel: still no.
        assert!(!route.permits_auto_merge(true));
    }

    #[test]
    fn a_change_elsewhere_still_follows_the_ordinary_gate() {
        // The contrast: the rails are about this repository, not a blanket
        // ban that would also stop ordinary work from merging.
        let sr = dispatcher();
        let change = ProposedChange::on_branch(
            OTHER,
            "judge/no-endpoint",
            vec!["crates/autospec-core/src/claim/mod.rs".to_owned()],
            "",
            "unchanged",
        );
        let route = sr.route(&change);
        assert_eq!(route, MergeRoute::Ordinary);
        assert!(route.permits_auto_merge(true));
        assert!(!route.permits_auto_merge(false));
        assert!(route.labels().is_empty());
    }

    // --- rail 2: it may not weaken its own safety checks -----------------

    #[test]
    fn removing_a_gate_is_refused() {
        let sr = dispatcher();
        let route = sr.route(&own_change(
            &["crates/autospec-dispatcher/src/quality_gate.rs"],
            "-fn check_gate() -> bool { true }\n",
        ));
        assert!(route.is_refused());
        let rails = rails_of(&route);
        assert!(rails.contains(&Rail::Gate), "{rails:?}");
    }

    #[test]
    fn raising_a_cap_is_refused() {
        let sr = dispatcher();
        let route = sr.route(&own_change(
            &["crates/dispatcher-core/src/judge.rs"],
            "-const MIN_CORROBORATING_APPROVALS: usize = 2;\n\
             +const MIN_CORROBORATING_APPROVALS: usize = 1;\n",
        ));
        assert!(route.is_refused());
        assert!(rails_of(&route).contains(&Rail::Cap));
    }

    #[test]
    fn shrinking_the_review_panel_is_corroboration() {
        let sr = dispatcher();
        let route = sr.route(&own_change(
            &["crates/dispatcher-core/src/judge.rs"],
            "-pub const DEFAULT_REVIEWERS: usize = 3;\n\
             +pub const DEFAULT_REVIEWERS: usize = 1;\n",
        ));
        assert!(rails_of(&route).contains(&Rail::Corroboration));
    }

    #[test]
    fn editing_the_merge_predicate_is_merge_authority() {
        let sr = dispatcher();
        let route = sr.route(&own_change(
            &["crates/dispatcher-core/src/judge.rs"],
            "-        if !self.is_mergeable() {\n+        if false {\n",
        ));
        assert!(rails_of(&route).contains(&Rail::MergeAuthority));
    }

    #[test]
    fn editing_the_self_repair_module_itself_is_refused() {
        let sr = dispatcher();
        let route = sr.route(&own_change(
            &["crates/dispatcher-core/src/self_repair.rs"],
            " // nothing scanned, only the file matters\n",
        ));
        assert!(route.is_refused());
        assert!(rails_of(&route).contains(&Rail::MergeAuthority));
    }

    #[test]
    fn an_ordinary_self_change_is_not_refused() {
        // The rails must not become a blanket veto: an ordinary fix to the
        // judging code is human-merged, not refused.
        let sr = dispatcher();
        let change = own_change(
            &["crates/dispatcher-core/src/judge.rs"],
            "+        let verdict = Verdict::Unknown {\n+            reason: \"connection refused\".to_owned(),\n+        };\n",
        );
        assert!(sr.surface.detect(&change).is_empty());
        assert!(!sr.route(&change).is_refused());
    }

    fn rails_of(route: &MergeRoute) -> Vec<Rail> {
        match route {
            MergeRoute::Refused { escalation, .. } => match &escalation.reason {
                EscalationReason::SafetyRails { rails } => rails.clone(),
                other => panic!("expected a rail-touch escalation, got {other:?}"),
            },
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    // --- the escalation is actionable ------------------------------------

    #[test]
    fn the_escalation_names_what_it_wanted_to_change_and_why() {
        let sr = dispatcher();
        let route = sr.route(&own_change(
            &["crates/dispatcher-core/src/judge.rs"],
            "-const MIN_CORROBORATING_APPROVALS: usize = 2;\n\
             +const MIN_CORROBORATING_APPROVALS: usize = 1;\n",
        ));
        let MergeRoute::Refused { escalation, .. } = route else {
            panic!("expected a refusal");
        };
        let report = escalation.report();
        // what it wanted
        assert!(report.contains("MIN_CORROBORATING_APPROVALS"), "{report}");
        assert!(
            report.contains("crates/dispatcher-core/src/judge.rs"),
            "{report}"
        );
        // why it wanted to, in its own words
        assert!(
            report.contains("treat a refused connection as UNKNOWN"),
            "{report}"
        );
        // why it was refused, and who decides now
        assert!(report.contains("CAP"), "{report}");
        assert!(
            report.contains("does not widen its own authority"),
            "{report}"
        );
        assert!(report.contains("human"), "{report}");
        assert_eq!(
            escalation.labels(),
            vec![SELF_CHANGE_LABEL.to_owned(), NEEDS_HUMAN_LABEL.to_owned()]
        );
    }

    // --- rail 3: never executes its own uncommitted changes --------------

    #[test]
    fn an_uncommitted_self_change_is_refused() {
        let mut sr = dispatcher();
        sr.handle(&condition());
        let mut change = own_change(&["crates/dispatcher-core/src/judge.rs"], "+ // wip\n");
        change.source = ChangeSource::WorkingTree;
        let route = sr.route(&change);
        assert!(route.is_refused());
        let MergeRoute::Refused { escalation, .. } = route else {
            unreachable!()
        };
        assert_eq!(escalation.reason, EscalationReason::UncommittedSelfChange);
        assert!(escalation.report().contains("uncommitted"));
    }

    #[test]
    fn the_repair_agent_is_cut_from_the_running_commit() {
        let mut sr = dispatcher();
        let Decision::Dispatch(plan) = sr.handle(&condition()) else {
            panic!("expected a dispatch");
        };
        // The plan carries a committed base, not a working tree.
        assert_eq!(plan.base_sha, "deadbeef");
    }

    // --- loop guard ------------------------------------------------------

    #[test]
    fn repeated_attempts_at_the_same_condition_stop_and_escalate() {
        let mut sr = dispatcher();
        for attempt in 1..=DEFAULT_SELF_REPAIR_ATTEMPTS {
            assert!(
                matches!(sr.handle(&condition()), Decision::Dispatch(_)),
                "attempt {attempt} should still dispatch"
            );
        }
        let Decision::Escalate(escalation) = sr.handle(&condition()) else {
            panic!("the third attempt must stop instead of retrying");
        };
        assert_eq!(
            escalation.reason,
            EscalationReason::AttemptsExhausted {
                limit: DEFAULT_SELF_REPAIR_ATTEMPTS
            }
        );
        assert_eq!(escalation.attempts, DEFAULT_SELF_REPAIR_ATTEMPTS + 1);
        assert!(escalation.report().contains("stopped self-repairing"));
    }

    #[test]
    fn the_guard_counts_conditions_separately() {
        let mut sr = dispatcher();
        let other = UnhandledCondition::new("dispatch/capacity-exhausted", "no worker", vec![]);
        assert!(matches!(sr.handle(&condition()), Decision::Dispatch(_)));
        assert!(matches!(sr.handle(&other), Decision::Dispatch(_)));
        assert_eq!(sr.loop_guard.attempts(&condition().key), 1);
        assert_eq!(sr.loop_guard.attempts("dispatch/capacity-exhausted"), 1);
    }

    #[test]
    fn escalation_after_the_guard_reports_the_attempts_it_made() {
        let mut sr = dispatcher();
        sr.handle(&condition());
        let change = own_change(&["crates/dispatcher-core/src/self_repair.rs"], "");
        let MergeRoute::Refused { escalation, .. } = sr.route(&change) else {
            panic!("expected a refusal");
        };
        assert_eq!(escalation.attempts, 1);
        assert_eq!(escalation.condition_key, "judge/no-endpoint");
    }

    // --- surface matching ------------------------------------------------

    #[test]
    fn path_matching_is_case_insensitive() {
        let surface = SafetySurface::new(vec![SurfaceRule::path(
            "gate",
            Rail::Gate,
            "a deterministic check",
        )]);
        let change = own_change(&["Crates/AutoSpec-Dispatchers/src/BIG_Gate.rs"], "");
        assert_eq!(surface.detect(&change).len(), 1);
    }

    #[test]
    fn context_lines_are_not_treated_as_changes() {
        // Only +/- lines are edits. A context line that happens to mention a
        // protected token must not refuse an unrelated change.
        let surface = SafetySurface::default();
        let change = own_change(
            &["crates/dispatcher-core/src/judge.rs"],
            "  const MIN_CORROBORATING_APPROVALS: usize = 2;\n+ // a comment\n",
        );
        assert!(surface.detect(&change).is_empty());
    }

    #[test]
    fn diff_headers_are_not_treated_as_changes() {
        let surface = SafetySurface::default();
        let change = own_change(
            &["crates/dispatcher-core/src/judge.rs"],
            "--- a/crates/dispatcher-core/src/judge.rs\n\
             +++ b/crates/dispatcher-core/src/judge.rs\n\
             @@ -1 +1 @@\n+ // a comment\n",
        );
        assert!(surface.detect(&change).is_empty());
    }
}
