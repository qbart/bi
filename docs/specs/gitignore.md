# `.gitignore`

`Ctrl-P` listed every file under the root and skipped a hardcoded handful of
directory names — `target`, `node_modules`, and four more that seemed likely.
That is a guess about your project, and it is wrong in both directions: it
misses whatever your build tool is called this year, and it hides a `build/`
directory that is checked in and full of scripts you wanted to open.

The project already says which files are not its files. bi reads it.

## Status

**Built**, for the format's own rules. The two things it leaves out are named
at the bottom.

## What it changes

The built-in skip list is **gone**. One mechanism, and it is the project's
rather than bi's guess about the project. What remains is the hidden-entry
rule (`.git` alone would double the list) and the 20,000-file cap, which is a
backstop against a picker over a home directory rather than a policy about
files.

```
[options]
gitignore = true
```

Off, and the walk lists everything again — which is how you open a file the
project ignores. Nothing else in bi consults it: `:e` on an ignored path has
always worked and still does. This is a question about a *list*, not about
what you are allowed to edit.

## Which files are read

From the outside in, because the last match wins and depth is what decides
between two of them:

1. `<repo>/.git/info/exclude`, if there is a repository — the ignores you keep
   out of the project's own file
2. every `.gitignore` from the repository root down to the session root
3. every `.gitignore` inside the walk, as the walk reaches its directory

The repository is found by walking up for a `.git`, which is what makes
`Ctrl-P` correct when bi was opened on a subdirectory: the root's
`.gitignore` still applies to files three levels below it.

`core.excludesFile` — the global one, usually `~/.config/git/ignore` — is
**not** read. It lives behind `git config`, which means parsing git's config
file and its includes, and it is the one of the four that nearly nobody sets
for anything but editor backup files.

## The rules, as the format has them

```
# a comment                    a blank line and a `#` line are nothing
build/                         a directory, at any depth
*.log                          any depth, because there is no `/` in it
/build                         the root of *this* .gitignore's directory
doc/*.txt                      anchored too, because it has a `/` in it
a/**/b                         zero or more directories between
!keep.log                      re-include something an earlier line excluded
\#literal                      a `#` or `!` that is not a marker
```

**A pattern with no `/` in it matches at any depth**; one with a `/` anywhere
but the end is anchored to the directory its `.gitignore` sits in. A leading
`/` anchors and is otherwise nothing. A trailing `/` means directories only.

**The last matching pattern decides**, which is what makes `!` work at all,
and it is why the file order above is the order it is: a deeper `.gitignore`
is consulted after a shallower one and therefore beats it.

**An ignored directory is never walked into.** That is git's behaviour, it is
where the speed comes from, and it is also the reason `!` cannot re-include
something inside an excluded directory — nothing looks in there to find it.
Which means the walker has to ask about every *directory* as it descends
rather than only about files, and `Rules::ignored` is written to be asked that
way.

Within one path component: `*` is anything, `?` is one character, `[abc]` and
`[a-c]` and `[!abc]` are what they look like, and `\` escapes any of them.
`**` between slashes spans directories; anywhere else it is two stars, which
is one star, which is what git does with it.

**`out/**` and `out/` are not the same rule**, and the difference is the one
thing here that looks like a quibble and is not. The first excludes what is
*in* the directory; the second excludes the directory. So a later
`!out/keep.rs` works under the first — git's walk descends into `out` and
lists the file — and cannot work under the second, where nothing looks inside
to find it. A trailing `**` therefore matches one or more components rather
than zero or more, which is the whole of that distinction in one line of the
matcher. It was found by asking git, not by reading the documentation.

POSIX character classes — `[[:alpha:]]` — are not supported. They are in the
format and are vanishingly rare in real files; a `[` that is not a class is
matched literally, so a pattern using one fails to match rather than
misbehaving.

## Why a second glob matcher

`src/editorconfig.rs` has one already, and this is deliberately not it.

The two dialects agree on the easy half — `*`, `?`, classes, escapes — and
disagree on everything that follows. editorconfig has `{a,b}` alternation and
`{1..9}` ranges, which git treats as literal braces. git's `**` spans
directories only when it stands alone between slashes, where editorconfig's
spans them anywhere. Sharing one matcher would mean a flag per divergence, and
the divergences are exactly what a shared implementation would get quietly
wrong in one of its two callers.

So this one matches **component by component** — the path split at `/`, the
pattern split at `/` — which is what makes git's `**` rule fall out rather
than being a special case, and leaves each component to a plain fnmatch that
cannot cross a separator because there is no separator left in it to cross.

## Asking git

`tests/gitignore_git.rs` builds a repository — thirty-five files, three ignore
files, nineteen patterns between them — and compares `bi::files::walk` against
`git ls-files --others --exclude-standard`, which is every file git would
offer you and therefore every file the picker should.

Against the **walk**, not against `git check-ignore`, and the difference
matters: `check-ignore` reports `out/` as ignored under `out/**` while the
walk descends into it anyway. `check-ignore` answers a question about a
pattern; bi implements a walk. Asking the wrong one of the two is what made
the first version of this test pass while the code was wrong.

It skips itself, loudly, where there is no git to ask.

## Tests

- Comments, blanks, `!`, a trailing `/`, `\#` and `\!`, and trailing spaces
  that are not escaped.
- `out/**` leaves the directory walkable and `out/` does not, so a `!` reaches
  inside the first and not the second.
- Anchoring: no `/` matches at any depth, an inner `/` and a leading `/` do
  not.
- `a/**/b` matches with zero directories between and with two.
- Last match wins, including a `!` that re-includes and a later line that
  excludes again.
- A deeper `.gitignore` overrides the one above it.
- Directory-only patterns do not match a file of the same name.
- Within a component: `?`, ranges, negated classes, an escaped `*`, and a `*`
  that does not cross `/`.
- The walk prunes an ignored directory rather than filtering its files, which
  is visible as the files inside it being absent even when a later rule would
  have re-included one.
- `gitignore = false` lists everything.
- A repository root above the session root is found, and its rules apply.
