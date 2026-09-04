# Packaging an exact Ratspeak release

Ratspeak releases use a coordinated set of five repositories. The
`release/dependency-set.json` committed at the **product release tag** is the
authority for all four sibling sources. Read it from the version being packaged,
not from a moving branch.

| Manifest field | Packaging meaning |
| --- | --- |
| `components[].commit` | Exact, lowercase, 40-character Git commit to fetch. |
| `components[].integrationTag` | Permanent annotated alias, such as `ratspeak-v1.0.31`, which must resolve to that exact commit. |
| `components[].version` | Cargo package version metadata; it is **not** a source revision or a standalone tag selector. |
| `components[].repository` and `path` | Reviewed upstream repository and required sibling location. |
| `toolchains` | Toolchain versions used to qualify the release. |

For example, rsReticulum and rsLXMF both reported package version `1.2.0` in
Ratspeak v1.0.31, but their standalone `v1.2.0` tags predated APIs used by that
Ratspeak release. Their `ratspeak-v1.0.31` tags identify the correct later commits.
Do not choose a sibling's latest tag, branch tip, or nearest commit by date.
Coordinated aliases are immutable: a new source set requires a new release,
even when the component package version is unchanged.

## Reconstructing the source tree

The Node.js helper in this source tree fetches an annotated Ratspeak release tag,
reads its dependency set, fetches the four exact component commits, and verifies
that all annotated aliases and component versions agree. It supports coordinated
releases starting at v1.0.29; the helper itself ships with newer releases and can
also be run from a current tools checkout to reconstruct older ones.

```sh
# Run using this tools checkout; the source inputs come from the requested tag.
node scripts/release/checkout-release-source.mjs \
  --tag v1.0.31 --destination ../ratspeak-v1.0.31-source --plan

node scripts/release/checkout-release-source.mjs \
  --tag v1.0.31 --destination ../ratspeak-v1.0.31-source
```

The destination's parent directory must already exist. The resulting layout is:

```text
ratspeak-v1.0.31-source/
  Ratspeak/
  rsReticulum/
  rsLXMF/
  rsLXST/
  lrgp-rs/
```

The JSON result uses the explicit name `fetchCommit`. Plan mode fetches only the
product release and predecessor tags, reports the component pins, and leaves the
destination unchanged. `componentTagsVerified` stays `false` until a full checkout
verifies the sibling aliases. A plan is not a completed source verification.

Use `--checkout /path/to/Ratspeak` instead of `--tag` to infer the release from a
clean product checkout already at its exact annotated release tag. A downloaded
GitHub source archive has no Git metadata: run explicit `--tag` mode from an
archive that includes this helper, or use the manifest directly in a packager.
The tool does not turn an archive into a verified existing checkout.

For offline verification or local repository caches, `--local-sources DIR`
fetches from five local Git repositories named as above. The same tag and commit
checks apply. This checks consistency, not a cryptographic signature or the
provenance of a supplied mirror; consumers still select trusted sources and
record source hashes.

The helper does not build, install dependencies, change global Git settings, or
push. It stages and verifies new sources before installing them. Existing
destination checkouts are reused only when their commits, tags, and worktrees
already match. Modified, untracked, or ignored files, unrelated entries, wrong
revisions, and symlinked targets are refused. Use another dedicated destination
for an existing build tree; nothing is reset or cleaned automatically. Git
commands have a two-minute timeout with noninteractive authentication.

## Locked builds and release verification

Install the platform prerequisites described in the
[build guide](https://docs.ratspeak.org/docs/reference/building-from-source).
The source helper preserves the product's full release-tag history for the API
baseline ancestry checks, plus its predecessor tag. Component fetches remain
shallow and pinned to exact commits. Local product mirrors must contain complete
history. The completed tree can use the normal check:

```sh
cd ../ratspeak-v1.0.31-source
node Ratspeak/scripts/release/source-integrity.mjs verify-tags
cd Ratspeak/src-tauri
cargo tauri build -- --locked
```

Both `Ratspeak/Cargo.lock` and `Ratspeak/src-tauri/Cargo.lock` are committed release
inputs. The application is a separate Cargo workspace rooted at `src-tauri` and
uses the **src-tauri lockfile**. Root workspace checks use the root lockfile. Do
not substitute one for the other or regenerate them while packaging a release.
For a compile-only application check, run `cargo check --locked` from `src-tauri`.

Source verification establishes the selected release graph. It does not replace
platform library dependencies, an actual platform build, device tests, or
distribution-specific packaging validation.

## Nix packaging

Keep the selected product source immutable, load its dependency-set JSON, and
derive each sibling fetch from `commit`. Each fetch also needs the corresponding
Nix fixed-output hash. The following is a fragment to integrate into a complete
derivation, not a standalone package definition:

```nix
let
  # ratspeakSrc is the already pinned source for the selected Ratspeak release.
  dependencySet = builtins.fromJSON
    (builtins.readFile "${ratspeakSrc}/release/dependency-set.json");
  componentSources = builtins.listToAttrs (map (component: {
    name = component.id;
    value = fetchgit {
      url = component.repository;
      rev = component.commit;
      hash = componentHashes.${component.id};
    };
  }) dependencySet.components);
in
  # Assemble the sibling layout above and build from Ratspeak/src-tauri.
  # cargoLock.lockFile = "${ratspeakSrc}/src-tauri/Cargo.lock";
  # componentSources.rsreticulum, .rslxmf, .rslxst, and .lrgp supply the siblings.
  ...
```

The updater must read the manifest from each new Ratspeak release and update
the exact component commits and their source hashes together. If the derivation
uses `tag` attributes elsewhere, its updater must not assume every fetch has a
`rev` field. Do not derive `rev = "v${component.version}"` or select revisions
by a release date.

Verify annotated aliases when updating the package, using the source helper or
the existing release verifier with Git checkouts. Nix fetches that remove `.git`
still use the exact commits and fixed-output hashes during the build. GitHub's
automatic Ratspeak source archive contains only Ratspeak, so it cannot replace
the four sibling fetches.

This source layout supports Nix without publishing the application or protocol
crates to crates.io or removing Cargo's workspace dependency declarations.
