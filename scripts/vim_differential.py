"""Differential test: run the same normal-mode keys through real vim and
through bi, and compare the resulting file.

Not part of `cargo test` — it needs vim on PATH and drives the real binary
through a pty, so it is slow and has an external dependency. Run it by hand
after touching motions, operators or text objects:

    cargo build && python3 scripts/vim_differential.py [substring-filter]

vim is the oracle. Any disagreement is either a bi bug or a deliberate,
documented divergence — the KNOWN_DIVERGENCES table records the latter.
"""
import os
import pty
import select
import subprocess
import sys
import tempfile
import time

BI = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "target", "debug", "bi"
)

# Keys bi deliberately does not implement the way vim does, with the reason.
# Anything here still runs; it is reported separately rather than as a failure.
# Replace mode's Backspace cannot be tested here: `vim -es -c "normal ..."`
# inserts the DEL byte literally instead of processing it as a keypress. It is
# covered by a unit test instead.
KNOWN_DIVERGENCES = {
    "esc on search line": "same harness artifact as \"/ not found\": `-es` aborts"
                          " the sequence. Interactive vim agrees with bi.",
    "e past last": "same class: `e` at the last character of the buffer fails,"
                   " and `-es` aborts the sequence rather than running the"
                   " trailing x. Interactively vim beeps and stays put, which"
                   " is what bi does.",
    "ge at start": "as above, with `ge` at position 0 — nowhere further back.",
    "% no bracket": "as above: `%` with no bracket on the line fails in vim."
                    " bi stays put, which is what interactive vim does too.",
    "/ not found": "harness artifact, not a real difference: `vim -es -c normal`"
                   " aborts the whole key sequence when a search fails, so the"
                   " trailing x never runs. Interactive vim agrees with bi —"
                   " checked through a pty.",
}


def vim_run(text, keys):
    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as f:
        f.write(text)
        path = f.name
    subprocess.run(
        ["vim", "-u", "NONE", "-N", "-X", "-es", "-c", f"normal {keys}", "-c", "wq", path],
        capture_output=True,
    )
    out = open(path).read()
    os.unlink(path)
    return out


def vim_pty_run(text, keys):
    """The same oracle, driven interactively rather than through `-es`.

    `vim -es -c "normal ..."` is not vim enough for some sequences: a visual
    mode paste is silently dropped by it, which reads as "vim changes nothing"
    and would pin the wrong answer. Anything that behaves differently under
    `-es` goes in PTY_CASES and comes through here, where it is the real
    editor answering.

    Slow — a second or so per case — so it is not the default runner.
    """
    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as f:
        f.write(text)
        path = f.name

    pid, fd = pty.fork()
    if pid == 0:
        os.environ.update(TERM="xterm")
        # `-i NONE`: a viminfo file carries registers between runs, and a case
        # that pastes would then read what the *previous* case yanked.
        os.execv("/usr/bin/env", ["env", "vim", "-u", "NONE", "-N", "-X", "-i", "NONE", path])

    def drain():
        while select.select([fd], [], [], 0)[0]:
            try:
                if not os.read(fd, 65536):
                    return
            except OSError:
                return

    time.sleep(0.6)
    for k in list(keys) + [":", "w", "q", "\r"]:
        os.write(fd, k.encode())
        time.sleep(0.1)
        drain()

    end = time.time() + 6
    while time.time() < end:
        wpid, _ = os.waitpid(pid, os.WNOHANG)
        if wpid:
            break
        drain()
        time.sleep(0.1)
    else:
        os.kill(pid, 9)
        os.waitpid(pid, 0)

    out = open(path).read()
    os.unlink(path)
    return out


def bi_run(text, keys):
    """Sends `keys` one character at a time, then :wq."""
    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as f:
        f.write(text)
        path = f.name

    pid, fd = pty.fork()
    if pid == 0:
        os.environ.update(TERM="xterm-256color")
        os.execv(BI, ["bi", path])

    import fcntl
    import struct
    import termios

    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))

    def drain():
        while select.select([fd], [], [], 0)[0]:
            try:
                if not os.read(fd, 65536):
                    return
            except OSError:
                return

    time.sleep(0.55)
    for k in list(keys) + [":", "w", "q", "\r"]:
        os.write(fd, k.encode())
        time.sleep(0.16)
        drain()

    end = time.time() + 6
    while time.time() < end:
        wpid, _ = os.waitpid(pid, os.WNOHANG)
        if wpid:
            break
        drain()
        time.sleep(0.1)
    else:
        os.kill(pid, 9)
        os.waitpid(pid, 0)

    out = open(path).read()
    os.unlink(path)
    return out


CASES = [
    # (label, initial text, keys)
    ("dfx",            "a-b-x-c\n",            "dfx"),
    ("dtx",            "a-b-x-c\n",            "dtx"),
    ("dFa",            "abcXdef\n",            "4ldFa"),
    ("dTa",            "abcXdef\n",            "4ldTa"),
    ("2fx",            "x.x.x.x\n",            "2fxD"),
    ("f;",             "a,b,c,d\n",            "f,;x"),
    ("f;,",            "a,b,c,d\n",            "f,;,x"),
    ("f miss",         "abc\n",                "dfz"),
    ("t then ;",       "a.b.c.d\n",            "t.;x"),
    ("cfx",            "a-b-x-c\n",            "cfxZ\x1b"),

    # Step 3 motions: word ends, WORDs, first/last non-blank, %, paragraphs.
    ("yyp indent cursor", "    foo\nbar\n",     "yypx"),
    ("p linewise cursor",  "  a\nb\n",           "yyjpx"),
    ("de",             "foo bar baz\n",        "de"),
    ("de mid word",    "foobar baz\n",         "2lde"),
    ("de punct",       "foo.bar baz\n",        "de"),
    ("dE",             "foo.bar baz\n",        "dE"),
    ("d2e",            "foo bar baz qux\n",    "d2e"),
    ("dW",             "foo.bar baz\n",        "dW"),
    ("dB",             "foo.bar baz\n",        "$dB"),
    ("dge",            "foo bar baz\n",        "$dge"),
    ("dgE",            "foo.bar baz\n",        "$dgE"),
    ("e at word end",  "ab cd\n",              "lex"),
    ("e past last",    "ab\n",                 "eex"),
    ("ge at start",    "ab cd\n",              "gex"),
    ("d^",             "    foo bar\n",        "$d^"),
    ("^ then x",       "    foo\n",            "^x"),
    ("^ blank line",   "    \n",               "^x"),
    ("dg_",            "foo bar   \n",         "dg_"),
    ("g_ then x",      "foo bar   \n",         "g_x"),
    ("d% parens",      "a (b c) d\n",          "d%"),
    ("d% nested",      "((x)) y\n",            "d%"),
    ("d% from close",  "(ab) c\n",             "3ld%"),
    ("% no bracket",   "plain text\n",         "%x"),
    ("d}",             "one\ntwo\n\nthree\n",  "d}"),
    ("d{",             "one\n\ntwo\nthree\n",  "Gd{"),
    ("} then x",       "one\n\ntwo\n",         "}x"),

    ("diw start",      "foo bar baz\n",        "diw"),
    ("diw mid",        "foo bar baz\n",        "wdiw"),
    ("diw on space",   "foo bar\n",            "3ldiw"),
    ("daw",            "foo bar baz\n",        "wdaw"),
    ("daw last word",  "foo bar\n",            "wdaw"),
    ("diW",            "a foo.bar b\n",        "wdiW"),
    ("daW",            "a foo.bar b\n",        "wdaW"),
    ("ciw",            "foo bar\n",            "wciwZ\x1b"),
    ("yiw p",          "foo bar\n",            "yiwwP"),

    ('di"',            'say "hi there" ok\n',  'fhdi"'),
    ('da"',            'say "hi there" ok\n',  'fhda"'),
    ("di'",            "say 'hi' ok\n",        "fhdi'"),
    ('di" on quote',   'x "abc" y\n',          'f"di"'),
    ('di" empty',      'x "" y\n',             'f"di"'),

    ("di(",            "f(a, b)\n",            "fadi("),
    ("da(",            "f(a, b)\n",            "fada("),
    ("di( nested in",  "f(g(x), y)\n",         "fxdi("),
    ("da( nested in",  "f(g(x), y)\n",         "fxda("),
    ("di( from open",  "((a))\n",              "di("),
    ("dib",            "f(a, b)\n",            "fadib"),
    ("di{ multiline",  "fn a() {\n  body\n}\n", "jdi{"),
    ("da{ multiline",  "fn a() {\n  body\n}\n", "jda{"),
    ("di[",            "arr[idx]\n",           "fidi["),
    ("di( outside",    "a(b) c\n",             "$di("),

    ("dip",            "one\ntwo\n\nthree\n",  "dip"),
    ("dap",            "one\ntwo\n\nthree\n",  "dap"),
    ("dip 2nd",        "one\n\ntwo\nthree\n",  "3Gdip"),

    ("D",              "hello world\nkeep\n",  "4lD"),
    ("C",              "hello world\nkeep\n",  "4lCZ\x1b"),
    ("S",              "abc\ndef\n",           "SZ\x1b"),
    ("s",              "abc\n",                "sZ\x1b"),
    ("X",              "abc\n",                "2lX"),
    ("3rz",            "abcdef\n",             "3rz"),
    ("r short line",   "ab\n",                 "5rz"),
    ("5~",             "hello\n",              "5~"),
    ("J",              "foo\n    bar\nbaz\n",  "J"),
    ("3J",             "a\nb\nc\nd\n",         "3J"),
    ("J trailing sp",  "foo \nbar\n",          "J"),
    ("J blank next",   "foo\n\nbar\n",         "J"),

    # visual mode
    ("v then d",       "hello world\n",        "vlld"),
    ("v motion d",     "hello world\n",        "vwd"),
    ("viw d",          "foo bar baz\n",        "wviwd"),
    ("vi( d",          "f(a, b)\n",            "favi(d"),
    ("v$ d",           "hello world\n",        "v$d"),
    ("v0 d",           "hello world\n",        "6lv0d"),
    ("v then y then P","abc def\n",            "vlyP"),
    ("v then c",       "hello\n",              "vlcZ\x1b"),
    ("v then x",       "hello\n",              "vlx"),
    ("v o then d",     "hello world\n",        "6lvllohd"),
    ("v v cancels",    "hello\n",              "vlvx"),
    ("v esc cancels",  "hello\n",              "vl\x1bx"),
    ("V then d",       "one\ntwo\nthree\n",    "Vd"),
    ("V j d",          "one\ntwo\nthree\n",    "Vjd"),
    ("V then y P",     "one\ntwo\n",           "VyP"),
    ("V then c",       "one\ntwo\n",           "VcZ\x1b"),
    ("v across lines",  "one\ntwo\n",          "vjd"),

    # blockwise visual. \x16 is Ctrl-V.
    ("C-v d",          "abcdef\nghijkl\nmnopqr\n", "l\x16jjld"),
    ("C-v y then p",   "abcdef\nghijkl\nmnopqr\n", "l\x16jjly$p"),
    ("C-v y then P",   "abcdef\nghijkl\nmnopqr\n", "l\x16jjlyllP"),
    ("C-v over short", "abcdef\ngh\nmnopqr\n",     "3l\x16jjd"),
    ("C-v $ d",        "abcdef\ngh\nmnopqr\n",     "l\x16jj$d"),
    ("C-v c",          "abcdef\nghijkl\n",         "l\x16jlcZZ\x1b"),
    ("C-v I",          "abcdef\nghijkl\nmnopqr\n", "l\x16jjIZ\x1b"),
    ("C-v I short",    "abcdef\ngh\nmnopqr\n",     "3l\x16jjIZ\x1b"),
    ("C-v A",          "abcdef\nghijkl\nmnopqr\n", "l\x16jjAZ\x1b"),
    ("C-v A short",    "abcdef\ngh\nmnopqr\n",     "3l\x16jjAZ\x1b"),
    ("C-v $ A",        "abcdef\ngh\nmnopqr\n",     "l\x16jj$AZ\x1b"),
    ("C-v r",          "abcdef\nghijkl\nmnopqr\n", "l\x16jjlrz"),
    ("C-v o",          "abcdef\nghijkl\nmnopqr\n", "2l\x16jlohd"),
    ("C-v O",          "abcdef\nghijkl\nmnopqr\n", "2l\x16jlOhd"),
    ("C-v then d .",   "abcdef\nghijkl\nmnopqr\n", "l\x16jldj0l."),
    ("C-v esc",        "abcdef\nghijkl\n",         "l\x16jl\x1bx"),
    ("v r",            "abcdef\n",                 "vllrz"),
    ("V r",            "abc\ndef\n",               "Vjrz"),

    # undo leaves a cursor, never a selection
    ("C-v d then u",   "abcdef\nghijkl\nmnopqr\n", "l\x16jjldux"),
    ("v d then u",     "abcdef\n",                 "vlldux"),
    ("V d then u",     "abc\ndef\n",               "Vdux"),

    # replace mode
    ("R over text",    "abcdef\n",             "RXY\x1b"),
    ("R past the end", "ab\n",                 "RXYZ\x1b"),
    ("R then move",    "abcdef\n",             "llRZ\x1b"),

    # `.` — repeat the last change
    ("x then .",       "abcdef\n",             "x."),
    ("x then 3.",      "abcdef\n",             "x3."),
    ("3x then .",      "abcdefghi\n",          "3x."),
    ("dw then .",      "one two three four\n", "dw."),
    ("dw j .",         "a b c\nd e f\n",       "dwj0."),
    ("dd then .",      "1\n2\n3\n4\n",        "dd."),
    ("diw then .",     "foo bar baz\n",        "diwww."),
    ("ciw then .",     "aa bb cc\n",           "ciwXX\x1bww."),
    ("i then .",       "hello\n",              "iAB\x1b."),
    ("A then .",       "x\ny\n",               "AZ\x1bj."),
    ("o then .",       "one\n",                "oNEW\x1b."),
    ("O then .",       "one\n",                "ONEW\x1b."),
    ("r then .",       "aaaa\n",               "rzl."),
    ("J then .",       "1\n2\n3\n4\n",        "J."),
    ("~ then .",       "abcd\n",               "~."),
    ("p then .",       "ab\n",                 "ylp."),
    ("C then .",       "one two\nthree four\n", "CX\x1bj0."),
    (". after yank",   "one two three\n",      "dwyw."),
    (". after motion", "one two three\n",      "dwww."),
    (". after undo",   "one two three\n",      "dwu."),
    ("v then d then .","abcdefgh\n",           "vlld0."),
    ("V then d then .","1\n2\n3\n4\n5\n",     "Vjd."),

    # search
    ("/ lands on match",   "one two three\n",       "/three\rx"),
    ("d/ is exclusive",    "one two three four\n",  "d/three\r"),
    ("/ wraps",            "one two\n",             "$/one\rx"),
    ("? backward",         "one two three\n",       "$?two\rx"),
    ("d? backward",        "one two three\n",       "$d?two\r"),
    ("n repeats",          "foo boo zoo\n",         "/oo\rnx"),
    ("N reverses",         "aXbXcXd\n",             "/X\rnnNx"),
    ("n after ? keeps dir","a1a2a3\n",              "$?a\rnx"),
    ("* whole word",       "foo\nfoobar\nfoo\n",   "*x"),
    ("* wraps to itself",  "foo bar\n",             "*x"),
    ("# backward",         "foo\nbar\nfoo\n",      "G#x"),
    ("smartcase lower",    "Foo foo\n",             "/foo\rx"),
    ("smartcase upper",    "foo Foo\n",             "/Foo\rx"),
    ("c/ then type",       "one two three\n",       "c/three\rZ\x1b"),
    ("y/ then P",          "one two three\n",       "y/three\rP"),
    ("/ not found",        "abc\n",                 "/zzz\rx"),
    ("esc on search line", "abc\n",                 "/zz\x1bx"),
    ("bare / repeats",     "a1a2a3\n",              "/a\r/\rx"),
    ("/ then . repeats",   "aXbXc\n",               "d/X\r."),

    # vim's "exclusive motion ending in column 1" rule, which the README used
    # to say was only approximated
    ("dw ending col 1",    "a b\ncd\n",            "2ldw"),
    ("dw into blank line", "foo\n\nbar\n",         "dw"),
    ("dw from indent",     "  foo\nbar\n",         "dw"),
    ("dw trailing spaces", "foo   \nbar\n",        "dw"),
    ("dw at line end",     "hello\nworld\n",       "3ldw"),

    # regressions on what already existed
    ("dw",             "foo bar baz\n",        "dw"),
    ("d3w",            "a b c d\n",            "d3w"),
    ("cw",             "foo   bar\n",          "cwZ\x1b"),
    ("dd",             "one\ntwo\nthree\n",    "dd"),
    ("2dd",            "one\ntwo\nthree\n",    "2dd"),
    ("dG",             "one\ntwo\nthree\n",    "jdG"),
    ("dgg",            "one\ntwo\nthree\n",    "jdgg"),
    ("d$",             "hello world\n",        "6ld$"),
    ("d0",             "hello world\n",        "6ld0"),
    ("x",              "abc\n",                "x"),
    ("p charwise",     "abc\n",                "ylp"),
    ("yyp",            "one\ntwo\n",           "yyp"),
]

# Cases `vim -es` cannot answer for, run against interactive vim instead. A
# visual mode paste is one of them: `-es` drops it and leaves the file
# untouched, so the whole table would "pass" against a vim that did nothing.
#
# See `docs/specs/registers.md` — every row of the kinds table is here.
PTY_CASES = [
    ("v p",            "one two\nthree\n",         "yiwwviwp"),
    ("v P",            "one two\nthree\n",         "yiwwviwP"),
    ("v p linewise",   "one\ntwo three\n",         "yyjviwp"),
    ("V p charwise",   "one two\nthree\nfour\n",   "yiwjVp"),
    ("V p",            "one\ntwo\nthree\n",        "yyjVp"),
    ("v 3p",           "one two\nthree\n",         "yiwwviw3p"),
    # `p` puts what it displaced on the ring, so the second one pastes it back.
    ("v p swaps",      "one two\nthree\n",         "yiwwviwpjviwp"),
    # `P` does not, so the same entry lands on both words.
    ("v P keeps",      "aa bb cc\n",               "yiwwviwPwviwP"),
    ("v$ p",           "abcdef\nxy\n",             "yyj0v$p"),
    ("v j p",          "one two three\nfour\n",    "yiwwvjp"),
    ("C-v p charwise", "abc\ndef\nghi\n",          "yiwj0\x16jlp"),
    ("C-v p linewise", "abc\ndef\nghi\n",          "yyj0\x16jlp"),
    ("C-v p block",    "abcd\nefgh\nijkl\nmnop\n", "\x16jly2j0\x16lp"),
    ("v p block",      "abcd\nefgh\nijkl\nmnop\n", "\x16jly2j0vlp"),
    # Where the cursor ends up, read off by deleting the character under it.
    ("v p cursor",     "one two\nthree\n",         "yiwwviwpx"),
    ("V p cursor",     "one\n  two\nthree\n",      "jjyy0kVpx"),
    ("v p lines cur",  "one\ntwo three\n",         "yyjviwpx"),
]


def main():
    only = sys.argv[1] if len(sys.argv) > 1 else None
    same = diverged = 0
    failures = []
    cases = [(case, vim_run) for case in CASES] + [(case, vim_pty_run) for case in PTY_CASES]
    for (label, text, keys), oracle in cases:
        if only and only not in label:
            continue
        # vim -es starts on the LAST line; bi starts on the first. Normalise
        # both to line 1, column 0 so the cases mean what they say.
        v = oracle(text, "gg0" + keys)
        b = bi_run(text, "gg0" + keys)
        if v == b:
            same += 1
            print(f"  ok    {label:<16} -> {b!r}")
        elif label in KNOWN_DIVERGENCES:
            diverged += 1
            print(f"  note  {label:<16} vim={v!r} bi={b!r}\n        {KNOWN_DIVERGENCES[label]}")
        else:
            failures.append((label, keys, text, v, b))
            print(f"  DIFF  {label:<16} vim={v!r}\n        {'':16} bi={b!r}")

    print(f"\n{same} match vim, {diverged} known divergences, {len(failures)} differ")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
