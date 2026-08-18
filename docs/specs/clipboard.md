# The system clipboard, and pasting into the terminal

Two problems that look unrelated and are the same one: text crossing the
boundary between bi and everything else.

Pasting into bi with the terminal's own paste is slow enough to watch —
characters appear one at a time, like a typewriter. And there is no way to get
text *out* of bi into another program, or in from one, except through whatever
the terminal is willing to do with a mouse.

## Status

**Specified and built.**

## Why the typewriter

Nothing is wrong with the buffer. Bracketed paste was never enabled, so the
terminal has no way to say "this is a paste" and sends it as what it looks
like: keystrokes. Every character then costs a full turn of the loop —
`translate`, `on_key`, `apply` with its own undo entry and its own syntax edit,
`settle` with its reparse — and, worst of all, a full `term.draw` before the
loop will even look at the next event. A 2 KB paste is 2000 redraws.

Three fixes, all in the frontend, none of which the core learns about:

**Ask for bracketed paste.** `EnableBracketedPaste` on the way in,
`DisableBracketedPaste` on the way out, beside the alternate screen — including
in the panic hook, because a terminal left in bracketed-paste mode pastes
`~` escape noise into the next shell prompt.

**One event, one action.** `Event::Paste(String)` becomes a single
`Action::InsertText`, which is one insertion, one undo entry and one reparse.
It is an `is_session_key`, so a paste inside an insert session is part of what
`.` replays — which is what typing it would have been.

**Draw when the queue is empty.** `event::poll(ZERO)` before drawing: while
more events are already waiting, keep applying them. This is what makes a
terminal that does *not* support bracketed paste fast too, and it fixes held-down
`j` at the same time — the screen is only ever behind by the events that have
already arrived, and the frame the user sees is the one after all of them.

The three are independent. The first two make a paste one operation; the third
makes any burst of input one frame.

## Where the text goes

`"+y` yanks to the system clipboard and `"+p` pastes from it, as in vim.
`Sink` gains `System` beside `Ring` and `BlackHole`, so the register grammar
already in `input.rs` reaches it with no new state: `"` is pending, `+` names
the sink, and the rest of the command is unchanged.

Explicit rather than mirrored. `clipboard=unnamedplus` — every yank, delete and
change also going to the system clipboard — was rejected: a delete is not a
copy, and a terminal editor that silently exports every `dd` to the desktop
clipboard is a surprise in the direction that cannot be undone. The option
remains addable later; it needs the register to exist first either way.

`"*` is accepted as a spelling of `"+`. X11's distinction between the primary
selection and the clipboard is real, but OSC 52 addresses them with one code and
one of them is not worth a different letter here.

## The boundary

The core does not learn what a clipboard is, the same way it does not learn what
a filesystem is. It gets a trait, and the frontend supplies it:

```rust
pub trait SystemClipboard {
    fn set(&self, text: &str) -> anyhow::Result<()>;
    fn get(&self) -> anyhow::Result<Option<String>>;
}
```

`Editor::set_clipboard` installs one, exactly as `load_config` installs a
`ConfigSource`. Without one — an embedder that has not supplied it, or every
test — `"+y` says "no system clipboard" and changes nothing. That is a
diagnostic, not a panic, and it is the same shape as a config that will not
load.

`Ok(None)` from `get` means the clipboard is empty or the terminal declined to
answer, which is a real outcome and not an error.

## OSC 52, and what it costs

The TUI's implementation is escape sequences: `ESC ] 52 ; c ; <base64> BEL`
writes the clipboard, and the terminal does the work.

Chosen over the native route (`arboard` and the X11/Wayland stack behind it) for
one reason that outweighs the rest: **it works over SSH**. bi runs in a
terminal, and a terminal is very often not on the machine the user is sitting
at. A native clipboard library talks to a display server that is on the wrong
end of the connection, where OSC 52 travels the same path the text does. It also
costs no dependency, which matters for a project that already flinches at a
23.6 MB binary and a C toolchain.

What it costs is reading. Writing is widely supported; *reading* the clipboard
with OSC 52 requires the terminal to write back to the program, which many
terminals refuse by default because a program that can read your clipboard can
read the password you copied a moment ago. So `"+p` may find nothing through no
fault of bi's.

Rather than hang waiting for a reply that will never come, the read is
best-effort with a short deadline and reports what happened:

```
the terminal did not answer — many refuse to read the clipboard
```

An embedder that wants a native clipboard implements the trait with one; that is
the whole point of the trait. A `--clipboard=native` build behind a Cargo
feature is the obvious future step and needs nothing here to change.

## Tests

The trait is what makes this testable at all: a fake `SystemClipboard` in a
`RefCell<String>` proves `"+y` writes it, `"+p` reads it, and neither disturbs
the ring. The escape sequence itself is tested where it is built — the base64
encoding, the terminator — and never by talking to a real terminal.

- `"+y` and `"*y` reach the same sink; the ring is untouched by both.
- `"+p` with no clipboard installed reports and does nothing.
- `"_` still discards, and `"` followed by anything else is still a no-op.
- `Action::InsertText` is one undo step, not one per character.
- A paste while a `[No Name]` is displayed does not sweep it away mid-edit.
