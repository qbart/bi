# bi

A batteries-included modal editor. Tree-sitter, git, and LSP are meant to be
built in, not plugins.

## What bi is

bi stands for **Bart's IDE**. It exists because I wanted an editor where all
the things I love about Neovim and its plugins are built in rather than
assembled, where one person can hold the whole thing in their head, and where
an update never breaks the setup.

That means bi occasionally varies from standard vim/neovim behavior — good
defaults stayed, some things work differently.
[docs/GENERAL.md](docs/GENERAL.md) notes each difference where it happens.

C/C++, Go, C3 and Rust are first-class citizens: they get grammar, LSP and
testing attention first. Everything else is best-effort.

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
```

`:reload` inside bi re-reads the config without restarting.

Or build from source with `cargo build --release`.

## Docs

- [docs/GENERAL.md](docs/GENERAL.md) — everything: status, key bindings, commands, config
- [docs/specs](docs/specs) — the design behind each piece
- [CONTRIBUTING.md](CONTRIBUTING.md) — what contributions fit
