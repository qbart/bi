//! The Rust backend: an `enum` with a `NAMES` table per schema enum, a
//! `struct` with `impl Default` per schema struct, ids as `pub const`s in
//! a module named after the struct, and `bi_types.rs` for the builtins.
//! See `docs/specs/gen-struct.md`.

use serde_json::Value;

use super::super::model::{Builtin, Field, Ty, Type};
use super::super::{Backend, Context, DataUnit, Lang, Model, Unit, header, lit, names};

pub struct Rust;

const LANG: Lang = Lang::Rust;

impl Backend for Rust {
    fn unit(&self, unit: &Unit, model: &Model, cx: &Context) -> String {
        let mut out = header("//", &unit.source);
        if let Some(h) = &cx.mapping.header {
            out.push('\n');
            out.push_str(h);
            out.push('\n');
        }
        let mut imports = imports(unit, cx);
        if model.uses_support(unit) {
            imports.insert(0, "use super::bi_types::*;".to_string());
        }
        write_imports(&mut out, &imports);
        let serde = cx.mapping.derive.as_ref().is_some_and(|d| {
            d.iter().any(|x| x.ends_with("Deserialize") || x.ends_with("Serialize"))
        });
        for t in unit.types.iter().filter(|t| t.external.is_none()) {
            out.push('\n');
            if t.is_enum() {
                write_enum(&mut out, t, cx, serde);
            } else {
                write_struct(&mut out, t, model, cx, serde);
            }
        }
        out
    }

    fn data(&self, data: &DataUnit, unit: &Unit, model: &Model, cx: &Context) -> String {
        let mut out = header("//", &data.source);
        let mut imports = imports(unit, cx);
        imports.push(format!("use super::{}::*;", names::snake(&unit.stem)));
        if model.uses_support(unit) {
            imports.push("use super::bi_types::*;".to_string());
        }
        imports.sort();
        imports.dedup();
        write_imports(&mut out, &imports);
        for wire in data.types() {
            let Some(t) = model.type_named(wire) else { continue };
            let ty = Ty::Named(wire.to_string());
            out.push_str(&format!("\npub mod {} {{\n    use super::*;\n", names::snake(&t.name)));
            for inst in data.of(wire) {
                out.push_str(&format!(
                    "\n    pub fn {}() -> {} {{\n        {}\n    }}\n",
                    inst.name,
                    t.name,
                    value(&ty, &inst.value, model, cx)
                ));
            }
            let pairs: Vec<String> = data
                .of(wire)
                .map(|i| format!("({}, {}())", lit::string(&i.wire, LANG), i.name))
                .collect();
            out.push_str(&format!(
                "\n    /// Every instance of the file, in its order, with its id.\n    pub fn all() -> Vec<(&'static str, {})> {{\n        vec![{}]\n    }}\n",
                t.name,
                pairs.join(", ")
            ));
            out.push_str(&format!(
                "\n    pub fn find(id: &str) -> Option<{}> {{\n        match id {{\n",
                t.name
            ));
            for inst in data.of(wire) {
                out.push_str(&format!(
                    "            {} => Some({}()),\n",
                    lit::string(&inst.wire, LANG),
                    inst.name
                ));
            }
            out.push_str("            _ => None,\n        }\n    }\n}\n");
        }
        out
    }

    fn support(&self, model: &Model, _cx: &Context) -> Option<String> {
        if model.builtins.is_empty() {
            return None;
        }
        let mut out = header("//", "the bi format's builtins");
        for b in &model.builtins {
            out.push('\n');
            out.push_str(match b {
                Builtin::Rgb => {
                    "#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]\npub struct Rgb {\n    pub r: u8,\n    pub g: u8,\n    pub b: u8,\n}\n"
                }
                Builtin::Rgba => {
                    "#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]\npub struct Rgba {\n    pub r: u8,\n    pub g: u8,\n    pub b: u8,\n    pub a: u8,\n}\n"
                }
                Builtin::Curve => {
                    "/// A point of a tuning curve over `0..1`: position and the two tangents as slopes.\n#[derive(Debug, Clone, Copy, PartialEq, Default)]\npub struct CurvePoint {\n    pub x: f32,\n    pub y: f32,\n    pub in_: f32,\n    pub out: f32,\n}\n\n#[derive(Debug, Clone, PartialEq, Default)]\npub struct Curve {\n    pub points: Vec<CurvePoint>,\n}\n"
                }
                Builtin::Gradient => {
                    "/// A colour stop over `0..1`.\n#[derive(Debug, Clone, Copy, PartialEq, Default)]\npub struct GradientStop {\n    pub t: f32,\n    pub color: Rgba,\n}\n\n#[derive(Debug, Clone, PartialEq, Default)]\npub struct Gradient {\n    pub stops: Vec<GradientStop>,\n}\n"
                }
                Builtin::Ref => {
                    "/// A reference to an instance of `T` by id; a loader turns it into a handle.\npub struct Ref<T> {\n    pub id: String,\n    marker: std::marker::PhantomData<T>,\n}\n\nimpl<T> Ref<T> {\n    pub fn new(id: &str) -> Self {\n        Ref { id: id.to_string(), marker: std::marker::PhantomData }\n    }\n}\n\nimpl<T> Clone for Ref<T> {\n    fn clone(&self) -> Self {\n        Ref::new(&self.id)\n    }\n}\n\nimpl<T> std::fmt::Debug for Ref<T> {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        write!(f, \"Ref({:?})\", self.id)\n    }\n}\n\nimpl<T> PartialEq for Ref<T> {\n    fn eq(&self, other: &Self) -> bool {\n        self.id == other.id\n    }\n}\n\nimpl<T> Eq for Ref<T> {}\n\nimpl<T> Default for Ref<T> {\n    fn default() -> Self {\n        Ref::new(\"\")\n    }\n}\n"
                }
            });
        }
        // Gradient needs Rgba even when no field is an rgba.
        if model.builtins.contains(&Builtin::Gradient) && !model.builtins.contains(&Builtin::Rgba) {
            let rgba = "\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]\npub struct Rgba {\n    pub r: u8,\n    pub g: u8,\n    pub b: u8,\n    pub a: u8,\n}\n";
            let at = out.find("\n/// A colour stop").unwrap_or(out.len());
            out.insert_str(at, rgba);
        }
        out.into()
    }
}

/// The mapping's imports plus what the unit's external types and mapped
/// builtins ask for, sorted and deduplicated.
fn imports(unit: &Unit, cx: &Context) -> Vec<String> {
    let mut imports: Vec<String> = cx.mapping.imports.clone();
    for t in &unit.types {
        if let Some(ext) = &t.external
            && let Some(i) = &ext.import
        {
            imports.push(i.clone());
        }
    }
    for b in Builtin::ALL {
        if let Some(ext) = cx.mapping.types.get(b.key())
            && let Some(i) = &ext.import
        {
            imports.push(i.clone());
        }
    }
    imports.sort();
    imports.dedup();
    imports
}

fn write_imports(out: &mut String, imports: &[String]) {
    if !imports.is_empty() {
        out.push('\n');
        for i in imports {
            out.push_str(i);
            out.push('\n');
        }
    }
}

fn doc(out: &mut String, indent: &str, text: Option<&str>) {
    if let Some(d) = text {
        for line in d.lines() {
            out.push_str(&format!("{indent}/// {line}\n"));
        }
    }
}

fn derive_line(cx: &Context, default: &str) -> String {
    match &cx.mapping.derive {
        Some(list) => format!("#[derive({})]\n", list.join(", ")),
        None => format!("#[derive({default})]\n"),
    }
}

fn write_enum(out: &mut String, t: &Type, cx: &Context, serde: bool) {
    doc(out, "", t.doc.as_deref());
    out.push_str(&derive_line(cx, "Debug, Clone, Copy, PartialEq, Eq"));
    out.push_str(&format!("pub enum {} {{\n", t.name));
    for v in t.values() {
        if serde && v.name.trim_start_matches("r#") != v.wire {
            out.push_str(&format!("    #[serde(rename = {})]\n", lit::string(&v.wire, LANG)));
        }
        out.push_str(&format!("    {},\n", v.name));
    }
    out.push_str("}\n\n");
    out.push_str(&format!("impl {} {{\n", t.name));
    out.push_str("    /// The names the data stores, in value order.\n");
    let wires: Vec<String> = t.values().iter().map(|v| lit::string(&v.wire, LANG)).collect();
    out.push_str(&format!(
        "    pub const NAMES: [&'static str; {}] = [{}];\n}}\n\n",
        wires.len(),
        wires.join(", ")
    ));
    let first = t.values().first().map(|v| v.name.as_str()).unwrap_or("_empty");
    out.push_str(&format!(
        "impl Default for {} {{\n    fn default() -> Self {{\n        {}::{first}\n    }}\n}}\n",
        t.name, t.name
    ));
}

fn write_struct(out: &mut String, t: &Type, model: &Model, cx: &Context, serde: bool) {
    doc(out, "", t.doc.as_deref());
    out.push_str(&derive_line(cx, "Debug, Clone, PartialEq"));
    out.push_str(&format!("pub struct {} {{\n", t.name));
    for f in t.fields() {
        doc(out, "    ", f.doc.as_deref());
        doc(out, "    ", f.range.as_deref());
        if f.required_ref {
            out.push_str("    /// required\n");
        }
        if serde && f.name.trim_start_matches("r#") != f.wire {
            out.push_str(&format!("    #[serde(rename = {})]\n", lit::string(&f.wire, LANG)));
        }
        out.push_str(&format!("    pub {}: {},\n", f.name, ty(&f.ty, model, cx)));
    }
    out.push_str("}\n\n");
    out.push_str(&format!("impl Default for {} {{\n    fn default() -> Self {{\n", t.name));
    if t.fields().is_empty() {
        out.push_str(&format!("        {} {{}}\n", t.name));
    } else {
        out.push_str(&format!("        {} {{\n", t.name));
        for f in t.fields() {
            out.push_str(&format!(
                "            {}: {},\n",
                f.name,
                value(&f.ty, &f.default, model, cx)
            ));
        }
        out.push_str("        }\n");
    }
    out.push_str("    }\n}\n");
    if !t.ids().is_empty() {
        out.push_str(&format!("\npub mod {} {{\n", names::snake(&t.name)));
        for id in t.ids() {
            out.push_str(&format!(
                "    pub const {}: &str = {};\n",
                id.name,
                lit::string(&id.wire, LANG)
            ));
        }
        out.push_str("}\n");
    }
}

/// The spelling of a builtin: the mapping's, or the support file's.
fn builtin(b: Builtin, cx: &Context) -> String {
    match cx.mapping.types.get(b.key()) {
        Some(ext) => ext.spelling.clone(),
        None => match b {
            Builtin::Rgb => "Rgb",
            Builtin::Rgba => "Rgba",
            Builtin::Curve => "Curve",
            Builtin::Gradient => "Gradient",
            Builtin::Ref => "Ref",
        }
        .to_string(),
    }
}

pub fn ty(t: &Ty, model: &Model, cx: &Context) -> String {
    match t {
        Ty::Bool => "bool".into(),
        Ty::Int(k) => k.text().into(),
        Ty::F32 => "f32".into(),
        Ty::F64 => "f64".into(),
        Ty::Str => "String".into(),
        Ty::Rgb => builtin(Builtin::Rgb, cx),
        Ty::Rgba => builtin(Builtin::Rgba, cx),
        Ty::Curve => builtin(Builtin::Curve, cx),
        Ty::Gradient => builtin(Builtin::Gradient, cx),
        Ty::Named(n) => model.spelling(n).to_string(),
        Ty::List(inner) => match &cx.mapping.generics.list {
            Some(tpl) => tpl.replace("{T}", &ty(inner, model, cx)),
            None => format!("Vec<{}>", ty(inner, model, cx)),
        },
        Ty::Optional(inner, boxed) => {
            let inner = ty(inner, model, cx);
            let inner = if *boxed { format!("Box<{inner}>") } else { inner };
            match &cx.mapping.generics.optional {
                Some(tpl) => tpl.replace("{T}", &inner),
                None => format!("Option<{inner}>"),
            }
        }
        Ty::Ref(s) => match &cx.mapping.generics.reference {
            Some(tpl) => tpl.replace("{S}", model.spelling(s)),
            None => format!("Ref<{}>", model.spelling(s)),
        },
    }
}

/// A resolved default as a Rust expression of type `t`.
pub fn value(t: &Ty, v: &Value, model: &Model, cx: &Context) -> String {
    let external = |key: &str| -> Option<String> {
        let ext = cx.mapping.types.get(key)?;
        Some(ext.default.clone().unwrap_or_else(|| "Default::default()".to_string()))
    };
    match t {
        Ty::Bool => if v.as_bool().unwrap_or(false) { "true" } else { "false" }.into(),
        Ty::Int(_) => lit::int(v),
        Ty::F32 => lit::float_of(v, true, LANG),
        Ty::F64 => lit::float_of(v, false, LANG),
        Ty::Str => format!("{}.to_string()", lit::string(v.as_str().unwrap_or(""), LANG)),
        Ty::Rgb => external("rgb").unwrap_or_else(|| {
            let (r, g, b, _) = lit::colour(v.as_str().unwrap_or(""));
            format!("Rgb {{ r: {r}, g: {g}, b: {b} }}")
        }),
        Ty::Rgba => external("rgba").unwrap_or_else(|| rgba(v.as_str().unwrap_or(""))),
        Ty::Curve => external("curve").unwrap_or_else(|| {
            let points: Vec<String> = v
                .as_array()
                .map(|pts| {
                    pts.iter()
                        .map(|p| {
                            let n = |i: usize| {
                                lit::float(
                                    p.get(i).and_then(Value::as_f64).unwrap_or(0.0),
                                    true,
                                    LANG,
                                )
                            };
                            format!(
                                "CurvePoint {{ x: {}, y: {}, in_: {}, out: {} }}",
                                n(0),
                                n(1),
                                n(2),
                                n(3)
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("Curve {{ points: vec![{}] }}", points.join(", "))
        }),
        Ty::Gradient => external("gradient").unwrap_or_else(|| {
            let stops: Vec<String> = v
                .as_array()
                .map(|st| {
                    st.iter()
                        .map(|s| {
                            let t = lit::float(
                                s.get(0).and_then(Value::as_f64).unwrap_or(0.0),
                                true,
                                LANG,
                            );
                            let c = rgba(s.get(1).and_then(Value::as_str).unwrap_or(""));
                            format!("GradientStop {{ t: {t}, color: {c} }}")
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("Gradient {{ stops: vec![{}] }}", stops.join(", "))
        }),
        Ty::Named(n) => {
            if let Some(d) = external(n) {
                return d;
            }
            match model.type_named(n) {
                Some(t) if t.is_enum() => {
                    let wire = v.as_str().unwrap_or("");
                    let variant = t
                        .values()
                        .iter()
                        .find(|ev| ev.wire == wire)
                        .or(t.values().first())
                        .map(|ev| ev.name.as_str())
                        .unwrap_or("_empty");
                    format!("{}::{variant}", t.name)
                }
                Some(t) => {
                    let map = v.as_object();
                    let fields: Vec<String> = t
                        .fields()
                        .iter()
                        .map(|f: &Field| {
                            let fv = map.and_then(|m| m.get(&f.wire)).unwrap_or(&f.default);
                            format!("{}: {}", f.name, value(&f.ty, fv, model, cx))
                        })
                        .collect();
                    if fields.is_empty() {
                        format!("{} {{}}", t.name)
                    } else {
                        format!("{} {{ {} }}", t.name, fields.join(", "))
                    }
                }
                None => "Default::default()".into(),
            }
        }
        Ty::List(inner) => {
            let items: Vec<String> = v
                .as_array()
                .map(|a| a.iter().map(|x| value(inner, x, model, cx)).collect())
                .unwrap_or_default();
            if cx.mapping.generics.list.is_some() {
                format!("[{}].into_iter().collect()", items.join(", "))
            } else if items.is_empty() {
                "Vec::new()".into()
            } else {
                format!("vec![{}]", items.join(", "))
            }
        }
        Ty::Optional(inner, boxed) => {
            if v.is_null() {
                "None".into()
            } else {
                let inner = value(inner, v, model, cx);
                if *boxed { format!("Some(Box::new({inner}))") } else { format!("Some({inner})") }
            }
        }
        Ty::Ref(_) => {
            let id = lit::string(v.as_str().unwrap_or(""), LANG);
            match &cx.mapping.generics.reference {
                Some(_) => format!("{id}.into()"),
                None => format!("Ref::new({id})"),
            }
        }
    }
}

fn rgba(s: &str) -> String {
    let (r, g, b, a) = lit::colour(s);
    format!("Rgba {{ r: {r}, g: {g}, b: {b}, a: {a} }}")
}

#[cfg(test)]
mod tests {
    use super::super::super::fixture;
    use super::*;

    fn cx<'a>(m: &'a super::super::super::LangMapping, pkg: Option<&'a str>) -> Context<'a> {
        Context { pkg, mapping: m, dir_name: "out" }
    }

    const EXPECTED: &str = r#"// generated by `bi gen struct` from weapons.bischema — do not edit

use super::bi_types::*;

/// Tier
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rarity {
    Common,
    VeryRare,
}

impl Rarity {
    /// The names the data stores, in value order.
    pub const NAMES: [&'static str; 2] = ["common", "very rare"];
}

impl Default for Rarity {
    fn default() -> Self {
        Rarity::Common
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Default for Vec2 {
    fn default() -> Self {
        Vec2 {
            x: 0.0,
            y: 0.0,
        }
    }
}

/// A thing
#[derive(Debug, Clone, PartialEq)]
pub struct Weapon {
    pub name: String,
    /// 0..999
    pub damage: i32,
    pub rarity: Rarity,
    pub offset: Vec2,
    pub tags: Vec<String>,
    pub notes: Option<String>,
    pub tint: Rgb,
    /// required
    pub owner: Ref<Weapon>,
    pub r#type: bool,
}

impl Default for Weapon {
    fn default() -> Self {
        Weapon {
            name: "Sword \"x\"".to_string(),
            damage: 10,
            rarity: Rarity::Common,
            offset: Vec2 { x: 0.5, y: 0.0 },
            tags: vec!["a".to_string()],
            notes: None,
            tint: Rgb { r: 200, g: 200, b: 200 },
            owner: Ref::new(""),
            r#type: false,
        }
    }
}

pub mod weapon {
    pub const RUSTY_SWORD: &str = "rusty_sword";
    pub const DAGGER: &str = "dagger";
}
"#;

    #[test]
    fn the_fixture_as_rust() {
        let m = fixture::model(Lang::Rust, true, "");
        let lm = Default::default();
        let text = Rust.unit(&m.units[0], &m, &cx(&lm, None));
        assert_eq!(text, EXPECTED);
        let support = Rust.support(&m, &cx(&lm, None)).unwrap();
        assert!(support.contains("pub struct Rgb {"));
        assert!(support.contains("pub struct Ref<T> {"));
        assert!(!support.contains("pub struct Curve {"));
    }

    const EXPECTED_DATA: &str = r#"// generated by `bi gen struct` from level1.bidata — do not edit

use super::bi_types::*;
use super::weapons::*;

pub mod weapon {
    use super::*;

    pub fn rusty_sword() -> Weapon {
        Weapon { name: "Sword \"x\"".to_string(), damage: 10, rarity: Rarity::Common, offset: Vec2 { x: 0.5, y: 0.0 }, tags: vec!["a".to_string()], notes: None, tint: Rgb { r: 200, g: 200, b: 200 }, owner: Ref::new("rusty_sword"), r#type: false }
    }

    pub fn dagger() -> Weapon {
        Weapon { name: "Sword \"x\"".to_string(), damage: 10, rarity: Rarity::Common, offset: Vec2 { x: 0.5, y: 0.0 }, tags: vec!["a".to_string()], notes: None, tint: Rgb { r: 200, g: 200, b: 200 }, owner: Ref::new("rusty_sword"), r#type: false }
    }

    /// Every instance of the file, in its order, with its id.
    pub fn all() -> Vec<(&'static str, Weapon)> {
        vec![("rusty_sword", rusty_sword()), ("dagger", dagger())]
    }

    pub fn find(id: &str) -> Option<Weapon> {
        match id {
            "rusty_sword" => Some(rusty_sword()),
            "dagger" => Some(dagger()),
            _ => None,
        }
    }
}
"#;

    #[test]
    fn the_fixture_data_as_rust() {
        let m = fixture::model(Lang::Rust, true, "");
        let lm = Default::default();
        let u = &m.units[0];
        assert_eq!(Rust.data(&u.data[0], u, &m, &cx(&lm, None)), EXPECTED_DATA);
        // A set field overrides the default; an external type's import rides along.
        let data = fixture::DATA.replace(
            "\"$id\": \"dagger\", \"owner\": \"rusty_sword\"",
            "\"$id\": \"dagger\", \"damage\": 7, \"notes\": \"n\"",
        );
        let mapping =
            "[rust.types]\nVec2 = { as = \"glam::Vec2\", import = \"use glam::Vec2;\" }\n";
        let m = fixture::model_of(Lang::Rust, fixture::SCHEMA, &[&data], mapping);
        let lm = super::super::super::Mapping::parse(mapping).unwrap().for_lang(Lang::Rust);
        let u = &m.units[0];
        let text = Rust.data(&u.data[0], u, &m, &cx(&lm, None));
        assert!(text.contains("damage: 7,"));
        assert!(text.contains("notes: Some(\"n\".to_string()),"));
        assert!(text.contains("offset: Default::default(),"));
        assert!(
            text.contains("\nuse glam::Vec2;\nuse super::bi_types::*;\nuse super::weapons::*;\n")
        );
    }

    #[test]
    fn every_builtin_in_the_support_file() {
        let m = fixture::model_of(Lang::Rust, fixture::EVERY_BUILTIN, &[], "");
        let lm = Default::default();
        let support = Rust.support(&m, &cx(&lm, None)).unwrap();
        for s in [
            "pub struct Rgb {",
            "pub struct Rgba {",
            "pub struct Curve {",
            "pub struct CurvePoint {",
            "pub struct Gradient {",
            "pub struct GradientStop {",
            "pub struct Ref<T> {",
        ] {
            assert!(support.contains(s), "{s}");
        }
        assert_eq!(support.matches("pub struct Rgba {").count(), 1);
        let text = Rust.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("pub next: Option<Ref<Fx>>,"));
        assert!(text.contains("pub rows: Vec<Vec<f32>>,"));
        assert!(text.contains("rows: vec![vec![1.0, 2.0], Vec::new()],"));
        assert!(text.contains("big: 18446744073709551615,"));
        assert!(text.contains("glow: Rgba { r: 255, g: 0, b: 0, a: 128 },"));
        assert!(text.contains("falloff: Curve { points: vec![CurvePoint { x: 0.0, y: 0.0, in_: 1.0, out: 1.0 }, CurvePoint { x: 1.0, y: 1.0, in_: 1.0, out: 1.0 }] },"));
        assert!(text.contains("trail: Gradient { stops: vec![GradientStop { t: 0.0, color: Rgba { r: 0, g: 0, b: 0, a: 255 } }, GradientStop { t: 1.0, color: Rgba { r: 255, g: 255, b: 255, a: 255 } }] },"));
        let m = fixture::model_of(
            Lang::Rust,
            fixture::EVERY_BUILTIN,
            &[],
            "[rust.types]\ngradient = \"G\"\n",
        );
        let support = Rust.support(&m, &cx(&lm, None)).unwrap();
        assert!(!support.contains("pub struct Gradient {"));
        assert!(support.contains("pub struct Curve {"));
    }

    #[test]
    fn serde_renames_where_the_code_name_differs() {
        let mapping = "[rust]\nderive = [\"Debug\", \"serde::Deserialize\"]\n";
        let m = fixture::model(Lang::Rust, false, mapping);
        let lm = super::super::super::Mapping::parse(mapping).unwrap().for_lang(Lang::Rust);
        let text = Rust.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("#[derive(Debug, serde::Deserialize)]\npub enum Rarity"));
        assert!(text.contains("    #[serde(rename = \"very rare\")]\n    VeryRare,"));
        assert!(!text.contains("rename = \"type\""), "serde strips r# itself");
        assert!(
            text.contains("    #[serde(rename = \"common\")]\n    Common,"),
            "Common is not common"
        );
    }

    #[test]
    fn an_external_type_is_spelled_not_generated() {
        let mapping =
            "[rust.types]\nVec2 = { as = \"glam::Vec2\", import = \"use glam::Vec2;\" }\n";
        let m = fixture::model(Lang::Rust, false, mapping);
        let lm = super::super::super::Mapping::parse(mapping).unwrap().for_lang(Lang::Rust);
        let text = Rust.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(!text.contains("pub struct Vec2"));
        assert!(text.contains("    pub offset: glam::Vec2,\n"));
        assert!(text.contains("            offset: Default::default(),\n"));
        assert!(text.contains("use super::bi_types::*;\nuse glam::Vec2;\n"));
        let mapping =
            "[rust.types]\nVec2 = { as = \"glam::Vec2\", default = \"glam::Vec2::ZERO\" }\n";
        let m = fixture::model(Lang::Rust, false, mapping);
        let lm = super::super::super::Mapping::parse(mapping).unwrap().for_lang(Lang::Rust);
        let text = Rust.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("            offset: glam::Vec2::ZERO,\n"));
    }

    #[test]
    fn generics_templates_and_boxed_cycles() {
        let mapping = "[rust.generics]\nlist = \"SmallVec<[{T}; 4]>\"\nref = \"Handle<{S}>\"\n[rust]\nheader = \"#![allow(dead_code)]\"\n";
        let m = fixture::model(Lang::Rust, false, mapping);
        let lm = super::super::super::Mapping::parse(mapping).unwrap().for_lang(Lang::Rust);
        let text = Rust.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.starts_with("// generated by `bi gen struct` from weapons.bischema — do not edit\n\n#![allow(dead_code)]\n"));
        assert!(text.contains("pub tags: SmallVec<[String; 4]>,"));
        assert!(text.contains("tags: [\"a\".to_string()].into_iter().collect(),"));
        assert!(text.contains("pub owner: Handle<Weapon>,"));
        assert!(text.contains("owner: \"\".into(),"));
        assert!(Rust.support(&m, &cx(&lm, None)).unwrap().contains("Rgb"));
        assert!(!Rust.support(&m, &cx(&lm, None)).unwrap().contains("Ref<T>"));

        let m = fixture::model_of(Lang::Rust, fixture::CYCLE, &[], "");
        let lm = Default::default();
        let text = Rust.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("pub next: Option<Box<Node>>,"));
        assert!(text.contains("pub kids: Vec<Node>,"));
        assert!(text.contains("next: None,"));
        assert!(Rust.support(&m, &cx(&lm, None)).is_none());
        assert!(!text.contains("bi_types"));
    }
}
