//! Link evidence retained separately from crawl scheduling.
//!
//! A page can mention the same destination many times, with different anchors and stages.
//! Scheduling still deduplicates destination requests; this module keeps each observed anchor
//! occurrence so the audit can point at the exact source, while network checking stays optional
//! and bounded.

use crate::indexability::EvidenceStage;

pub const MAX_PAGE_OCCURRENCES: usize = 10_000;
pub const MAX_RUN_OCCURRENCES: usize = 250_000;
pub const MAX_ANCHOR_CHARS: usize = 2_048;
pub const MAX_URL_BYTES: usize = 8 * 1024;
pub const MAX_CHECKED_DESTINATIONS: usize = 1_000;
pub const MAX_REDIRECT_HOPS: usize = 5;

/// One anchor occurrence, retained even when its destination is not scheduled or checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkOccurrence {
    /// The page whose document contained the anchor.
    pub source: String,
    /// Stable document order within this source and stage.
    pub order: u32,
    /// The literal href, bounded and UTF-8 safe.
    pub raw_href: String,
    /// The raw or resolved URL was bounded. A truncated URL is evidence, not a fetch target.
    pub truncated: bool,
    /// The resolved HTTP(S) destination without its fragment, bounded.
    pub resolved: String,
    /// Normalized join key for this destination, matching `crawl::url_key`.
    pub key: String,
    /// The fragment exactly as declared, without `#`, bounded.
    pub fragment: String,
    /// Normalized descendant text, excluding script/style/template. Empty is preserved.
    pub anchor: String,
    /// The document stage that supplied the occurrence.
    pub stage: EvidenceStage,
    /// Why scheduling or checking did not happen, when it did not.
    pub skip_reason: String,
    /// The observed optional-check state. Page-crawler evidence fills this for internal pages.
    pub check: Option<Box<LinkCheckResult>>,
}

impl Default for LinkOccurrence {
    fn default() -> Self {
        Self {
            source: String::new(),
            order: 0,
            raw_href: String::new(),
            truncated: false,
            resolved: String::new(),
            key: String::new(),
            fragment: String::new(),
            anchor: String::new(),
            stage: EvidenceStage::Document,
            skip_reason: String::new(),
            check: None,
        }
    }
}

impl LinkOccurrence {
    pub fn check_state(&self) -> LinkCheckState {
        self.check
            .as_ref()
            .map_or(LinkCheckState::NotChecked, |check| check.state)
    }

    pub fn state_label(&self) -> &'static str {
        self.check
            .as_ref()
            .map(|check| check.state.label())
            .unwrap_or(LinkCheckState::NotChecked.label())
    }

    pub fn state_reason(&self) -> &str {
        self.check
            .as_ref()
            .map(|check| check.reason.as_str())
            .filter(|reason| !reason.is_empty())
            .unwrap_or(self.skip_reason.as_str())
    }

    pub fn is_finding(&self) -> bool {
        self.check.as_ref().is_some_and(|check| check.is_finding())
    }

    pub fn is_incomplete(&self) -> bool {
        self.check
            .as_ref()
            .is_none_or(|check| check.state.is_incomplete())
    }

    pub fn status(&self) -> Option<u16> {
        self.check
            .as_ref()
            .and_then(|check| (check.status != 0).then_some(check.status))
    }

    pub fn final_url(&self) -> &str {
        self.check
            .as_ref()
            .map(|check| check.final_url.as_str())
            .filter(|url| !url.is_empty())
            .unwrap_or(self.resolved.as_str())
    }

    pub fn redirect_chain(&self) -> &[String] {
        self.check
            .as_ref()
            .map(|check| check.redirect_chain.as_slice())
            .unwrap_or(&[])
    }

    /// A stable row key for selection and navigation.
    pub fn row_key(&self) -> String {
        format!("{}#{}#{}", self.source, self.order, self.stage.label())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkCheckState {
    /// No optional check was requested, or the destination was left for the page crawler.
    #[default]
    NotChecked,
    /// A response or a deliberate page-crawler observation was available.
    Checked,
    /// robots.txt refused the destination.
    Blocked,
    /// The optional budget, timeout, or evidence cap stopped the check.
    Limited,
    /// The origin rate-limited the check.
    RateLimited,
    /// Authentication or authorization restricted the response.
    Restricted,
    /// Transport, TLS, DNS, or timeout failure.
    Unavailable,
    /// The href could not be resolved to a safe HTTP(S) destination.
    Invalid,
    /// A same-document fragment, not an independently fetched resource.
    Fragment,
}

impl LinkCheckState {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotChecked => "Not checked",
            Self::Checked => "Checked",
            Self::Blocked => "Blocked",
            Self::Limited => "Limited",
            Self::RateLimited => "Rate limited",
            Self::Restricted => "Restricted",
            Self::Unavailable => "Unavailable",
            Self::Invalid => "Invalid",
            Self::Fragment => "Fragment",
        }
    }

    pub fn is_incomplete(self) -> bool {
        matches!(
            self,
            Self::NotChecked
                | Self::Blocked
                | Self::Limited
                | Self::RateLimited
                | Self::Unavailable
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkCheckResult {
    pub state: LinkCheckState,
    pub reason: String,
    pub status: u16,
    pub final_url: String,
    pub redirect_chain: Vec<String>,
    /// A guardrail stopped the chain or the check, not the destination itself.
    pub limited: bool,
}

impl LinkCheckResult {
    pub fn checked(status: u16, final_url: String, redirect_chain: Vec<String>) -> Self {
        let state = match status {
            401 | 403 => LinkCheckState::Restricted,
            429 => LinkCheckState::RateLimited,
            _ => LinkCheckState::Checked,
        };
        Self {
            state,
            reason: String::new(),
            status,
            final_url,
            redirect_chain,
            limited: false,
        }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            state: LinkCheckState::Unavailable,
            reason: reason.into(),
            ..Self::default()
        }
    }

    pub fn blocked(reason: impl Into<String>) -> Self {
        Self {
            state: LinkCheckState::Blocked,
            reason: reason.into(),
            ..Self::default()
        }
    }

    pub fn limited(reason: impl Into<String>) -> Self {
        Self {
            state: LinkCheckState::Limited,
            reason: reason.into(),
            limited: true,
            ..Self::default()
        }
    }

    pub fn is_finding(&self) -> bool {
        match self.state {
            LinkCheckState::Invalid | LinkCheckState::Unavailable => true,
            LinkCheckState::Restricted | LinkCheckState::RateLimited => true,
            LinkCheckState::Checked => self.status >= 400,
            _ => false,
        }
    }

    pub fn verdict(&self) -> &'static str {
        match self.state {
            LinkCheckState::Checked => match self.status {
                200..=299 => "OK",
                300..=399 => "Redirect",
                404 | 410 => "Missing",
                400..=499 => "Client error",
                500..=599 => "Server error",
                _ => "Checked",
            },
            LinkCheckState::Blocked => "Blocked",
            LinkCheckState::Limited => "Not checked",
            LinkCheckState::RateLimited => "Rate limited",
            LinkCheckState::Restricted => "Restricted",
            LinkCheckState::Unavailable => "Unavailable",
            LinkCheckState::Invalid => "Invalid",
            LinkCheckState::Fragment => "Fragment",
            LinkCheckState::NotChecked => "Not checked",
        }
    }
}

/// Bound a string by Unicode characters, preserving valid UTF-8 and appending an ellipsis only
/// when content was removed.
pub fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Bound a string by bytes without splitting a code point.
pub fn truncate_bytes(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max.saturating_sub(3).min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = value[..end].to_string();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_is_utf8_safe_and_only_marks_removal() {
        assert_eq!(truncate_chars("abcdef", 10), "abcdef");
        assert_eq!(truncate_chars("abcdef", 4), "abc…");
        assert_eq!(truncate_bytes("éééé", 5), "é…");
        assert_eq!(truncate_bytes("hello", 5), "hello");
    }

    #[test]
    fn occurrence_findings_distinguish_missing_from_restricted_and_incomplete() {
        let mut occurrence = LinkOccurrence {
            source: "https://example.com/".into(),
            resolved: "https://example.com/missing".into(),
            ..LinkOccurrence::default()
        };
        assert!(occurrence.is_incomplete());
        assert!(!occurrence.is_finding());
        occurrence.check = Some(Box::new(LinkCheckResult::checked(
            404,
            occurrence.resolved.clone(),
            Vec::new(),
        )));
        assert!(occurrence.is_finding());
        assert!(!occurrence.is_incomplete());
        occurrence.check = Some(Box::new(LinkCheckResult::checked(
            403,
            occurrence.resolved.clone(),
            Vec::new(),
        )));
        assert_eq!(occurrence.check_state(), LinkCheckState::Restricted);
        assert!(occurrence.is_finding());
    }
}
