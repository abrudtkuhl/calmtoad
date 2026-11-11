# Contributing

Thanks for helping improve Calm Toad. Keep changes focused and describe the user-visible effect in your pull request.

## Development setup

Install a stable Rust toolchain that supports edition 2024. On Linux, install `libxkbcommon-dev` before building.

```sh
cargo run
```

To crawl a public site from the terminal and print the first ten results:

```sh
cargo run --release --example crawl -- https://pmprompt.com/
```

## Checks

Run these before opening a pull request:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The Linux CI job also runs the performance gate:

```sh
cargo bench --bench crawl
```

The benchmark uses a synthetic site and does not make network requests. CI runs formatting, Clippy, and tests on Ubuntu, macOS, and Windows, plus the performance gate on Linux.

## Performance

The harness reports the median time per page (or per parse) for each scenario, and can emit JSON or compare against the committed baseline:

```sh
cargo bench --bench crawl                     # human-readable table
cargo bench --bench crawl -- --json           # machine-readable report
scripts/perf-gate.sh                          # exit 1 if a gated scenario regresses >15%
scripts/update-baseline.sh                    # refresh benches/perf-baseline.json
scripts/profile.sh                            # flamegraph (needs cargo-flamegraph)
scripts/profile.sh audit --seconds 5          # profile one phase
```

The baseline also stores a calibration run and the platform it came from. `--compare` scales the baseline by the calibration ratio, so a slower machine does not read as a code regression, and warns when the platform differs. The gate compares the fastest sample (best of nine) to resist scheduler noise. CI runs the gate with a 30% threshold and uploads the JSON report as an artifact. The multi-threaded crawl is reported but not gated, because its throughput depends on the runner's core count. Refresh the baseline on the machine or runner that will gate:

```sh
scripts/update-baseline.sh --samples 9
```

The profiling example uses the same synthetic pages:

```sh
cargo flamegraph --example profile
cargo flamegraph --example profile -- counts --seconds 5
```

## UI changes and screenshots

The `sample` scene is an offline fixture and can render a local screenshot:

```sh
cargo run -- --scene sample --snap shots/sample.png --size 1180x740 --scale 1 --theme dark --at 0.4
```

The `pmprompt` scene runs a live crawl and regenerates the README screenshot (requires network access):

```sh
cargo run --release -- --scene pmprompt --snap assets/pmprompt-crawl.png --size 1180x740 --scale 2 --theme dark --at 0.4
```

Use `assets/` for screenshots intended to be committed. The generated `shots/` directory is ignored.

## Pull requests

- Keep the change focused and explain how you verified it.
- Include or update a screenshot when a UI change materially affects the app.
- Keep machine-specific Codex and Impeccable settings out of commits; `.codex/hooks.json` and `.impeccable/` are ignored.
- CI runs on pushes and pull requests. Resolve failed checks before merging.
- For each user-visible change, including work prepared by agents, update the
  upcoming version in `CHANGELOG.md` in the same change (use `## [Unreleased]`
  until the next version is chosen). Write concise bullets
  under `### Added`, `### Improved`, or `### Fixed`, describing what users can do
  or what now works. Omit empty categories and internal-only maintenance. Use git
  history and the actual implementation as evidence; do not invent older entries.

## Releases

1. Update the version in `Cargo.toml` and the matching `Cargo.lock` entry. Prepare
   a matching `## [0.2.0]` section in `CHANGELOG.md` (an optional release date uses
   `## [0.2.0] - YYYY-MM-DD`), moving the entries being shipped out of `Unreleased`.
   Review the entries against the changes being shipped.
   Complete and commit these notes before creating a tag or dispatching a release.
2. Validate the extractor and preview exactly the user-facing notes:

   ```sh
   python3 -m unittest discover -s scripts -p 'test_release_notes.py'
   python3 scripts/release_notes.py 0.2.0
   ```

   Use the actual Cargo version. The dependency-free extractor fails for missing,
   duplicate, or empty version sections; headings and comments alone are empty.
3. Push the version and notes commit to `main`.
4. In GitHub Actions, run **Release** from `main` and enter the Cargo version without a `v` prefix (for example, `0.2.0`).

The workflow validates the requested version and changelog before creating a tag.
It publishes the reviewed notes plus a compare link to the previous reachable
version tag. CI needs no AI credentials or note-generation service. Missing notes
stop publication rather than falling back to commit messages.

Release runs are serialized by a single workflow concurrency group, and the
requested version must not be older than the latest published release. Each run
that publishes promotes its release with `--latest`, so publishing an older
version would move `latest` (and the updater manifest URL) backwards. The
workflow rejects that before it creates a tag. If the latest release cannot be
determined (any API failure other than a confirmed 404 for no releases), the run
stops rather than guessing.

For a failed **draft** release, rerun the workflow on the same version. An
existing tag on the selected `main` history is reused and downloads still build
from that tag. Notes come from the selected dispatch commit, so a notes-only
correction can update the notes of the latest published release. A rerun for the
latest published version takes a notes-only path: it edits release notes and
leaves the tag, every asset, `latest.json`, and the `latest` pointer untouched.
Do not include changes newer than the tag in that version's notes. Retries resume
a draft release and replace its assets; an already-published release is never
given new assets, and its tag is never moved.

Before creating a tag, the workflow requires `MINISIGN_SECRET_KEY`, signs a small
probe with it, and verifies that signature against the committed
`assets/updater.pub`. A missing secret or a key pair that does not match fails
before the tag exists, leaving no half-configured release behind.

Retries of an existing **tag** build from that tag, not from the dispatch commit.
The workflow therefore checks that the tag's `assets/updater.pub` and
`scripts/release-manifest.sh` match the selected `main` commit. If the tag
predates the updater signing tooling (or carries a different public key), a
retry cannot repair it: delete and recreate that tag deliberately, or ship a new
version. Do not retag a published release just to pick up newer release tooling.

The workflow creates the matching `vX.Y.Z` tag, builds downloads for Linux x86_64, macOS Apple silicon, macOS Intel, and Windows x86_64, then publishes them to a GitHub Release and GitHub Packages. To pull the OCI package, install ORAS and run:

```sh
oras pull ghcr.io/abrudtkuhl/calmtoad:v0.2.0
```

### Update signing key

The app downloads `latest.json` from `releases/latest/download/` and verifies its
minisign signature before trusting any download. The public key is committed at
`assets/updater.pub`; the private key is not in the repository.

Generate a key pair once (minisign or the compatible `rsign`):

```sh
rsign generate -p calmtoad.pub -s calmtoad.key --unencrypted \
  -c "Calm Toad update signing key"
```

Keep the private key offline and add its full contents as the
`MINISIGN_SECRET_KEY` repository secret. Put the public key in
`assets/updater.pub` and commit it with the release that starts using it. Rotate
by adding a second public key to `PUBLIC_KEY` handling in `src/update/mod.rs` for
one release before removing the old key.

The release workflow builds `latest.json` with `scripts/release-manifest.sh`,
signs it with `minisign -Sm`, verifies the signature, uploads the manifest and
`latest.json.minisig` to the draft release, re-downloads every asset from the
draft and checks its SHA-256, and only then publishes the release. A missing
secret or a key that does not match `assets/updater.pub` stops the run before a
tag is created. A later verification failure leaves the release as a draft, so
the `releases/latest` URL keeps pointing at the previous version.

Users who installed a build from before in-app updates existed need one manual
upgrade; later versions update themselves.

The Linux build needs the `libxkbcommon` runtime. macOS app bundles are currently unsigned and unnotarized.
