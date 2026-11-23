# axe-core 4.14.1 — vendored browser engine

Calm Toad embeds the official Deque axe-core browser bundle so accessibility checks run locally
with no CDN, npm, Node.js or runtime download.

## Pinned version

- Engine: **axe-core 4.14.1**
- License: **MPL-2.0**
- Pinned source tag: <https://github.com/dequelabs/axe-core/tree/v4.14.1>
- Tagged package manifest: <https://github.com/dequelabs/axe-core/blob/v4.14.1/package.json>
- API reference: <https://github.com/dequelabs/axe-core/blob/v4.14.1/doc/API.md>
- License text: <https://github.com/dequelabs/axe-core/blob/v4.14.1/LICENSE>
- Third-party notices: <https://github.com/dequelabs/axe-core/blob/v4.14.1/LICENSE-3RD-PARTY.txt>

`axe.min.js` is a build artifact and is not committed at the source tag root. The file here is the
official npm distributable for the same release:

- Tarball: <https://registry.npmjs.org/axe-core/-/axe-core-4.14.1.tgz>
- Tarball SHA-256: `e6b0ce02b84d892e2e33e403485e7f3d6c29e2be6df65daf44d89ba564139745`
- npm integrity: `sha512-FHfa+lmNjQZfJz1q8wKqIDwC6DExcHt/pkjNQb09a3NPjCKg2rhu5qb2S4r76ASyFNNtuFooJpZe37OgudVGtQ==`
- npm `dist.shasum`: `14d8a6f3f085afd126b8b5ca1e42e50e17fb1d99`

## Vendored files and SHA-256

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `axe.min.js` | 653,780 | `29348999450cdf2849a8e70b7912fc2da85a849da39c0d0d5394f51d4a9ed95c` |
| `LICENSE` | 15,921 | `af175b9d96ee93c21a036152e1b905b0b95304d4ae8c2c921c7609100ba8df7e` |
| `LICENSE-3RD-PARTY.txt` | 3,766 | `4f8563870d0fca38bbc3e00b6f670cb7fa9f380ba9f26a7f7d1184a6b18b1653` |

`LICENSE` and `LICENSE-3RD-PARTY.txt` are byte-identical to the files at the pinned source tag.
The bundle header identifies `axe v4.14.1` and carries the Deque copyright and MPL-2.0 notice.

## How to reproduce or update

1. Choose a new tagged axe-core release and review its changelog, license and third-party notices.
2. Download the npm tarball for that exact version, record its SHA-256 and npm integrity, and extract
   `package/axe.min.js`, `package/LICENSE` and `package/LICENSE-3RD-PARTY.txt`.
3. Verify the bundle header version, then update this directory, the engine constants in
   `src/accessibility.rs`, and these hashes.
4. Run the offline provenance test. It reads the embedded bytes and asserts the pinned version,
   size and SHA-256 without network access.
5. Re-run the real-browser fixtures, because rule selection and result shape can change between
   engine releases. Do not upgrade silently or load an unversioned download.

## Distributed-code obligations

The vendored `axe.min.js` remains covered by MPL-2.0. Its license and third-party notices ship with
the application and remain in this directory. Calm Toad does not modify the minified bundle; if a
future build step does, matching source availability for the modified covered code must be
provided. The rest of Calm Toad is a larger work under its own terms.
