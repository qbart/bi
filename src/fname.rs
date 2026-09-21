//! `%`, `#`, their modifiers and the `$` aliases on the ex line. Pure:
//! the editor says what the current and alternate files are, this says
//! what the line means. See `docs/specs/filenames.md`.

use std::path::Path;

/// `%` is `current`, `#` is `alternate`; `:p`, `:h`, `:t`, `:r`, `:e`
/// after either pick a part and chain; `$path`, `$dir`, `$name`, `$stem`,
/// `$ext`, `$base` are the same parts by name. `\%`, `\#` and `\$` are
/// literal, and so is a `$` that starts no alias.
pub fn expand(
    text: &str,
    current: Option<&str>,
    alternate: Option<&str>,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if matches!(chars.get(i + 1), Some('%' | '#' | '$')) => {
                out.push(chars[i + 1]);
                i += 2;
            }
            '%' | '#' => {
                let (file, what) = match c {
                    '%' => (current, "no file name for %"),
                    _ => (alternate, "no alternate file for #"),
                };
                let mut value = file.ok_or(what)?.to_string();
                i += 1;
                while chars.get(i) == Some(&':')
                    && let Some(&m) = chars.get(i + 1)
                    && let Some(part) = modify(&value, m)
                {
                    value = part;
                    i += 2;
                }
                out.push_str(&value);
            }
            '$' => {
                let word: String =
                    chars[i + 1..].iter().take_while(|c| c.is_ascii_lowercase()).collect();
                let mods: Option<&str> = match word.as_str() {
                    "path" => Some("p"),
                    "dir" => Some("h"),
                    "name" => Some("t"),
                    "stem" => Some("tr"),
                    "ext" => Some("e"),
                    "base" => Some("r"),
                    _ => None,
                };
                match mods {
                    Some(mods) => {
                        let mut value =
                            current.ok_or_else(|| format!("no file name for ${word}"))?.to_string();
                        for m in mods.chars() {
                            value = modify(&value, m).expect("a known modifier");
                        }
                        out.push_str(&value);
                        i += 1 + word.len();
                    }
                    None => {
                        out.push('$');
                        i += 1;
                    }
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// One of vim's modifiers applied to a path; `None` for a letter that is
/// not one, so the text after `:` stays text.
fn modify(value: &str, m: char) -> Option<String> {
    let path = Path::new(value);
    Some(match m {
        'p' => {
            std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()).display().to_string()
        }
        'h' => match path.parent() {
            Some(parent) if parent.as_os_str().is_empty() => ".".to_string(),
            Some(parent) => parent.display().to_string(),
            None => value.to_string(),
        },
        't' => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| value.to_string()),
        'r' => path.with_extension("").display().to_string(),
        'e' => path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn x(s: &str) -> String {
        expand(s, Some("assets/atlas.png"), Some("src/main.rs")).unwrap()
    }

    #[test]
    fn the_modifiers_pick_the_file_apart() {
        assert_eq!(x("%"), "assets/atlas.png");
        assert_eq!(x("%:h"), "assets");
        assert_eq!(x("%:t"), "atlas.png");
        assert_eq!(x("%:r"), "assets/atlas");
        assert_eq!(x("%:e"), "png");
        assert_eq!(x("%:t:r"), "atlas");
        assert_eq!(x("%:r:r"), "assets/atlas", "no second extension to take");
        assert_eq!(x("%:r.normal.%:e"), "assets/atlas.normal.png");
        assert_eq!(x("%:h/normal.png"), "assets/normal.png");
        assert_eq!(x("#:r.rs"), "src/main.rs");
        assert_eq!(x("#:t"), "main.rs");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(x("%:p"), cwd.join("assets/atlas.png").display().to_string());
        assert_eq!(x("%:p:h"), cwd.join("assets").display().to_string());
    }

    #[test]
    fn the_aliases_are_the_same_parts() {
        assert_eq!(x("$dir/normal.png"), "assets/normal.png");
        assert_eq!(x("$name"), "atlas.png");
        assert_eq!(x("$base.normal.$ext"), "assets/atlas.normal.png");
        assert_eq!(x("$stem.md"), "atlas.md");
        assert_eq!(x("$path"), x("%:p"));
        assert_eq!(x("$ext$ext"), "pngpng");
    }

    #[test]
    fn the_edges_are_vims() {
        let e = |s: &str, f: &str| expand(s, Some(f), None).unwrap();
        assert_eq!(e("%:r", "Makefile"), "Makefile");
        assert_eq!(e("%:e", "Makefile"), "");
        assert_eq!(e("%:h", "Makefile"), ".");
        assert_eq!(e("%:r", "archive.tar.gz"), "archive.tar");
        assert_eq!(e("%:r:r", "archive.tar.gz"), "archive");
        assert_eq!(e("%:e", "archive.tar.gz"), "gz");
        assert_eq!(e("%:h", "/atlas.png"), "/");
        assert_eq!(e("%:x", "a.png"), "a.png:x", "an unknown modifier is text");
    }

    #[test]
    fn escapes_and_strangers_are_literal() {
        assert_eq!(x(r"\% \# \$dir"), "% # $dir");
        assert_eq!(x("echo $HOME $dirt"), "echo $HOME $dirt", "$dirt is not $dir");
        assert_eq!(x("100%"), "100assets/atlas.png", "vim's rule too: % is always the file");
        assert!(expand("cat %", None, None).unwrap_err().contains("no file name for %"));
        assert!(expand("diff #", Some("a"), None).unwrap_err().contains("no alternate file for #"));
        assert!(expand("$dir", None, None).unwrap_err().contains("no file name for $dir"));
    }
}
