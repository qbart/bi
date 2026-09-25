# The command line

`bi` opens a file, or starts empty, and a few words after it are commands
that never open the editor: writing a config, seeding a project, printing
a sample. The rule for which is which is the one git and cargo use: **a
command word wins, and a file spelled like one is opened by its path**.

## Status

**Built.** `--help`, `--version`, the bare command words listing their
subcommands, `--` before a file.

## Forms

```
bi                              start empty
bi <path>                       open the file
bi -- <path>                    open the file, whatever it is called
bi --help  -h  help             this list
bi --version  -V                the version
bi config                       the config commands
bi config init                  the user config written, every default commented out
bi config edit                  the user config opened, written first when it is not there
bi debug                        the debug commands
bi debug init                   a project's .bi.toml seeded with launch configs
bi gen                          the generators
bi gen sample [schema|data|mapping]
                                the sample .bischema, .bidata and .bimapping written beside you
bi gen struct --lang <lang> -o <dir> -i <file>... [-m <file>...] [--pkg <name>] [--force] [-v]
                                the schema as code — see gen-struct.md
bi help <command>               one command's list: `bi help gen`
```

A command word alone — `bi gen`, `bi config`, `bi debug` — prints that
command's subcommands on stdout and exits clean, the way `cargo` does.
A subcommand that does not exist, or an argument one does not take,
prints what was expected on stderr and exits with a failure. An argument
starting with `-` that is not a flag above is refused the same way.

## A file named like a command

`bi gen` lists the generators even when a file called `gen` sits in the
directory: the command wins, because a command line that changes meaning
with the directory's contents cannot be typed from memory or put in a
script. The file is opened by a spelling that is not a bare word:

```
bi ./gen
bi -- gen
```

`--` ends the flags and the commands, as it does everywhere; everything
after it is a path. Vim has the same `--` and no subcommands at all, which
is why `vim gen` is always the file: its commands are flags (`-d`, `-R`,
`+cmd`), and a name can never collide with a flag because a name that
starts with `-` is what `--` is for. bi has subcommands because `config
init` and `gen sample` read better than flags, and pays for them with the
`./` rule, which every shell user already knows from `git` and `cargo`.

## Tests

- `bi --help`, `-h` and `help` print the list; `bi help gen` and `bi gen`
  print the generators; `bi config` and `bi debug` theirs.
- `bi --version` prints `bi <version>`.
- `bi gen` is the command even beside a file called `gen`; `bi -- gen` and
  `bi ./gen` open the file.
- `bi --nope` is refused naming `--help`; `bi gen nope` names `sample`;
  `bi a b` is refused.
