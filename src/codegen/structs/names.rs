//! Names: the schema's identifiers spelled the way each language spells
//! them, kept out of the way of its keywords, and refused when two of
//! them land on one. See `docs/specs/gen-struct.md` §Names.

use super::Lang;

/// The words of a name: split on `_`, `-`, space and any other
/// non-alphanumeric, and on `fooBar` and `HTTPServer` boundaries. Each
/// word comes back lower-cased.
fn words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let prev = if i > 0 { Some(chars[i - 1]) } else { None };
        let next = chars.get(i + 1).copied();
        let boundary = match prev {
            Some(p) if p.is_alphanumeric() => {
                // aB, 1B: lower/digit to upper
                (c.is_uppercase() && (p.is_lowercase() || p.is_ascii_digit()))
                    // ABc: the last upper of a run before a lower starts a word
                    || (c.is_uppercase() && p.is_uppercase() && next.is_some_and(|n| n.is_lowercase()))
            }
            _ => false,
        };
        if boundary && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn capitalise(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// `foo_bar`, `fooBar`, `very rare` → `FooBar`, `FooBar`, `VeryRare`;
/// `1st` → `_1st`; `""` → `_empty`.
pub fn pascal(s: &str) -> String {
    let ws = words(s);
    if ws.is_empty() {
        return "_empty".into();
    }
    let out: String = ws.iter().map(|w| capitalise(w)).collect();
    lead(out)
}

/// `FooBar`, `fooBar`, `very rare` → `foo_bar`; `HTTPServer` → `http_server`.
pub fn snake(s: &str) -> String {
    let ws = words(s);
    if ws.is_empty() {
        return "_empty".into();
    }
    lead(ws.join("_"))
}

/// `fooBar` → `FOO_BAR`.
pub fn screaming(s: &str) -> String {
    snake(s).to_uppercase()
}

/// The name made an identifier and nothing more: every other character
/// becomes `_`, a leading digit gets a `_` in front, the empty string is
/// `_empty`. Casing is kept — this is the "as-is" spelling.
pub fn identifier(s: &str) -> String {
    if s.is_empty() {
        return "_empty".into();
    }
    lead(s.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect())
}

/// A leading digit gets a `_` in front.
fn lead(s: String) -> String {
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) { format!("_{s}") } else { s }
}

const C_RESERVED: &[&str] = &[
    "alignas",
    "alignof",
    "auto",
    "bool",
    "break",
    "case",
    "char",
    "const",
    "constexpr",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "false",
    "float",
    "for",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "nullptr",
    "register",
    "restrict",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "static_assert",
    "struct",
    "switch",
    "thread_local",
    "true",
    "typedef",
    "typeof",
    "typeof_unqual",
    "union",
    "unsigned",
    "void",
    "volatile",
    "while",
    "_Alignas",
    "_Alignof",
    "_Atomic",
    "_Bool",
    "_Complex",
    "_Generic",
    "_Imaginary",
    "_Noreturn",
    "_Static_assert",
    "_Thread_local",
];

const CPP_RESERVED: &[&str] = &[
    "alignas",
    "alignof",
    "and",
    "and_eq",
    "asm",
    "auto",
    "bitand",
    "bitor",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "char8_t",
    "char16_t",
    "char32_t",
    "class",
    "compl",
    "concept",
    "const",
    "consteval",
    "constexpr",
    "constinit",
    "const_cast",
    "continue",
    "co_await",
    "co_return",
    "co_yield",
    "decltype",
    "default",
    "delete",
    "do",
    "double",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "not",
    "not_eq",
    "nullptr",
    "operator",
    "or",
    "or_eq",
    "private",
    "protected",
    "public",
    "register",
    "reinterpret_cast",
    "requires",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "switch",
    "template",
    "this",
    "thread_local",
    "throw",
    "true",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "wchar_t",
    "while",
    "xor",
    "xor_eq",
];

const GO_RESERVED: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
    // predeclared identifiers a field would shadow inside the package
    "any",
    "bool",
    "byte",
    "comparable",
    "complex64",
    "complex128",
    "error",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "true",
    "false",
    "iota",
    "nil",
    "append",
    "cap",
    "clear",
    "close",
    "complex",
    "copy",
    "delete",
    "imag",
    "len",
    "make",
    "max",
    "min",
    "new",
    "panic",
    "print",
    "println",
    "real",
    "recover",
];

const RUST_RESERVED: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "gen", "macro",
    "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
];

/// The ones `r#` cannot rescue.
const RUST_NO_RAW: &[&str] = &["self", "Self", "super", "crate"];

const C3_RESERVED: &[&str] = &[
    "alias",
    "any",
    "anyfault",
    "asm",
    "assert",
    "attrdef",
    "bfloat16",
    "bitstruct",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "const",
    "continue",
    "default",
    "defer",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "false",
    "fault",
    "faultdef",
    "float",
    "float128",
    "float16",
    "fn",
    "for",
    "foreach",
    "foreach_r",
    "ichar",
    "if",
    "import",
    "inline",
    "int",
    "int128",
    "interface",
    "iptr",
    "isz",
    "long",
    "macro",
    "module",
    "nextcase",
    "null",
    "return",
    "short",
    "static",
    "struct",
    "switch",
    "tlocal",
    "true",
    "try",
    "typedef",
    "typeid",
    "uint",
    "uint128",
    "ulong",
    "uptr",
    "ushort",
    "usz",
    "var",
    "void",
    "while",
];

const LUA_RESERVED: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

pub fn reserved(lang: Lang, name: &str) -> bool {
    let list = match lang {
        Lang::C => C_RESERVED,
        Lang::Cpp => CPP_RESERVED,
        Lang::Go => GO_RESERVED,
        Lang::Rust => RUST_RESERVED,
        Lang::C3 => C3_RESERVED,
        Lang::Lua => LUA_RESERVED,
    };
    list.contains(&name)
}

/// The reason `name` cannot be used in `lang` at all — no suffix mends
/// it: C and C++ own `_X…` and `__…`, Go's `_` is the blank.
pub fn owned(lang: Lang, name: &str) -> Option<String> {
    if is_owned(lang, name) {
        Some(format!("`{name}` is reserved in {} — [names] picks another", lang.name()))
    } else {
        None
    }
}

fn is_owned(lang: Lang, name: &str) -> bool {
    match lang {
        Lang::C | Lang::Cpp => {
            name.starts_with("__")
                || (name.starts_with('_')
                    && name.chars().nth(1).is_some_and(|c| c.is_ascii_uppercase()))
        }
        Lang::Go => name == "_",
        _ => false,
    }
}

/// The spelling `name` gets in `lang`: itself, `r#name` in Rust or
/// `name_` elsewhere when it is a keyword, or the reason it cannot be.
pub fn escape(lang: Lang, name: &str) -> Result<String, String> {
    if let Some(why) = owned(lang, name) {
        return Err(why);
    }
    if !reserved(lang, name) {
        return Ok(name.to_string());
    }
    if lang == Lang::Rust && !RUST_NO_RAW.contains(&name) {
        return Ok(format!("r#{name}"));
    }
    let suffixed = format!("{name}_");
    if is_owned(lang, &suffixed) || reserved(lang, &suffixed) {
        return Err(format!("`{name}` is reserved in {} — [names] picks another", lang.name()));
    }
    Ok(suffixed)
}

/// Pascal for Go, Rust and C3, as-is elsewhere.
pub fn type_name(lang: Lang, wire: &str) -> String {
    match lang {
        Lang::Go | Lang::Rust | Lang::C3 => pascal(wire),
        Lang::C | Lang::Cpp | Lang::Lua => identifier(wire),
    }
}

/// Pascal for Go, snake for Rust and C3, as-is elsewhere.
pub fn field_name(lang: Lang, wire: &str) -> String {
    match lang {
        Lang::Go => pascal(wire),
        Lang::Rust | Lang::C3 => snake(wire),
        Lang::C | Lang::Cpp | Lang::Lua => identifier(wire),
    }
}

/// `RARITY_VERY_RARE` in C, `very_rare` in C++ and Lua, `RarityVeryRare`
/// in Go, `VeryRare` in Rust, `VERY_RARE` in C3.
pub fn enum_value_name(lang: Lang, enum_wire: &str, value: &str) -> String {
    match lang {
        Lang::C => format!("{}_{}", screaming(enum_wire), screaming(value)),
        Lang::Cpp | Lang::Lua => snake(value),
        Lang::Go => format!("{}{}", pascal(enum_wire), pascal(value)),
        Lang::Rust => pascal(value),
        Lang::C3 => screaming(value),
    }
}

/// `WEAPON_RUSTY_SWORD` in C and C3, `RUSTY_SWORD` in C++ and Rust,
/// `WeaponRustySword` in Go, `rusty_sword` in Lua.
pub fn id_name(lang: Lang, type_wire: &str, id: &str) -> String {
    match lang {
        Lang::C | Lang::C3 => format!("{}_{}", screaming(type_wire), screaming(id)),
        Lang::Cpp | Lang::Rust => screaming(id),
        Lang::Go => format!("{}{}", pascal(type_wire), pascal(id)),
        Lang::Lua => snake(id),
    }
}

/// The bare cased id an instance constant is built from: `rusty_sword`
/// in Rust, C, C++ and Lua, `RustySword` in Go, `RUSTY_SWORD` in C3. The
/// backend puts the type and the data file's stem around it where its
/// namespace is flat.
pub fn instance_name(lang: Lang, id: &str) -> String {
    match lang {
        Lang::Rust | Lang::C | Lang::Cpp | Lang::Lua => snake(id),
        Lang::Go => pascal(id),
        Lang::C3 => screaming(id),
    }
}

/// Which `(wire, code)` pairs share a code: one message per shared code,
/// naming the first two wires that got it.
pub fn collisions(pairs: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for (i, (a_wire, code)) in pairs.iter().enumerate() {
        if seen.contains(&code.as_str()) {
            continue;
        }
        if let Some((b_wire, _)) = pairs[i + 1..].iter().find(|(_, c)| c == code) {
            out.push(format!("`{a_wire}` and `{b_wire}` both become `{code}`"));
            seen.push(code);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cases() {
        assert_eq!(pascal("foo_bar"), "FooBar");
        assert_eq!(pascal("fooBar"), "FooBar");
        assert_eq!(pascal("very rare"), "VeryRare");
        assert_eq!(pascal("1st"), "_1st");
        assert_eq!(pascal(""), "_empty");
        assert_eq!(pascal("Vec2"), "Vec2");
        assert_eq!(pascal("rusty_sword"), "RustySword");
        assert_eq!(snake("FooBar"), "foo_bar");
        assert_eq!(snake("fooBar"), "foo_bar");
        assert_eq!(snake("HTTPServer"), "http_server");
        assert_eq!(snake("very rare"), "very_rare");
        assert_eq!(snake("x"), "x");
        assert_eq!(snake("Vec2"), "vec2");
        assert_eq!(screaming("fooBar"), "FOO_BAR");
        assert_eq!(screaming("Weapon"), "WEAPON");
        assert_eq!(identifier("very-rare"), "very_rare");
        assert_eq!(identifier("1st"), "_1st");
        assert_eq!(identifier(""), "_empty");
        assert_eq!(identifier("fooBar"), "fooBar");
    }

    #[test]
    fn reserved_words_get_out_of_the_way() {
        assert_eq!(escape(Lang::Rust, "type").unwrap(), "r#type");
        assert_eq!(escape(Lang::C, "default").unwrap(), "default_");
        assert_eq!(escape(Lang::Lua, "end").unwrap(), "end_");
        assert_eq!(escape(Lang::Go, "func").unwrap(), "func_");
        assert_eq!(escape(Lang::Go, "Type").unwrap(), "Type");
        assert!(escape(Lang::Go, "_").is_err());
        assert!(escape(Lang::C, "_Foo").is_err());
        assert!(escape(Lang::Cpp, "__x").is_err());
        assert_eq!(escape(Lang::Rust, "self").unwrap(), "self_");
        assert_eq!(escape(Lang::C3, "fn").unwrap(), "fn_");
        assert_eq!(escape(Lang::Rust, "name").unwrap(), "name");
    }

    #[test]
    fn per_language_idioms() {
        assert_eq!(enum_value_name(Lang::C, "Rarity", "very rare"), "RARITY_VERY_RARE");
        assert_eq!(enum_value_name(Lang::Go, "Rarity", "very rare"), "RarityVeryRare");
        assert_eq!(enum_value_name(Lang::Rust, "Rarity", "very rare"), "VeryRare");
        assert_eq!(enum_value_name(Lang::Cpp, "Rarity", "very rare"), "very_rare");
        assert_eq!(enum_value_name(Lang::C3, "Rarity", "very rare"), "VERY_RARE");
        assert_eq!(enum_value_name(Lang::Lua, "Rarity", "very rare"), "very_rare");
        assert_eq!(id_name(Lang::Go, "Weapon", "rusty_sword"), "WeaponRustySword");
        assert_eq!(id_name(Lang::C, "Weapon", "rusty_sword"), "WEAPON_RUSTY_SWORD");
        assert_eq!(id_name(Lang::Rust, "Weapon", "rusty_sword"), "RUSTY_SWORD");
        assert_eq!(id_name(Lang::Lua, "Weapon", "rusty_sword"), "rusty_sword");
        assert_eq!(instance_name(Lang::Go, "rusty_sword"), "RustySword");
        assert_eq!(instance_name(Lang::C3, "rusty_sword"), "RUSTY_SWORD");
        assert_eq!(instance_name(Lang::Rust, "RustySword"), "rusty_sword");
        assert_eq!(field_name(Lang::Go, "crit_chance"), "CritChance");
        assert_eq!(field_name(Lang::Rust, "critChance"), "crit_chance");
        assert_eq!(field_name(Lang::C, "critChance"), "critChance");
        assert_eq!(type_name(Lang::Rust, "weapon"), "Weapon");
        assert_eq!(type_name(Lang::C, "weapon"), "weapon");
    }

    #[test]
    fn collisions_name_both() {
        let c = collisions(&[
            ("foo_bar".into(), "FooBar".into()),
            ("fooBar".into(), "FooBar".into()),
            ("x".into(), "X".into()),
        ]);
        assert_eq!(c, vec!["`foo_bar` and `fooBar` both become `FooBar`"]);
        assert!(collisions(&[("a".into(), "A".into()), ("b".into(), "B".into())]).is_empty());
    }
}
