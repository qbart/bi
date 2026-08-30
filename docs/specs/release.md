# Release

How a version of bi becomes downloadable binaries. Pushing a tag shaped like
`v*` (say `v0.1.0`) triggers a GitHub Actions workflow that builds the editor
for three targets, packages each as a tarball, and publishes a GitHub Release
with checksums and auto-generated notes. Nothing happens on ordinary pushes;
the tag is the whole trigger.

## Targets

- `x86_64-unknown-linux-gnu` — built natively on `ubuntu-22.04`. The older
  runner is deliberate: its glibc 2.35 is the compatibility floor, so the
  binary runs on most distros from ~2022 on.
- `aarch64-unknown-linux-gnu` — cross-compiled on `ubuntu-22.04` with
  `gcc-aarch64-linux-gnu`. Covers ARM servers and Raspberry Pi 3/4/5 on a
  64-bit OS. GitHub's native ARM runners are public-repo only, and the repo is
  private; cross-compiling costs two env vars (`CC` for the tree-sitter C
  grammars, the cargo linker for the final link) and works everywhere.
- `aarch64-apple-darwin` — built natively on `macos-latest`, which is Apple
  Silicon.

Deliberately absent: 32-bit ARM (`armv7`) for Pis on 32-bit Raspberry Pi OS —
no current need, and it is the one target that gets fiddly. Linux binaries
link glibc rather than musl; the price is the ≥ 2.35 floor (Debian Bullseye is
below it), the gain is the standard allocator.

## The workflow

One file, `.github/workflows/release.yml`, two jobs.

**build** fans out over the target matrix. Each leg checks out, installs the
Rust target, runs `cargo build --release --locked` (`--locked` so a release is
built from `Cargo.lock` exactly, never a silently updated dependency), then
packages `bi-<tag>-<target>.tar.gz` containing the `bi` binary and uploads it
as a workflow artifact.

**release** waits for every leg, downloads the artifacts, writes a
`sha256sums.txt` over the tarballs, and creates the release with
`gh release create` — auto-generated notes, the three tarballs and the
checksum file attached. It needs `contents: write`; the built-in
`GITHUB_TOKEN` suffices, no PAT.

## Verifying a release

The workflow can only be tested by pushing a tag. If a leg fails, the release
job never runs — there is no half-published release to clean up. Re-running
after a fix means deleting the tag and release (if any) and pushing the tag
again.
