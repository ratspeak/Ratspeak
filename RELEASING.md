# Source Release Qualification

Ratspeak releases are built from a reviewed set of exact source commits. A
successful qualification proves that the source tree can produce artifacts;
it does not create a tag, upload an artifact, publish a crate, or authorize a
store submission.

## Version model

- `Cargo.toml` and `src-tauri/Cargo.toml` carry the numeric application/API
  version (`1.0.26`). First-party Ratspeak crates share it.
- `VERSION` carries the public display version: the numeric marketing version
  for a stable release, or that version plus one lowercase prerelease letter
  (`1.0.26o`). A letter is never reused after its Git tag exists. Promotion
  from a prerelease to its stable numeric release and later numeric version
  lines are explicit supported transitions.
- `tauri.conf.json` carries the numeric marketing version, the monotonically
  increasing Android `versionCode`, and the matching numeric iOS
  `bundleVersion`. The explicit iOS value prevents Tauri generation from
  replacing the intended Apple build sequence with the marketing version.
- The Apple project and generated Info.plist carry that same marketing version
  and numeric `CFBundleVersion`. Android and iOS use the same build sequence
  when a candidate is prepared for both platforms.
- Component manifest versions express API compatibility. They do not identify
  the source alone; exact Git commits do.

## Reviewed dependency set

`release/dependency-set.json` is the canonical Ratspeak integration graph. It
records:

- the exact rsReticulum, rsLXMF, rsLXST, and lrgp-rs commit;
- the compatible package version expected from each checkout;
- product display, marketing, Android, and iOS versions;
- the supported Android artifact targets (`aarch64`, `armv7`, and `x86_64`;
  never i686 until separately requalified);
- release and MSRV Rust versions plus platform build tool versions;
- the Node version used by source qualification and release evidence tooling;
  and
- an optional integration tag only when that tag resolves to the exact commit.

Ordinary CI mirrors those commits in its static checkout fields so GitHub can
resolve dependencies before running project code. The pin contract proves the
fields equal the canonical JSON. Release workflows load their checkout commits
directly from the JSON and verify the resulting sibling worktrees before any
build begins. Component checkouts include tag objects, and every non-null
integration tag is required to be an annotated tag resolving to that exact
component commit.

## Qualification

From the normal sibling checkout layout:

```bash
node scripts/release/source-integrity.mjs verify-local
node --test scripts/release/source-integrity.test.mjs
bash scripts/ci/check-workflow-dependency-pins.sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo check --manifest-path src-tauri/Cargo.toml --locked
```

After committing the candidate, run
`node scripts/release/source-integrity.mjs verify-release-source`; release
workflows require every product and component worktree to be completely clean
before building. BOM generation rechecks that tracked source remains clean
after the build while ignoring generated, untracked artifacts.

Release builds must use both committed lockfiles and the declared tool versions
in the dependency set. Every platform workflow emits the same canonical source
graph and lockfile evidence in its BOM. Runner-specific observed tool versions
are recorded separately within that BOM, so complete BOM bytes are expected to
differ across platforms while source identity remains identical. Platform and
architecture qualifiers in BOM filenames preserve every runner's evidence when
artifacts are merged into one GitHub Release.

The application bundle also carries
`third_party/opus-rs-0.1.29-COPYING`. Its source contract and hash are checked
alongside the exact upstream Opus dependency so binary artifacts retain the
BSD-3-Clause and patent notice after the old rsLXST vendor is removed.

Android release qualification must inspect the final R8-minified artifact,
not only compile Kotlin or test an unminified debug build. Rust resolves the
app's BLE, USB, platform-state, stored-file, and voice helpers by literal JVM
class, method, and descriptor. Their reviewed inventory lives in
`scripts/release/android-jni-boundaries.json`. Run the source check before the
build and the archive check against every produced APK or AAB:

```bash
python3 scripts/release/assert-android-jni-boundaries.py source
python3 scripts/release/assert-android-jni-boundaries.py archive path/to/Ratspeak.apk
```

Ordinary CI builds and inspects one arm64 minified release APK. The Android
release workflow repeats the archive check for all arm64, ARMv7, and x86_64
APKs and for the Play AAB when one is requested. A release artifact is invalid
if any inventoried boundary method was renamed, removed, or changed signature.

Before a later release operation, the maintainer must also verify that:

1. all component source-qualification contracts are green;
2. the changelog describes the candidate;
3. before tag authorization, the intended Ratspeak tag is unused; then, after
   the separately authorized annotated tag is created, it resolves to the exact
   candidate commit; and
4. release qualification accepts only that existing annotated tag, and the
   generated BOM and platform artifacts come from its checkout.

Component semantic tags and Ratspeak integration tags serve different
purposes. A component tag denotes that component's own release. An optional
integration tag is only an audit label for one Ratspeak source graph. Neither
may replace the exact commit in the dependency set.

Crates.io publication, NixOS packaging, crate naming, license changes, and API
stability decisions are outside this qualification process.
