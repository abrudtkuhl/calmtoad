# Duplicate content research

Research date: 2026-11-19. Design recommendation for the Rust desktop crawler; these proposed defaults need corpus benchmarking, not a claim of production performance. No LLM or remote service is needed.

## Decision

Use independent normalized-content exact groups, then fixed-seed MinHash/LSH candidates over sets of consecutive five-token shingles, followed by verified Jaccard scoring. Show near-duplicate **pairs**, not transitive equivalence groups. If A resembles B and B resembles C, A need not resemble C. Broder defines shingle-set resemblance as intersection divided by union; this measures syntactic overlap rather than semantic equivalence. [Broder et al., original technical report](https://www.cse.iitb.ac.in/~soumen/readings/papers/BroderGMZ1997shingle.pdf).

Prefer MinHash to SimHash here because the score can be explained directly: `J(A,B) = |A∩B| / |A∪B|`. A shared MinHash coordinate has probability J under ideal min-wise permutations. Charikar's random-hyperplane alternative measures vector angle; Hamming distance is not a Jaccard percentage. [Charikar, original paper](https://www.cs.princeton.edu/courses/archive/spring04/cos598B/bib/CharikarEstim.pdf).

SimHash remains a valid smaller alternative: 64 bits, four indexes on 16-bit blocks, then XOR/popcount ≤3 retrieves every fingerprint within three bits if buckets are uncapped (pigeonhole principle). It does **not** retrieve every pair above a specified Jaccard threshold. The original WWW07 study selected 64 bits/three differences empirically, with precision and recall approximately 0.75 on its evaluation, not universal accuracy. [Manku et al., §§3–4](https://archives.iw3c2.org/www2007/papers/paper215.pdf).

## Text and eligibility contract

Extract from the same successful HTML snapshot used for content audits. Preserve `Document` versus `Rendered` evidence; partition comparisons by stage by default. Exclude incomplete/truncated reads, failed rendering, missing captures, non-HTML and empty content; never hash a common empty string into an exact group. The existing `ReadState`/`EvidenceStage` definitions in `app/src/indexability.rs` and extraction states in `app/src/extract.rs` support these distinctions.

Proposed deterministic selection: cleaned nonempty `main` (including `[role=main]`), otherwise largest cleaned `article` by text length with DOM-order ties, otherwise cleaned `body`. Use one subtree, avoiding nested text duplication. Remove `script`, `style`, `template`, `noscript`, `nav`, navigation roles, `footer`, and `aside`; discard known hidden attributes. Keep article headers/headings. Preserve spaces at block/`br` boundaries but concatenate ordinary inline text correctly (`co<em>op</em>` → `coop`). Record main/article/body provenance; body fallback warrants a template-noise caveat. The HTML standard gives `main` its dominant-content meaning; author markup remains imperfect. [HTML Standard](https://html.spec.whatwg.org/multipage/grouping-content.html#the-main-element).

This is a deliberately small heuristic, not Mozilla Readability. Readability is article-oriented and provides extracted text; adding a maintained Rust port is a separate dependency decision. CSS class hiding and arbitrary cookie banners will escape simple structural cleanup; do not claim browser-visible text in source mode. [Mozilla Readability](https://github.com/mozilla/readability/blob/main/README.md).

Define exact normalization separately: decoded DOM text, ASCII case fold, trim and collapse Unicode whitespace, preserve punctuation/digits/order. Thus punctuation changes remain different exact content. Hash the entire result with SHA-256; compare retained normalized bytes within equal-hash buckets if literal equality is promised. Exclude pages exceeding retention limits instead of hashing identical prefixes.

For near comparison, split normalized text into Unicode-alphanumeric runs, treating punctuation as boundaries, then form unique five-token shingles. `red-blue` and `red blue` match; `can't` yields `can`, `t`; numbers remain significant. This is simple rather than linguistic tokenization: unspaced languages, combining marks and Unicode case equivalents need documented limitations or a dedicated Unicode tokenizer. `split_whitespace` alone retains punctuation; `is_alphanumeric` tests Alphabetic or Number properties. Version this policy. [Rust str](https://doc.rust-lang.org/std/primitive.str.html#method.split_whitespace), [Rust char](https://doc.rust-lang.org/std/primitive.char.html#method.is_alphanumeric).

## Candidate policy and limits

Suggested starting point: 120 fixed-seed 64-bit MinHash coordinates, 24 bands ×5 rows; candidates share an identical complete band. Under ideal independent coordinates, candidate probability is `1-(1-s^5)^24`: approximately 99.993% at J=.80, 53.3% at .50, 0.765% at .20. Fixed seeds make runs reproducible; they do not guarantee recall. Practical hash families and preprocessing change behavior. [Banding derivation, authors' textbook](https://infolab.stanford.edu/~ullman/mmds/ch3n.pdf).

Verify candidate shingle intersections and require J≥.80; apply safe size pruning `min(|A|,|B|)/max(|A|,|B|) ≥ .80`. Never present signature agreement as the final score. Sorted 64-bit shingle hashes give collision-negligible rather than mathematically exact token-set scores; retain actual shingles for collision-free verification if required. Skip near comparisons below 50 tokens, showing “too short”; exact detection still applies.

Index exact-group representatives, then attach exact members without expanding every member pair. Sort URLs before bounded analysis. Initial guardrails to benchmark: 256 entries/bucket, 200 distinct candidates/page, 100,000 verified pairs/run, 512 MiB retained evidence, and 10,000 tokens/page; exceeded limits yield explicit skipped/limited states and counts. Never silently truncate text or claim “no duplicates” after candidate loss.

At 10k/50k pages, raw signatures occupy 9.6/48 MB plus indexes, text and shingles. All-pairs would require 49,995,000/1,249,975,000 comparisons. LSH avoids that baseline but pathological shared bands still produce quadratic candidates without caps. These caps deliberately sacrifice recall; benchmark extraction, signature CPU, peak memory and recall against exhaustive small fixtures before setting release defaults.
