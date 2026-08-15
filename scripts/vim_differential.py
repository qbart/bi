"""Differential test: run the same normal-mode keys through real vim and
through bee, and compare the resulting file.

Not part of `cargo test` — it needs vim on PATH and drives the real binary
through a pty, so it is slow and has an external dependency. Run it by hand
after touching motions, operators or text objects:

    cargo build && python3 scripts/vim_differential.py [substring-filter]

vim is the oracle. Any disagreement is either a bee bug or a deliberate,
documented divergence — the KNOWN_DIVERGENCES table records the latter.
"""
import os
import pty
import select
import subprocess
import sys
import tempfile
import time

BEE = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "target", "debug", "bee"
)

# Keys bee deliberately does not implement the way vim does, with the reason.
# Anything here still runs; it is reported separately rather than as a failure.
# Replace mode's Backspace cannot be tested here: `vim -es -c "normal ..."`
# inserts the DEL byte literally instead of processing it as a keypress. It is
# covered by a unit test instead.
KNOWN_DIVERGENCES = {
    "dw at line end": "bee stops at the line end rather than vim's full "
                      "'end in column 1' rule (documented in README)",
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


def bee_run(text, keys):
    """Sends `keys` one character at a time, then :wq."""
    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as f:
        f.write(text)
        path = f.name

    pid, fd = pty.fork()
    if pid == 0:
        os.environ.update(TERM="xterm-256color")
        os.execv(BEE, ["bee", path])

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

    # replace mode
    ("R over text",    "abcdef\n",             "RXY\x1b"),
    ("R past the end", "ab\n",                 "RXYZ\x1b"),
    ("R then move",    "abcdef\n",             "llRZ\x1b"),

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


def main():
    only = sys.argv[1] if len(sys.argv) > 1 else None
    same = diverged = 0
    failures = []
    for label, text, keys in CASES:
        if only and only not in label:
            continue
        # vim -es starts on the LAST line; bee starts on the first. Normalise
        # both to line 1, column 0 so the cases mean what they say.
        v = vim_run(text, "gg0" + keys)
        b = bee_run(text, "gg0" + keys)
        if v == b:
            same += 1
            print(f"  ok    {label:<16} -> {b!r}")
        elif label in KNOWN_DIVERGENCES:
            diverged += 1
            print(f"  note  {label:<16} vim={v!r} bee={b!r}\n        {KNOWN_DIVERGENCES[label]}")
        else:
            failures.append((label, keys, text, v, b))
            print(f"  DIFF  {label:<16} vim={v!r}\n        {'':16} bee={b!r}")

    print(f"\n{same} match vim, {diverged} known divergences, {len(failures)} differ")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
