# bi

A batteries-included modal editor. Tree-sitter, git, and LSP are meant to be
built in, not plugins.

## What bi is

bi stands for **Bart's IDE**. It exists because I wanted an editor where all
the things I love about Neovim and its plugins are built in rather than
assembled, and where an update never breaks the setup. It also aims to turn
the editor into a game development environment, thanks to the kitty protocol.

That means bi occasionally varies from standard vim/neovim behavior.
[docs/GENERAL.md](docs/GENERAL.md) notes each difference where it happens.

C/C++, Go, C3 and Rust are first-class citizens: they get attention first.
Everything else is best-effort.

bi ships custom editors that live inside the text: a colour picker, a curve
editor with tangent handles, a gradient editor with stops, and images that
open as the picture they are. Each opens over the literal under the cursor
and writes the result back. They are drawn with the
[kitty](https://sw.kovidgoyal.net/kitty/) graphics protocol (kitty, ghostty).

bi has its own property format too: a `.bischema` defines struct-like types,
a `.bidata` holds sparse instances, both plain JSON, both opened as a
Unity-style inspector. `bi gen struct` turns them into types and constants in
C, C++, Go, Rust, C3 or Lua. See [bi-format.md](docs/specs/bi-format.md) and
[gen-struct.md](docs/specs/gen-struct.md).

**Disclaimer:** bi is a 100% AI-driven project — it started as an experiment
and led to a working editor.

## Install

Grab the tarball for your platform from the
[latest release](https://github.com/qbart/bi/releases/latest), then:

```sh
tar xzf bi-*.tar.gz
sudo mv bi-*/bi /usr/local/bin/bi
```

On macOS, clear the quarantine flag so Gatekeeper lets it run:

```sh
xattr -dr com.apple.quarantine /usr/local/bin/bi
```

Then set up your config:

```sh
bi config init   # writes ~/.config/bi/config.toml, defaults commented out
bi config edit   # opens the config directory in bi
bi gen sample    # writes game.bischema and level1.bidata here — the property view's sample
```

`:reload` inside bi re-reads the config without restarting.

Or build from source with `cargo build --release`.

## Docs

- [docs/GENERAL.md](docs/GENERAL.md) — everything: status, key bindings, commands, config
- [docs/specs](docs/specs) — the design behind each piece
- [CONTRIBUTING.md](CONTRIBUTING.md) — what contributions fit
