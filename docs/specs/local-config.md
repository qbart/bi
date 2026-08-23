# Local config

A project gets a say: `.bi.toml`, found in the working directory or the
nearest ancestor holding one, laid over the main config the same way the main
config lays over the defaults — a PATCH. An option the file does not mention
keeps whatever the main config decided; one it does mention wins.

## Status

**Built.**

## The lookup, and the silence rule

The frontend walks up from the working directory and takes the **first**
`.bi.toml` it can read — one file, nearest wins, no layering of several. The
walk is silent by contract: a missing file, an unreadable one, a directory bi
may not enter are all just "keep looking", and running out of parents is
"there is none". That contract is in the type — `ConfigSource::local` returns
`Option`, not `Result`, so a lookup has no way to report trouble.

What *parses* badly is different: the file was found, the project meant it,
and a mistake in it is reported exactly as a mistake in the main config is —
each diagnostic prefixed with the file's path, so `:reload`'s "3 problems"
names which file. An unparseable local config reports and changes nothing:
the main config stays in force, never half of each.

`:reload` re-runs the whole layering, both files, same path as startup — the
only way the two stay in agreement.

## What a project may not say

Two sections are refused, with a diagnostic naming the refusal rather than a
silence:

- **`[lsp.servers.<name>].command`** — a repository that can name the binary
  bi spawns on open is arbitrary code execution by `git clone`. The rest of
  a server's definition — `enabled`, `filetypes`, `roots`, and `[lsp]`
  itself — is a project's legitimate business and is read.
- **`[keys]`** — a binding can carry an ex line, which is the same trap one
  keypress later; and a project has no business with your muscle memory
  besides.

Everything else is read: `[options]` (including `theme`), `[filetype.*]`,
`[alternate]`.

## Where it sits in the layers

Defaults → main config → **local config** → and then, per file, the layers
that always applied on top: bi's built-in filetype table, the project's
`.editorconfig`, and `:set`. A `:set` still outranks everything, for the
reason `options.md` gives: you said so, this session, by hand.

## Shape

`ConfigSource` grows one defaulted method — `local() -> Option<(PathBuf,
String)>` — so an embedder that wants no project config does nothing, and
the terminal frontend implements the walk (the working directory is process
state, which has always been the frontend's to know). The parser grows
`parse_local`, the same reader with the two refusals switched on, so a
refused key gets a real line number in its diagnostic.
