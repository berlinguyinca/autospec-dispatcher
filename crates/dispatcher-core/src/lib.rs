//! Domain types for the dispatch plane.
//!
//! The split this crate exists to hold: deterministic observation decides
//! pass/fail, and a model decides only what a failure *means*. See
//! `docs/ARCHITECTURE.md`.

/// Why a check failed, established mechanically by comparing against the merge
/// base — never by asking a model.
///
/// The distinction is load-bearing: `architecture-fitness` failing on a PR that
/// also fails on `main` is overridable, while `file-size-ratchet` failing
/// because the change grew an oversized file is not. Both are a red X on a
/// named check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureOrigin {
    /// The merge base fails this check the same way. Overridable, with the
    /// evidence recorded.
    PreExisting,
    /// The change introduced this failure. Never overridable.
    CausedByChange,
}

/// A model's verdict. Absent judgement is its own state and is never a pass.
///
/// Measured: five of twelve reviewer verdicts in one batch returned empty
/// content after exhausting the token budget. Reading those as "no problems
/// found" would have merged unreviewed changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    Concerns,
    Reject,
    /// No usable judgement: empty response, malformed output, timeout, or no
    /// endpoint. Never a pass, never a fail.
    Unknown {
        reason: String,
    },
}

impl Verdict {
    /// Only an explicit approval counts toward corroboration.
    pub fn is_approval(&self) -> bool {
        matches!(self, Verdict::Approve)
    }

    /// How the verdict is recorded on the PR. `UNKNOWN` keeps its reason
    /// visible: the tally must show absent judgement, not hide it.
    pub fn label(&self) -> String {
        match self {
            Verdict::Approve => "APPROVE".to_owned(),
            Verdict::Concerns => "CONCERNS".to_owned(),
            Verdict::Reject => "REJECT".to_owned(),
            Verdict::Unknown { reason } => format!("UNKNOWN ({reason})"),
        }
    }
}

/// A concrete model: which model, served at which deployment.
///
/// Identity is load-bearing twice:
/// - reviewers must sit on **different models**; agreement between copies of
///   one model is corroboration between clones,
/// - a reviewer must never be the **same instance** that implemented the
///   change. Separation of duties is enforced in [`ReviewPanel::assemble`],
///   not asked for in prompt prose.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelInstance {
    /// Model name/version, e.g. `GLM-Q6`. Two instances with equal `model`
    /// are the same model even behind different endpoints.
    pub model: String,
    /// Serving deployment, e.g. `prod-a`.
    pub endpoint: String,
}

impl ModelInstance {
    pub fn new(model: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            endpoint: endpoint.into(),
        }
    }

    /// Whether both instances run the same underlying model.
    pub fn same_model(&self, other: &ModelInstance) -> bool {
        self.model == other.model
    }
}

/// Panel size the dispatcher expects unless configured otherwise. Default 3:
/// measured, only the 3/3 approval was safe to merge.
pub const DEFAULT_REVIEWERS: usize = 3;

/// A merge needs at least this many independent approvals. One approval is
/// never corroboration, so a panel of one can never merge.
pub const MIN_CORROBORATING_APPROVALS: usize = 2;

/// Review panel configuration.
#[derive(Debug, Clone)]
pub struct ReviewConfig {
    /// Number of reviewers the panel must contain. Configurable because not
    /// everyone has capacity for three independent reviews; fewer approvals
    /// still cannot merge below [`MIN_CORROBORATING_APPROVALS`].
    pub reviewers: usize,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            reviewers: DEFAULT_REVIEWERS,
        }
    }
}

/// A problem a reviewer reported, identified by a stable `key` so the same
/// finding raised by two reviewers is recognized as one finding corroborated
/// twice, not two findings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub key: String,
    pub detail: String,
}

impl Finding {
    pub fn new(key: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            detail: detail.into(),
        }
    }
}

/// One reviewer's submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub reviewer: ModelInstance,
    pub verdict: Verdict,
    pub findings: Vec<Finding>,
}

/// Why a set of reviews cannot form a panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewError {
    /// A panel must have at least one reviewer.
    NoReviewers,
    /// The panel must contain exactly the configured number of reviewers.
    ReviewerCountMismatch { expected: usize, got: usize },
    /// Two reviewers ran the same underlying model.
    DuplicateModel { model: String },
    /// A reviewer is the instance that implemented the change.
    SeparationOfDuties { model: String, endpoint: String },
}

impl std::fmt::Display for ReviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReviewError::NoReviewers => write!(f, "a review panel needs at least one reviewer"),
            ReviewError::ReviewerCountMismatch { expected, got } => {
                write!(f, "expected {expected} reviewers, got {got}")
            }
            ReviewError::DuplicateModel { model } => write!(
                f,
                "reviewers must be different models; {model} appears more than once"
            ),
            ReviewError::SeparationOfDuties { model, endpoint } => write!(
                f,
                "{model}@{endpoint} implemented this change and cannot review it"
            ),
        }
    }
}

impl std::error::Error for ReviewError {}

/// Deterministic aggregation of the panel's verdicts. The model judgement is
/// per reviewer; the merge call itself is code, on those judgements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewOutcome {
    /// Every reviewer explicitly approved, with at least
    /// [`MIN_CORROBORATING_APPROVALS`] approvals. The only mergeable state.
    CorroboratedApproval,
    /// Two or more reviewers independently returned CONCERNS. Not mergeable;
    /// the concerns are actionable and corroborated.
    CorroboratedConcerns,
    /// Anything else — a reject, a lone approval, or any absent judgement.
    /// Not mergeable.
    NotMergeable,
}

impl ReviewOutcome {
    pub fn is_mergeable(self) -> bool {
        self == ReviewOutcome::CorroboratedApproval
    }

    pub fn label(self) -> &'static str {
        match self {
            ReviewOutcome::CorroboratedApproval => "MERGE",
            ReviewOutcome::CorroboratedConcerns => "CONCERNS_CORROBORATED",
            ReviewOutcome::NotMergeable => "NOT_MERGEABLE",
        }
    }
}

/// A finding merged across reviewers, ranked by how many independently
/// raised it. Two or more independent raisers is the signal that held; a
/// finding one reviewer raised may be run-to-run variance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedFinding {
    pub finding: Finding,
    /// Distinct reviewer instances that raised this finding.
    pub raised_by: Vec<ModelInstance>,
}

impl RankedFinding {
    /// Number of independent reviewers that raised this finding.
    pub fn corroboration(&self) -> usize {
        self.raised_by.len()
    }
}

/// The assembled review panel: who reviewed, what they said, and what the
/// panel decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPanel {
    pub implementer: ModelInstance,
    pub reviews: Vec<Review>,
}

impl ReviewPanel {
    /// Assemble a panel, enforcing the structural rules before any verdict
    /// is read: the configured panel size, distinct reviewer models, and
    /// separation of duties against the implementer.
    pub fn assemble(
        config: &ReviewConfig,
        implementer: ModelInstance,
        reviews: Vec<Review>,
    ) -> Result<Self, ReviewError> {
        if config.reviewers == 0 || reviews.is_empty() {
            return Err(ReviewError::NoReviewers);
        }
        if reviews.len() != config.reviewers {
            return Err(ReviewError::ReviewerCountMismatch {
                expected: config.reviewers,
                got: reviews.len(),
            });
        }
        for (i, reviewer) in reviews.iter().enumerate() {
            if reviewer.reviewer == implementer {
                return Err(ReviewError::SeparationOfDuties {
                    model: reviewer.reviewer.model.clone(),
                    endpoint: reviewer.reviewer.endpoint.clone(),
                });
            }
            for other in &reviews[..i] {
                if reviewer.reviewer.same_model(&other.reviewer) {
                    return Err(ReviewError::DuplicateModel {
                        model: reviewer.reviewer.model.clone(),
                    });
                }
            }
        }
        Ok(Self {
            implementer,
            reviews,
        })
    }

    /// How many distinct models corroborate. Reported so the basis of the
    /// decision is auditable.
    pub fn distinct_model_count(&self) -> usize {
        let mut models: Vec<&str> = Vec::with_capacity(self.reviews.len());
        for review in &self.reviews {
            if !models.contains(&review.reviewer.model.as_str()) {
                models.push(&review.reviewer.model);
            }
        }
        models.len()
    }

    /// The merge decision. A lone APPROVE beside two `UNKNOWN`s does not
    /// qualify: `UNKNOWN` is absent judgement, and absent judgement is never
    /// a pass — so anything but unanimous explicit approval fails to merge.
    pub fn outcome(&self) -> ReviewOutcome {
        let approvals = self
            .reviews
            .iter()
            .filter(|r| r.verdict.is_approval())
            .count();
        let rejects = self
            .reviews
            .iter()
            .filter(|r| r.verdict == Verdict::Reject)
            .count();
        let concerns = self
            .reviews
            .iter()
            .filter(|r| r.verdict == Verdict::Concerns)
            .count();

        if rejects > 0 {
            ReviewOutcome::NotMergeable
        } else if approvals == self.reviews.len() && approvals >= MIN_CORROBORATING_APPROVALS {
            ReviewOutcome::CorroboratedApproval
        } else if concerns >= MIN_CORROBORATING_APPROVALS {
            ReviewOutcome::CorroboratedConcerns
        } else {
            ReviewOutcome::NotMergeable
        }
    }

    /// Findings merged by key across reviewers, most-corroborated first.
    /// Ties keep first-seen order (stable sort).
    pub fn ranked_findings(&self) -> Vec<RankedFinding> {
        let mut merged: Vec<RankedFinding> = Vec::new();
        for review in &self.reviews {
            for finding in &review.findings {
                match merged.iter_mut().find(|r| r.finding.key == finding.key) {
                    Some(ranked) => {
                        if !ranked.raised_by.contains(&review.reviewer) {
                            ranked.raised_by.push(review.reviewer.clone());
                        }
                    }
                    None => merged.push(RankedFinding {
                        finding: finding.clone(),
                        raised_by: vec![review.reviewer.clone()],
                    }),
                }
            }
        }
        merged.sort_by_key(|ranked| std::cmp::Reverse(ranked.corroboration()));
        merged
    }

    /// The audit record for the PR: the decision, the distinct-model count,
    /// every reviewer including `UNKNOWN`s, and findings ranked by
    /// corroboration.
    pub fn pr_record(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("review outcome: {}\n", self.outcome().label()));
        out.push_str(&format!(
            "distinct reviewer models: {}\n\n",
            self.distinct_model_count()
        ));
        out.push_str("| reviewer | verdict | findings |\n| --- | --- | --- |\n");
        for review in &self.reviews {
            out.push_str(&format!(
                "| {}@{} | {} | {} |\n",
                review.reviewer.model,
                review.reviewer.endpoint,
                review.verdict.label(),
                review.findings.len(),
            ));
        }
        let ranked = self.ranked_findings();
        if !ranked.is_empty() {
            out.push_str("\n| finding | raised by | corroboration |\n| --- | --- | --- |\n");
            for ranked_finding in &ranked {
                let raisers: Vec<String> = ranked_finding
                    .raised_by
                    .iter()
                    .map(|m| format!("{}@{}", m.model, m.endpoint))
                    .collect();
                out.push_str(&format!(
                    "| {} | {} | {} |\n",
                    ranked_finding.finding.key,
                    raisers.join(", "),
                    ranked_finding.corroboration(),
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_is_not_an_approval() {
        let v = Verdict::Unknown {
            reason: "finish=length".to_owned(),
        };
        assert!(!v.is_approval());
    }

    #[test]
    fn only_approve_counts_toward_corroboration() {
        assert!(Verdict::Approve.is_approval());
        assert!(!Verdict::Concerns.is_approval());
        assert!(!Verdict::Reject.is_approval());
    }

    fn instance(model: &str) -> ModelInstance {
        ModelInstance::new(model, "prod")
    }

    fn review(model: &str, verdict: Verdict) -> Review {
        Review {
            reviewer: instance(model),
            verdict,
            findings: Vec::new(),
        }
    }

    fn unknown() -> Verdict {
        Verdict::Unknown {
            reason: "finish=length".to_owned(),
        }
    }

    fn assemble_with(implementer: &str, reviews: Vec<Review>) -> Result<ReviewPanel, ReviewError> {
        ReviewPanel::assemble(&ReviewConfig::default(), instance(implementer), reviews)
    }

    fn panel(reviews: Vec<Review>) -> ReviewPanel {
        assemble_with("Builder-7", reviews).expect("panel assembles")
    }

    #[test]
    fn default_panel_is_three_reviewers() {
        assert_eq!(ReviewConfig::default().reviewers, 3);
    }

    #[test]
    fn panel_size_is_configurable() {
        let config = ReviewConfig { reviewers: 2 };
        let panel = ReviewPanel::assemble(
            &config,
            instance("Builder-7"),
            vec![
                review("GLM-Q6", Verdict::Approve),
                review("Flash-Next", Verdict::Approve),
            ],
        )
        .expect("a two-reviewer panel is allowed");
        assert_eq!(panel.outcome(), ReviewOutcome::CorroboratedApproval);
    }

    #[test]
    fn panel_rejects_wrong_reviewer_count() {
        let err = assemble_with(
            "Builder-7",
            vec![
                review("GLM-Q6", Verdict::Approve),
                review("Flash-Next", Verdict::Approve),
            ],
        )
        .expect_err("two reviews do not fill a three-reviewer panel");
        assert_eq!(
            err,
            ReviewError::ReviewerCountMismatch {
                expected: 3,
                got: 2,
            }
        );
    }

    #[test]
    fn reviewers_must_be_different_models() {
        let err = assemble_with(
            "Builder-7",
            vec![
                review("GLM-Q6", Verdict::Approve),
                // same model behind a different endpoint is still the same model
                Review {
                    reviewer: ModelInstance::new("GLM-Q6", "spare"),
                    verdict: Verdict::Approve,
                    findings: Vec::new(),
                },
                review("Qwen-27B-Q8", Verdict::Approve),
            ],
        )
        .expect_err("copies of one model cannot corroborate");
        assert_eq!(
            err,
            ReviewError::DuplicateModel {
                model: "GLM-Q6".to_owned(),
            }
        );
    }

    #[test]
    fn implementer_may_never_review_its_own_change() {
        let implementer = ModelInstance::new("Builder-7", "prod");
        let err = ReviewPanel::assemble(
            &ReviewConfig::default(),
            implementer.clone(),
            vec![
                Review {
                    reviewer: implementer,
                    verdict: Verdict::Approve,
                    findings: Vec::new(),
                },
                review("Flash-Next", Verdict::Approve),
                review("Qwen-27B-Q8", Verdict::Approve),
            ],
        )
        .expect_err("separation of duties is enforced in code");
        assert_eq!(
            err,
            ReviewError::SeparationOfDuties {
                model: "Builder-7".to_owned(),
                endpoint: "prod".to_owned(),
            }
        );
    }

    #[test]
    fn distinct_model_count_is_reported() {
        let p = panel(vec![
            review("GLM-Q6", Verdict::Approve),
            review("Flash-Next", Verdict::Approve),
            review("Qwen-27B-Q8", Verdict::Approve),
        ]);
        assert_eq!(p.distinct_model_count(), 3);
    }

    #[test]
    fn unanimous_approval_merges() {
        // Patch A: APPROVE / APPROVE / APPROVE -> merged.
        let p = panel(vec![
            review("GLM-Q6", Verdict::Approve),
            review("Flash-Next", Verdict::Approve),
            review("Qwen-27B-Q8", Verdict::Approve),
        ]);
        assert!(p.outcome().is_mergeable());
    }

    #[test]
    fn concerns_on_two_reviewers_are_corroborated_not_merged() {
        // Patch B: CONCERNS / CONCERNS / UNKNOWN -> corroborated concerns.
        let p = panel(vec![
            review("GLM-Q6", Verdict::Concerns),
            review("Flash-Next", Verdict::Concerns),
            review("Qwen-27B-Q8", unknown()),
        ]);
        assert_eq!(p.outcome(), ReviewOutcome::CorroboratedConcerns);
        assert!(!p.outcome().is_mergeable());
    }

    #[test]
    fn lone_approve_beside_two_unknown_does_not_merge() {
        // Patch C: APPROVE / UNKNOWN / UNKNOWN -> not merged.
        let p = panel(vec![
            review("GLM-Q6", Verdict::Approve),
            review("Flash-Next", unknown()),
            review("Qwen-27B-Q8", unknown()),
        ]);
        assert_eq!(p.outcome(), ReviewOutcome::NotMergeable);
        assert!(!p.outcome().is_mergeable());
    }

    #[test]
    fn lone_concern_beside_two_unknown_does_not_merge() {
        // Patch D: CONCERNS / UNKNOWN / UNKNOWN -> not merged, and not
        // corroborated either: one concern is one reviewer's opinion.
        let p = panel(vec![
            review("GLM-Q6", Verdict::Concerns),
            review("Flash-Next", unknown()),
            review("Qwen-27B-Q8", unknown()),
        ]);
        assert_eq!(p.outcome(), ReviewOutcome::NotMergeable);
    }

    #[test]
    fn a_single_approver_never_merges_even_with_no_companion_verdict() {
        // Panel of one cannot corroborate, whatever it says.
        let p = ReviewPanel::assemble(
            &ReviewConfig { reviewers: 1 },
            instance("Builder-7"),
            vec![review("GLM-Q6", Verdict::Approve)],
        )
        .expect("panel of one assembles");
        assert!(!p.outcome().is_mergeable());
    }

    #[test]
    fn reject_blocks_merge() {
        let p = panel(vec![
            review("GLM-Q6", Verdict::Approve),
            review("Flash-Next", Verdict::Approve),
            review("Qwen-27B-Q8", Verdict::Reject),
        ]);
        assert_eq!(p.outcome(), ReviewOutcome::NotMergeable);
    }

    #[test]
    fn findings_raised_twice_rank_above_findings_raised_once() {
        let mut r1 = review("GLM-Q6", Verdict::Concerns);
        r1.findings.push(Finding::new(
            "unbounded-read",
            "reads the whole file into memory",
        ));
        let mut r2 = review("Flash-Next", Verdict::Concerns);
        r2.findings
            .push(Finding::new("unbounded-read", "same: full-file read"));
        r2.findings
            .push(Finding::new("naming", "unclear module name"));
        let p = panel(vec![r1, r2, review("Qwen-27B-Q8", Verdict::Approve)]);

        let ranked = p.ranked_findings();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].finding.key, "unbounded-read");
        assert_eq!(ranked[0].corroboration(), 2);
        assert_eq!(ranked[1].finding.key, "naming");
        assert_eq!(ranked[1].corroboration(), 1);
    }

    #[test]
    fn pr_record_lists_every_reviewer_including_unknowns() {
        let p = panel(vec![
            review("GLM-Q6", Verdict::Approve),
            review("Flash-Next", unknown()),
            review("Qwen-27B-Q8", unknown()),
        ]);
        let record = p.pr_record();
        assert!(record.contains("GLM-Q6@prod | APPROVE"), "{record}");
        assert!(
            record.contains("Flash-Next@prod | UNKNOWN (finish=length)"),
            "{record}"
        );
        assert!(
            record.contains("Qwen-27B-Q8@prod | UNKNOWN (finish=length)"),
            "{record}"
        );
        assert!(record.contains("distinct reviewer models: 3"), "{record}");
        assert!(record.contains("NOT_MERGEABLE"), "{record}");
    }
}
