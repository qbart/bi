# Install

How a published release becomes a working `bi` on someone's machine in one
command. `release.md` covers the other half — how a tag becomes the tarballs
this script downloads.

```sh
curl -sSL https://raw.githubusercontent.com/qbart/bi/master/install.sh | bash
```

The script detects the platform, resolves the latest version, downloads the
matching tarball and its checksum file, verifies the one against the other,
unpacks the binary into `/usr/local/bin` and runs it once to prove the install
worked. It is the only install path that is a single line, and the README
leads with it.

## Status

**Built.** `install.sh` at the repository root, served over
`raw.githubusercontent.com` from `master`. The branch is `master`, not `main`
— a `main` URL 404s.

## Knobs

Environment variables, not flags. A script read from a pipe cannot take
arguments without the `bash -s --` incantation, and nobody remembers it.

```sh
BI_VERSION    # tag to install, e.g. v0.5.1. Default: the latest release
INSTALL_DIR   # where the binary lands.  Default: /usr/local/bin
```

Both are set in front of `bash`, which survives the pipe:

```sh
curl -sSL .../install.sh | BI_VERSION=v0.5.0 INSTALL_DIR="$HOME/.local/bin" bash
```

`BI_VERSION` accepts `v0.5.1` or `0.5.1`; the `v` is added when missing,
because the tag has one and half the people typing it will not.

**`/usr/local/bin` is the default, and it needs root.** The alternative,
`~/.local/bin`, needs no password but is absent from `PATH` on enough systems
that the install would end in a second manual step — editing a shell rc — which
is exactly what the one-liner exists to avoid. So the script escalates instead:
if the directory is not writable it re-runs the three mutating commands under
`sudo`. That works under `curl | bash` because `sudo` reads its password from
`/dev/tty`, not from stdin, and stdin is the pipe.

## Resolving the version

`https://github.com/qbart/bi/releases/latest` redirects to the tag page. The
script follows it and takes the tag off the final URL. The obvious
alternative, `api.github.com/repos/qbart/bi/releases/latest`, is rate-limited
to 60 requests per hour per IP when unauthenticated, which is nothing behind a
corporate NAT or in CI; the redirect has no such limit. The API stays as a
fallback for the case where the redirect shape changes.

## Platforms

`uname -s` and `uname -m` map onto the three targets `release.md` builds:

| `uname -s` | `uname -m`      | target                        |
| ---------- | --------------- | ----------------------------- |
| `Linux`    | `x86_64`/`amd64`| `x86_64-unknown-linux-gnu`    |
| `Linux`    | `aarch64`/`arm64`| `aarch64-unknown-linux-gnu`  |
| `Darwin`   | `arm64`         | `aarch64-apple-darwin`        |

Anything else exits non-zero with the reason and a `cargo build --release`
pointer. Two cases are worth naming because people hit them:

**Intel Macs.** There is no `x86_64-apple-darwin` asset, so the script says so
rather than failing later on a download 404. Adding the target to the release
matrix is a decision for `release.md`, not something the installer can paper
over — Rosetta translates x86 to ARM, never the reverse.

**glibc older than 2.35.** The Linux binaries link glibc and are built on
Ubuntu 22.04, so Debian Bullseye and Ubuntu 20.04 are below the floor. Left
alone they install fine and then fail at first run with
`GLIBC_2.35 not found`, which reads like a bug in bi. The script parses
`ldd --version` and warns before downloading. It warns rather than aborts: the
parse is best-effort across distros, and a false positive that blocks a working
install is worse than a false positive that prints a line.

## Integrity

The checksum check is not optional. Both files come down — the tarball and the
release's `sha256sums.txt` — and the tarball's digest is compared against its
line in that file. A mismatch, a missing line, or the absence of both
`sha256sum` and `shasum` aborts the install. Verifying nothing would be
security theatre over HTTPS; verifying and failing closed is the point.

The tarball unpacks to `bi-<tag>-<target>/bi`, one directory deep, so the
script extracts with `--strip-components=1`. Everything happens in a `mktemp
-d` removed by an `EXIT` trap, so a failure halfway leaves nothing behind.

## After the copy

**Quarantine.** macOS gets `xattr -d com.apple.quarantine`, failure ignored.
Strictly it is unnecessary — `com.apple.quarantine` is set by the application
that downloads a file, and `curl` is not one of them — but it costs a line and
covers the case where the binary arrived some other way.

**Proof.** The script runs the installed binary with `--version` and prints
what it said. An install that reports success without ever executing the thing
it installed is a guess.

**PATH.** If `INSTALL_DIR` is not on `PATH`, the script prints the `export`
line to add. It does not edit shell rc files; rewriting someone's dotfiles
from a piped script is a liberty.

The closing message points at `bi config init`, which is where the README used
to send people next.

## Tests

- `curl -sSL <raw master url> | bash` installs the latest release and
  `bi --version` reports the tag that was installed.
- `BI_VERSION=v0.5.0` installs that tag; `BI_VERSION=0.5.0` installs the same
  tag.
- `INSTALL_DIR=$(mktemp -d)` installs there without invoking `sudo`.
- A writable `INSTALL_DIR` is never escalated; a non-writable one is.
- A corrupted tarball fails the checksum comparison and exits non-zero, with
  nothing copied to `INSTALL_DIR`.
- A `BI_VERSION` that does not exist fails on download with a readable message,
  not a 404 page written to disk as a tarball.
- An unsupported platform (`Darwin`/`x86_64`, `Linux`/`armv7l`) exits non-zero
  before any download and names the reason.
- The temp directory is gone after both a successful and a failed run.
- The script passes `shellcheck`.
