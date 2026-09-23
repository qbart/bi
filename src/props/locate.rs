//! Where a value sits in a data file's text: a small JSON walker that
//! goes down `instances` to an instance and then key by key and index by
//! index, giving the byte span of the value as written, whatever the
//! layout. What opens the curve tool on the right bracket. See
//! `docs/specs/props.md`.

use super::view::Seg;

/// The byte span `(start, end)` of the value at `segs` below instance
/// `index`, `end` one past its last byte; `None` when the path is not
/// in the text.
pub fn value_span(text: &str, index: usize, segs: &[Seg]) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let root = skip_ws(bytes, 0);
    let list = member(bytes, root, "instances")?;
    let mut at = item(bytes, list, index)?;
    for seg in segs {
        at = match seg {
            Seg::Key(k) => member(bytes, at, k)?,
            Seg::Index(n) => item(bytes, at, *n)?,
        };
    }
    Some((at, value_end(bytes, at)?))
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

/// One past the closing quote of the string opening at `i`.
fn string_end(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'"') {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return Some(j + 1),
            _ => j += 1,
        }
    }
    None
}

/// One past the value opening at `i`.
fn value_end(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i)? {
        b'"' => string_end(b, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut j = i;
            while j < b.len() {
                match b[j] {
                    b'"' => j = string_end(b, j)?,
                    b'{' | b'[' => {
                        depth += 1;
                        j += 1;
                    }
                    b'}' | b']' => {
                        depth -= 1;
                        j += 1;
                        if depth == 0 {
                            return Some(j);
                        }
                    }
                    _ => j += 1,
                }
            }
            None
        }
        _ => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b']' | b'}' | b' ' | b'\t' | b'\n' | b'\r')
            {
                j += 1;
            }
            Some(j)
        }
    }
}

/// The start of the value of `key` in the object opening at `i`.
fn member(b: &[u8], i: usize, key: &str) -> Option<usize> {
    if b.get(i) != Some(&b'{') {
        return None;
    }
    let mut j = skip_ws(b, i + 1);
    loop {
        if b.get(j) == Some(&b'}') {
            return None;
        }
        let end = string_end(b, j)?;
        let name = std::str::from_utf8(&b[j + 1..end - 1]).ok()?;
        j = skip_ws(b, end);
        if b.get(j) != Some(&b':') {
            return None;
        }
        j = skip_ws(b, j + 1);
        // Keys the writer escapes are never field names, so a plain
        // comparison is the whole test.
        if name == key {
            return Some(j);
        }
        j = skip_ws(b, value_end(b, j)?);
        if b.get(j) == Some(&b',') {
            j = skip_ws(b, j + 1);
        }
    }
}

/// The start of item `n` of the array opening at `i`.
fn item(b: &[u8], i: usize, n: usize) -> Option<usize> {
    if b.get(i) != Some(&b'[') {
        return None;
    }
    let mut j = skip_ws(b, i + 1);
    let mut k = 0;
    loop {
        if b.get(j) == Some(&b']') {
            return None;
        }
        if k == n {
            return Some(j);
        }
        j = skip_ws(b, value_end(b, j)?);
        if b.get(j) == Some(&b',') {
            j = skip_ws(b, j + 1);
        }
        k += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HAND: &str = r#"{ "$dialect": "bi/1", "$schema": "g.bischema",
  "instances": [ {"$type":"A","$id":"one","n":1},
    { "$type": "A", "$id": "two", "name": "a [b] \"c\"", "off": {"x": [1, 2, {"y": 3}]},
      "curve": [[0, 0, 1, 0, true], [1, 1, 1, 1, true]] } ] }"#;

    fn at(segs: &[Seg]) -> &'static str {
        let (s, e) = value_span(HAND, 1, segs).expect("found");
        &HAND[s..e]
    }

    #[test]
    fn finds_fields_items_and_nested_values_in_any_layout() {
        assert_eq!(at(&[Seg::Key("name".into())]), r#""a [b] \"c\"""#);
        assert_eq!(at(&[Seg::Key("off".into()), Seg::Key("x".into()), Seg::Index(1)]), "2");
        assert_eq!(
            at(&[
                Seg::Key("off".into()),
                Seg::Key("x".into()),
                Seg::Index(2),
                Seg::Key("y".into())
            ]),
            "3"
        );
        assert_eq!(at(&[Seg::Key("curve".into())]), "[[0, 0, 1, 0, true], [1, 1, 1, 1, true]]");
        assert_eq!(at(&[Seg::Key("curve".into()), Seg::Index(1)]), "[1, 1, 1, 1, true]");
        let (s, e) = value_span(HAND, 0, &[Seg::Key("n".into())]).unwrap();
        assert_eq!(&HAND[s..e], "1");
        assert_eq!(value_span(HAND, 0, &[Seg::Key("missing".into())]), None);
        assert_eq!(value_span(HAND, 2, &[]), None);
        assert_eq!(value_span("{}", 0, &[]), None);
    }
}
