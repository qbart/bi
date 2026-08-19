# The other file

Every language has a pair. Go has `thing.go` and `thing_test.go`; C has `.c`
and `.h`; C++ picks two of five spellings. Getting from one to the other is a
`:e` and the whole path typed out, every time, for a file whose name you
already know because it is nearly the one you are in.

`:alt` — bound to `ga` — is that, without the typing.

## Status

**Built.**

## The rules

```toml
[alternate]
"*_test.go" = ["*.go"]
"*.go"      = ["*_test.go"]
"*.c"       = ["*.h"]
"*.h"       = ["*.c"]
"*.cpp"     = ["*.hpp", "*.h"]
"*.hpp"     = ["*.cpp", "*.cc"]
"*.cc"      = ["*.hh", "*.hpp", "*.h"]
"*.hh"      = ["*.cc", "*.cpp"]
```

`*` matches anything, separators included, and stands for the same text on the
right: `*_test.go` reads `internal/thing_test.go` as `internal/thing` and
offers `internal/thing.go`. That is one wildcard rather than a regular
expression on purpose — bi has no regex engine, and the pattern language a file
pair needs is exactly this big.

**Two orders, and both are load-bearing.** The first *rule* whose pattern
matches decides, which is why `*_test.go` is written before `*.go` — the other
way round, a test file matches `*.go` and becomes its own alternate forever.
Then the first *path* in that rule which exists is opened, which is how
`*.cpp` can prefer `.hpp` and settle for `.h`.

That is why the rules are a list rather than a table: a map would sort them,
and sorting them would break the first of those two orders.

**A rule you write replaces bi's for that pattern**, rather than sitting beside
it, so there is never a question of which one won. Any pattern bi does not
have is appended, keeping the order you wrote it in.

## What it does not do

**It does not create the file.** `:alt` finds the other one; if none of the
names exists it says which names it tried, and `:e` with one of them is the
command that makes it. Creating a file as a side effect of navigating to it is
how you end up with `thing_test.go` in the wrong directory.

**No labels.** The neovim plugin this was asked for by way of let each target
carry a name — `'Test'`, `'Implementation'` — for a chooser. With "first one
that exists" there is nothing to choose between, and a chooser that appears
only when two of them happen to exist is a surprise rather than a feature.

## The key

`ga`, under the `g` prefix, where vim prints a character code and bi does
nothing. `<leader>a` — which is what was asked for — is a line of config away:

```toml
[keys.normal]
"<leader>a" = ":alt<CR>"
```

It is not the default because bi's leader has no built-in meaning at all, and
the first binding to claim one should be the user's rather than one of ours.

## Tests

- Implementation to test and back, which is the ordering rule.
- The first path that exists wins, which is the other one.
- A missing alternate says which names it looked for; a file no rule matches
  says that instead.
- A `[alternate]` rule in the config replaces the built-in one for its pattern.
- `*` captures across directories, and greedily from the end, so `*.go` on
  `a.b.go` is `a.b`.
