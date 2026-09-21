# File name placeholders

`:!` has expanded `%` to the current file since the shell spec. `:w` and
`:e` did not, which is where it is most wanted: the normal map beside the
height map, the JSON beside the Rust, the copy with a suffix. Now every
path on the ex line reads the same placeholders, and vim's modifiers pick
the file apart.

## Status

**Built.**

## The placeholders

`%` is the current file and `#` the alternate, as before. A modifier
after either picks a part, and modifiers chain left to right:

| spelling | means                          | alias   |
|----------|--------------------------------|---------|
| `%`      | the file as it was opened      |         |
| `%:p`    | its full path                  | `$path` |
| `%:h`    | the directory (head)           | `$dir`  |
| `%:t`    | the file name (tail)           | `$name` |
| `%:r`    | the path without its extension | `$base` |
| `%:e`    | the extension, no dot          | `$ext`  |
| `%:t:r`  | the name without its extension | `$stem` |

The `$` names are the Ruby `File` vocabulary — `dirname`, `basename`,
`extname` — for anyone who thinks in it; each is exactly one of the `%`
spellings and the two can be mixed. `\%`, `\#` and `\$` are the literal
characters, and a `$` that is not followed by one of the six names is left
alone, so `:!echo $HOME` still asks the shell.

## Samples

Opened as `assets/atlas.png` from `/home/kiwi/game`:

```
%          assets/atlas.png
%:p        /home/kiwi/game/assets/atlas.png       $path
%:h        assets                                 $dir
%:t        atlas.png                              $name
%:r        assets/atlas                           $base
%:e        png                                    $ext
%:t:r      atlas                                  $stem
%:p:h      /home/kiwi/game/assets
%:r:r      assets/atlas          (one extension gone, no more to take)
```

And what the commands do with them:

```
:w %:r.normal.%:e        writes assets/atlas.normal.png
:w $base.normal.$ext     the same
:w %:h/normal.png        writes assets/normal.png
:w $dir/normal.png       the same
:e %:r.json              opens assets/atlas.json
:e $stem.md              opens atlas.md — in the working directory, not beside
:vs #:r.rs               splits on the alternate's .rs neighbour
:!convert % %:r.jpg      the shell gets both names
```

The edges are vim's: `:r` on a name with no extension changes nothing,
`:e` on one is empty, `:h` on a bare name is `.`, and `archive.tar.gz`
loses one extension per `:r`.

## Where it applies

Every ex command that takes a path: `:w`, `:wq`, `:e`, `:sp`, `:vs` and
`:!`. The current file is the focused window's, whether it holds a buffer
or an image — the normal map's `:w %:r.normal.%:e` is asked of a picture.
From the `:!` log, `%` still means the file the command was run from, as
`shell.md` says. A window with no file — an empty buffer, a tree — says
`no file name for %`, and `#` with no alternate says the same of `#`.

The expansion is one pure function, `fname::expand`, and `shell::expand`
is it under its old name.

## Tests

- Each modifier and each alias on `assets/atlas.png`, chained ones
  included; the edges above.
- `\%` and `$HOME` are literal; `%` with no file is refused.
- `:w %:r.normal.%:e` on an image writes beside it and re-points the
  window; `:e %:r.txt` on a buffer opens the neighbour; `:vs $dir/other`
  splits on it.
