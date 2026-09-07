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
}
