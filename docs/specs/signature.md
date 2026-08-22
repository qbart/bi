# Signature help

The parameters float: type `(` in a call and the signature appears above the
cursor with the parameter you are on highlighted, follows you from comma to
comma, and leaves when the call does. The third client of the float surface
(`docs/specs/hover.md`), and the cheapest — everything it needs was built by
hover and completion.

## Status

**Built.**

## Lifecycle

Insert-mode only, and automatic only — there is no key and no ex command,
because the question "what goes here" is only ever asked mid-call, mid-typing.

- **Opens** when a typed char is one of the server's
  `signatureHelpProvider.triggerCharacters` — `(` and `,` for rust-analyzer.
- **Follows** by re-asking: while the float is up, every insert and
  backspace re-requests, and the server's answer moves the highlight. The
  alternative — tracking commas and nesting client-side — is a parser bi
  would have to keep right in every language at once; the server already has
  one.
- **Closes** itself the same way: the server answers null once the cursor
  leaves the call (typing `)` included), and null means close. Leaving
  insert mode or moving the cursor closes it locally.

The ask rides the same mark-then-settle path as completion, for the same
race: the request must trail the `didChange` carrying the char that
triggered it. Stale answers die by the same request-counter rule.

The float coexists with the completion menu — signature above the cursor,
menu below the word — which is exactly the moment both are wanted: picking
an argument while being told which parameter it is.

## What shows

One line: the active signature's label, with the active parameter's span in
**bold underline over the popup style** — emphasis derived from the float's
own key rather than a new theme entry, because it is the same ink made
louder, not a new thing on screen. When the server offers several
signatures, a dim ` (1/3)` follows the label; bi shows the one the server
called active and does not page through them — the case is rare and the
keys it would cost are not.

The parameter's span comes in two wire shapes — a substring, or a pair of
offsets in the negotiated encoding — and both resolve to a char range in the
label at the registry boundary, like every other wire position.

## Deliberately not here

Signature cycling keys, parameter documentation, `retriggerCharacters` as a
distinct set (re-asking while open covers them), and the context's
`activeSignatureHelp` echo.
