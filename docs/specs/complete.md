# Completion

Insert-mode completion on the float surface (`docs/specs/hover.md`), fed by
the LSP core. The largest feature bi has grown, and the one the surface was
designed against.

## Status

**Built.** Snippets collapse to plain text; no `completionItem/resolve`.

## The state machine

`src/complete.rs`, in the picker's mold — state only, never draws:

```rust
pub struct Completion {
    items: Vec<Item>,          // what the server offered, once per request
    matches: Vec<usize>,       // filtered locally as the word grows
    selected: usize,
    scroll: usize,
    pub replace: Range<usize>, // chars an accept replaces: word start..cursor
    pub incomplete: bool,      // the server wants re-asking as you type
    ...
}
```

`Item` carries the label, the text an accept inserts (snippet syntax already
stripped: `${1:x}` → `x`, `$0` → nothing, `${1|a,b|}` → `a`), the kind, and
the server's `filterText`/`sortText` with the label as fallback for both.

**Filtering is local and bucketed**: prefix matches first, subsequence
matches after, each bucket ordered by `sortText` — the cheap rule that puts
`pos` before `expose_position` when you typed `pos`. Case-insensitive, like
the picker. The word is re-read from the buffer on every keystroke
(`replace.start..cursor`), so the filter cannot drift from the text.

## Triggering, and the race that shapes it

The menu opens by itself: typing an identifier char with the menu closed, or
one of the server's `triggerCharacters` (`.`, `::`). `Ctrl-N` with the menu
closed summons it manually — vim's own key for exactly this.

**`apply` never sends the request.** A completion triggered by a typed char
must reach the server *after* the `didChange` that carries that char, and
requests go down the pipe the moment they are filed — so `apply` only marks
what is wanted, and `settle` sends it after the edit drain. The mark also
carries whether the ask was manual: a manual summon that cannot be served
says so on the status line, an automatic one stays silent — status noise on
every keystroke is worse than no completion.

Stale answers die twice over: a request counter (only the newest request's
answer is accepted) and the mode/buffer/cursor checks at arrival. When the
server said `isIncomplete`, continued typing re-requests; otherwise the menu
narrows locally for free.

Multi-cursor insert does not trigger: an accept would need one edit per
cursor, and half-applying it is worse than none. A later feature can lift
this.

## Keys, intercepted so `Input` stays stateless

The insert-mode handler emits its ordinary actions; `Editor::apply`
reroutes them while the menu is open:

| Key | Menu open | Menu closed |
|---|---|---|
| `Ctrl-N` / `Ctrl-P` | next / previous, wrapping | summon / nothing |
| `Tab` / `Enter` | accept | indent / newline, as ever |
| `Shift-Tab` | previous | outdent |
| `Esc` | close, **stay in insert** | leave insert |
| typing | narrows the filter | may trigger |
| arrows, motions | close | — |

`Input` never learns whether a menu is open — the same reasoning that keeps
the keymap grammar free of editor state everywhere else.

## Accepting

The selected item replaces `replace.start..cursor` — bi's own word range,
recomputed at accept, which sidesteps stale server ranges entirely.
`additionalTextEdits` (rust-analyzer's auto-imports) apply first, bottom-up,
with the word range mapped through them; the cursor lands after the inserted
text. All of it joins the still-open insert-mode undo group, so the typing
run and the completion undo as one — the same promise `Esc` has always
closed over.

Known and accepted: `.` repeats the typed keystrokes, not the accepted
completion — the repeat records commands, and the accept is a response
arriving between them.

## Deliberately not here

Snippet expansion with tab-stops, `completionItem/resolve` (lazy docs and
edits), a documentation side-panel, signature help (next, on these same
rails), per-item preview, and fuzzy scoring beyond the two buckets.
