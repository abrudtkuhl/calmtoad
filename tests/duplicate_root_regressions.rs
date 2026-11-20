use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use calmtoad::content::{self, CONTENT_MEMORY_BUDGET, ContentBudget, ContentInput, ContentScope};
use calmtoad::indexability::EvidenceStage;
use scraper::Html;

fn evidence(html: &str, stage: EvidenceStage) -> Arc<content::ContentEvidence> {
    Arc::new(content::extract(
        &Html::parse_document(html),
        stage,
        true,
        &ContentBudget::default(),
    ))
}

#[test]
fn root_review_cases_have_expected_behaviour() {
    let a = (0..80)
        .map(|index| format!("word{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let b = a.replacen("word0", "different", 1);
    let x = evidence(&format!("<main>{a}</main>"), EvidenceStage::Document);
    let y = evidence(&format!("<main>{b}</main>"), EvidenceStage::Rendered);
    let cross = content::analyze(
        vec![
            ContentInput {
                url: "https://x/a".into(),
                content: x,
            },
            ContentInput {
                url: "https://x/b".into(),
                content: y,
            },
        ],
        1,
        &AtomicBool::new(false),
    );
    assert!(
        cross.pairs.is_empty(),
        "Document and Rendered captures must not form near pairs"
    );

    let deep = |text: &str| {
        format!(
            "<main>shared prefix{}{}{} </main>",
            "<div>".repeat(520),
            text,
            "</div>".repeat(520)
        )
    };
    let x = evidence(&deep("entirely different A"), EvidenceStage::Document);
    let y = evidence(&deep("entirely different B"), EvidenceStage::Document);
    assert!(x.exact_eligible && y.exact_eligible);
    assert_ne!(x.hash, y.hash, "deep text must not be silently dropped");
    assert!(x.excerpt.contains("entirely different a"));
    assert!(y.excerpt.contains("entirely different b"));

    let hidden = evidence(
        "<section hidden><main>hidden wrong content</main></section><article>real visible article</article>",
        EvidenceStage::Document,
    );
    assert_eq!(hidden.scope, Some(ContentScope::Article));
    assert_eq!(&*hidden.excerpt, "real visible article");

    let one = evidence(&format!("<main>{a}</main>"), EvidenceStage::Document);
    let two = evidence(
        &format!("<main>{}</main>", a.replacen("word0", "different", 1)),
        EvidenceStage::Document,
    );
    let analysis = content::analyze(
        vec![
            ContentInput {
                url: "https://x/a".into(),
                content: one,
            },
            ContentInput {
                url: "https://x/b".into(),
                content: two,
            },
        ],
        2,
        &AtomicBool::new(false),
    );
    assert_eq!(analysis.similar_affected(), 2);

    let exhausted = ContentBudget::with_used(CONTENT_MEMORY_BUDGET);
    let rejected = content::extract(
        &Html::parse_document("<main>Some retained excerpt text</main>"),
        EvidenceStage::Document,
        true,
        &exhausted,
    );
    assert!(!rejected.exact_eligible);
    assert!(
        rejected.excerpt.is_empty(),
        "exhausted budget retained an excerpt"
    );
    assert!(!rejected.exact_reason.is_empty());
}

#[test]
fn exhausted_metadata_does_not_allocate_per_url_result_maps() {
    let evidence = Arc::new(content::ContentEvidence::default());
    let inputs = (0..100)
        .map(|index| ContentInput {
            url: format!("https://x/{index}"),
            content: evidence.clone(),
        })
        .collect();
    let budget = ContentBudget::with_used(CONTENT_MEMORY_BUDGET);
    let result = content::analyze_with_budget(inputs, 1, &AtomicBool::new(false), &budget);
    assert_eq!(result.pages.len(), 0);
    assert_eq!(result.url_states.len(), 0);
    assert_eq!(result.not_compared.len(), 0);
    assert!(
        result
            .coverage
            .limited_reasons
            .iter()
            .any(|reason| reason.contains("URL metadata"))
    );
}

#[test]
fn long_url_metadata_is_reserved_for_every_retained_copy() {
    let evidence = Arc::new(content::ContentEvidence::default());
    let long_urls = || {
        (0..3)
            .map(|index| ContentInput {
                url: format!("https://x/{index}/{}", "x".repeat(7_980)),
                content: evidence.clone(),
            })
            .collect::<Vec<_>>()
    };

    // The exact root case: three valid ~8 KiB URLs with only 30,000 bytes free. The ledger must
    // refuse before retaining page/state/Not compared copies rather than charge one copy.
    let started = CONTENT_MEMORY_BUDGET - 30_000;
    let budget = ContentBudget::with_used(started);
    let result = content::analyze_with_budget(long_urls(), 1, &AtomicBool::new(false), &budget);
    assert_eq!(result.pages.len(), 0);
    assert_eq!(result.url_states.len(), 0);
    assert_eq!(result.not_compared.len(), 0);
    assert_eq!(
        budget.used(),
        started,
        "failed metadata reservation was charged"
    );
    assert!(
        result
            .coverage
            .limited_reasons
            .iter()
            .any(|reason| reason.contains("URL metadata"))
    );

    // With room to succeed, the ledger must still cover the URL payload actually retained by the
    // page map, URL-state map and Not compared diagnostics.
    let budget = ContentBudget::default();
    let result = content::analyze_with_budget(long_urls(), 1, &AtomicBool::new(false), &budget);
    let retained_url_payload = result.pages.keys().map(String::len).sum::<usize>()
        + result.url_states.keys().map(String::len).sum::<usize>()
        + result
            .not_compared
            .iter()
            .map(|row| row.url.len())
            .sum::<usize>();
    assert!(retained_url_payload > 0);
    assert!(
        result.retained_bytes >= retained_url_payload,
        "ledger {} undercharged retained URL payload {}",
        result.retained_bytes,
        retained_url_payload
    );
}

#[test]
fn final_copy_failure_preserves_near_eligible_resource_state() {
    let text = (0..80)
        .map(|index| format!("token{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let evidence = evidence(&format!("<main>{text}</main>"), EvidenceStage::Document);
    assert!(evidence.exact_eligible);
    assert_eq!(
        evidence.near_state,
        content::NearState::Eligible,
        "the regression must exercise a near-eligible representative"
    );
    // Metadata + member + member-index reservations fit at these budgets. At 1,800/2,000 the
    // near candidate index is omitted, so the representative would finalize as SearchLimited
    // without the terminal-state guard; at 3,000 the index fits and it would finalize Compared.
    for remaining in [1_800, 2_000, 3_000] {
        let budget = ContentBudget::with_used(CONTENT_MEMORY_BUDGET - remaining);
        let result = content::analyze_with_budget(
            vec![
                ContentInput {
                    url: "https://x/a".into(),
                    content: evidence.clone(),
                },
                ContentInput {
                    url: "https://x/b".into(),
                    content: evidence.clone(),
                },
            ],
            1,
            &AtomicBool::new(false),
            &budget,
        );
        assert!(
            result
                .coverage
                .limited_reasons
                .iter()
                .any(|reason| reason.contains("Final exact result copies")),
            "final-copy failure did not occur at {remaining}: {:?}",
            result.coverage.limited_reasons
        );
        assert_eq!(result.coverage.exact_resource_limited, 2, "at {remaining}");
        assert!(result.groups.is_empty(), "at {remaining}");
        assert!(result.all_groups.is_empty(), "at {remaining}");
        assert_eq!(result.coverage.exact_groups, 0, "at {remaining}");
        assert_eq!(result.same_text_affected(), 0, "at {remaining}");
        assert!(result.member_groups.is_empty(), "at {remaining}");
        for url in ["https://x/a", "https://x/b"] {
            assert_eq!(
                result.url_states.get(url),
                Some(&content::AnalysisState::ResourceLimited),
                "representative finalization overwrote the resource state for {url} at {remaining}"
            );
        }
        assert_eq!(result.not_compared.len(), 2, "at {remaining}");
        for row in &result.not_compared {
            assert_eq!(
                row.analysis_state,
                content::AnalysisState::ResourceLimited,
                "at {remaining}"
            );
            assert!(
                row.reason.contains("Final exact result copies"),
                "at {remaining}: {:?}",
                row.reason
            );
        }
    }
}

#[test]
fn near_eligible_final_copy_probe_budgets_remain_coherent() {
    let text = (0..80)
        .map(|index| format!("token{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let evidence = evidence(&format!("<main>{text}</main>"), EvidenceStage::Document);
    assert_eq!(evidence.near_state, content::NearState::Eligible);

    // Convert every root probe budget into an expected shape. The invariant is coherence rather
    // than a fixed failure budget: a retained group must advertise matching counts, and a shed
    // group must leave every exact-eligible URL terminally resource limited.
    for remaining in [1_600, 1_800, 2_000, 2_200, 2_400] {
        let budget = ContentBudget::with_used(CONTENT_MEMORY_BUDGET - remaining);
        let result = content::analyze_with_budget(
            vec![
                ContentInput {
                    url: "https://x/a".into(),
                    content: evidence.clone(),
                },
                ContentInput {
                    url: "https://x/b".into(),
                    content: evidence.clone(),
                },
            ],
            1,
            &AtomicBool::new(false),
            &budget,
        );
        let final_copy_failed = result
            .coverage
            .limited_reasons
            .iter()
            .any(|reason| reason.contains("Final exact result copies"));
        if final_copy_failed {
            assert_eq!(result.coverage.exact_resource_limited, 2, "at {remaining}");
            assert!(result.groups.is_empty(), "at {remaining}");
            assert!(result.all_groups.is_empty(), "at {remaining}");
            assert_eq!(result.coverage.exact_groups, 0, "at {remaining}");
            assert_eq!(result.same_text_affected(), 0, "at {remaining}");
            assert!(result.member_groups.is_empty(), "at {remaining}");
            assert_eq!(result.not_compared.len(), 2, "at {remaining}");
            for row in &result.not_compared {
                assert_eq!(
                    row.analysis_state,
                    content::AnalysisState::ResourceLimited,
                    "at {remaining}"
                );
                assert!(
                    row.reason.contains("Final exact result copies"),
                    "at {remaining}: {:?}",
                    row.reason
                );
            }
            for url in ["https://x/a", "https://x/b"] {
                assert_eq!(
                    result.url_states.get(url),
                    Some(&content::AnalysisState::ResourceLimited),
                    "at {remaining}"
                );
            }
        } else {
            assert_eq!(result.groups.len(), 1, "at {remaining}");
            assert_eq!(result.all_groups.len(), 1, "at {remaining}");
            assert_eq!(result.coverage.exact_groups, 1, "at {remaining}");
            assert_eq!(result.same_text_affected(), 2, "at {remaining}");
            assert!(
                result.not_compared.iter().all(|row| row.analysis_state
                    == content::AnalysisState::SearchLimited
                    && !row.reason.is_empty()),
                "at {remaining}: {:?}",
                result.not_compared
            );
            assert!(
                result
                    .url_states
                    .values()
                    .all(|state| *state != content::AnalysisState::ResourceLimited),
                "at {remaining}"
            );
        }
    }
}

#[test]
fn final_copy_failure_removes_phantom_groups_and_associations() {
    let evidence = Arc::new(content::extract(
        &Html::parse_document("<main>identical short text</main>"),
        EvidenceStage::Document,
        true,
        &ContentBudget::default(),
    ));
    let budget = ContentBudget::with_used(CONTENT_MEMORY_BUDGET - 1_800);
    let result = content::analyze_with_budget(
        vec![
            ContentInput {
                url: "https://x/a".into(),
                content: evidence.clone(),
            },
            ContentInput {
                url: "https://x/b".into(),
                content: evidence,
            },
        ],
        1,
        &AtomicBool::new(false),
        &budget,
    );
    assert!(result.groups.is_empty());
    assert!(result.all_groups.is_empty());
    assert_eq!(result.coverage.exact_groups, 0);
    assert_eq!(result.same_text_affected(), 0);
    assert!(result.member_groups.is_empty());
    assert!(
        result
            .url_states
            .values()
            .all(|state| *state == content::AnalysisState::ResourceLimited)
    );
    assert_eq!(result.not_compared.len(), 2);
}
