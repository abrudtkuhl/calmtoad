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

## Releases

1. Update the version in `Cargo.toml` and commit the matching `Cargo.lock` change.
2. Push the version commit to `main`.
3. In GitHub Actions, run **Release** from `main` and enter the Cargo version without a `v` prefix (for example, `0.2.0`).

The workflow creates the matching `vX.Y.Z` tag, builds downloads for Linux x86_64, macOS Apple silicon, macOS Intel, and Windows x86_64, then publishes them to a GitHub Release and GitHub Packages. To pull the OCI package, install ORAS and run:

```sh
oras pull ghcr.io/abrudtkuhl/calmtoad:v0.2.0
```

The Linux build needs the `libxkbcommon` runtime. macOS app bundles are currently unsigned and unnotarized.
