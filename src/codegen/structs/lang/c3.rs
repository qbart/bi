//! The C3 backend: an `enum` with a `_NAMES` table per schema enum, a
//! `struct` with a `<name>_default()` function per schema struct, ids as
//! `const String`s, a named `Opt*` struct per optional instantiation, a
//! `typedef <S>Ref = String;` per referenced struct, and `bi_types.c3`
//! for the builtins. C3 0.7 syntax. See `docs/specs/gen-struct.md`.

use serde_json::Value;

use super::super::model::{Builtin, Field, Ty, Type};
use super::super::{Backend, Context, Lang, Model, Unit, header, lit, names};
use crate::props::schema::IntKind;

pub struct C3;

const LANG: Lang = Lang::C3;

impl Backend for C3 {
    fn unit(&self, unit: &Unit, model: &Model, cx: &Context) -> String {
        let mut out = header("//", &unit.source);
        match cx.pkg {
            Some(pkg) => out.push_str(&format!("module {};\n", pkg.replace('.', "::"))),
            None => out.push_str(&format!("module {};\n", unit.stem)),
        }
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
        if model.uses_support(unit) {
            imports.insert(0, "import bi_types;".to_string());
        }
        if !imports.is_empty() {
            out.push('\n');
            for i in &imports {
                out.push_str(i);
                out.push('\n');
            }
        }
        if let Some(h) = &cx.mapping.header {
            out.push('\n');
            out.push_str(h);
            out.push('\n');
        }
        let generated: Vec<&Type> = unit.ordered().filter(|t| t.external.is_none()).collect();
        for t in generated.iter().filter(|t| t.is_enum()) {
            out.push('\n');
            write_enum(&mut out, t);
        }
        // The optional instantiations and the referenced structs, each once,
        // in order of first use; C3 does not need them before the structs
        // that use them, but a reader does.
        let mut opts: Vec<(String, String)> = Vec::new();
        let mut refs: Vec<String> = Vec::new();
        for t in &generated {
            for f in t.fields() {
                collect(&f.ty, model, cx, &mut opts, &mut refs);
            }
        }
        if !opts.is_empty() {
            out.push('\n');
            for (name, inner) in &opts {
                out.push_str(&format!("struct {name} {{ bool set; {inner} value; }}\n"));
            }
        }
        if !refs.is_empty() {
            out.push('\n');
            for s in &refs {
                out.push_str(&format!("typedef {} = String;\n", ref_name(model, s)));
            }
        }
        for t in generated.iter().filter(|t| t.is_struct()) {
            out.push('\n');
            write_struct(&mut out, t, model, cx);
        }
        out
    }

    fn support(&self, model: &Model, _cx: &Context) -> Option<String> {
        // Ref has no support type in C3: the typedefs are per unit.
        let types: Vec<Builtin> =
            model.builtins.iter().copied().filter(|b| *b != Builtin::Ref).collect();
        if types.is_empty() {
            return None;
        }
        let mut out = header("//", "the bi format's builtins");
        out.push_str("module bi_types;\n");
        let rgba = "struct Rgba {\n    char r;\n    char g;\n    char b;\n    char a;\n}\n";
        for b in &types {
            out.push('\n');
            out.push_str(match b {
                Builtin::Rgb => "struct Rgb {\n    char r;\n    char g;\n    char b;\n}\n",
                Builtin::Rgba => rgba,
                Builtin::Curve => {
                    "<* A point of a tuning curve over 0..1: position and the two tangents as slopes. *>\nstruct CurvePoint {\n    float x;\n    float y;\n    float in;\n    float out;\n}\n\nstruct Curve {\n    CurvePoint[] points;\n}\n"
                }
                Builtin::Gradient => {
                    "<* A colour stop over 0..1. *>\nstruct GradientStop {\n    float t;\n    Rgba color;\n}\n\nstruct Gradient {\n    GradientStop[] stops;\n}\n"
                }
                Builtin::Ref => unreachable!(),
            });
        }
        // Gradient needs Rgba even when no field is an rgba.
        if types.contains(&Builtin::Gradient) && !types.contains(&Builtin::Rgba) {
            let at = out.find("\n<* A colour stop").unwrap_or(out.len());
            out.insert_str(at, &format!("\n{rgba}"));
        }
        out.into()
    }
}

/// One `<* … *>` block from every line of documentation an item has:
/// `<* Tier *>` for one line, a multi-line block for more. One block,
/// because the compiler takes one doc comment per declaration.
fn doc(out: &mut String, indent: &str, lines: &[&str]) {
    match lines {
        [] => {}
        [one] => out.push_str(&format!("{indent}<* {one} *>\n")),
        many => {
            out.push_str(&format!("{indent}<*\n"));
            for l in many {
                out.push_str(&format!("{indent} {l}\n"));
            }
            out.push_str(&format!("{indent}*>\n"));
        }
    }
}

fn write_enum(out: &mut String, t: &Type) {
    let lines: Vec<&str> = t.doc.as_deref().map(|d| d.lines().collect()).unwrap_or_default();
    doc(out, "", &lines);
    out.push_str(&format!("enum {} : int {{\n", t.name));
    for v in t.values() {
        out.push_str(&format!("    {},\n", v.name));
    }
    out.push_str("}\n\n");
    let wires: Vec<String> = t.values().iter().map(|v| lit::string(&v.wire, LANG)).collect();
    let table =
        if wires.is_empty() { "{}".to_string() } else { format!("{{ {} }}", wires.join(", ")) };
    out.push_str(&format!(
        "const String[{}] {}_NAMES = {table};\n",
        wires.len(),
        names::screaming(&t.name)
    ));
}

fn write_struct(out: &mut String, t: &Type, model: &Model, cx: &Context) {
    let lines: Vec<&str> = t.doc.as_deref().map(|d| d.lines().collect()).unwrap_or_default();
    doc(out, "", &lines);
    out.push_str(&format!("struct {} {{\n", t.name));
    if t.fields().is_empty() {
        out.push_str("    char bi_empty_; // C3 refuses an empty struct\n");
    }
    for f in t.fields() {
        let mut lines: Vec<&str> =
            f.doc.as_deref().map(|d| d.lines().collect()).unwrap_or_default();
        if let Some(r) = &f.range {
            lines.push(r);
        }
        if f.required_ref {
            lines.push("required");
        }
        doc(out, "    ", &lines);
        out.push_str(&format!("    {} {};\n", ty(&f.ty, model, cx), f.name));
    }
    out.push_str("}\n\n");
    out.push_str(&format!("fn {} {}_default()\n{{\n", t.name, names::snake(&t.name)));
    if t.fields().is_empty() {
        out.push_str("    return {};\n");
    } else {
        out.push_str("    return {\n");
        for f in t.fields() {
            out.push_str(&format!(
                "        .{} = {},\n",
                f.name,
                value(&f.ty, &f.default, model, cx)
            ));
        }
        out.push_str("    };\n");
    }
    out.push_str("}\n");
    if !t.ids().is_empty() {
        out.push('\n');
        for id in t.ids() {
            out.push_str(&format!("const String {} = {};\n", id.name, lit::string(&id.wire, LANG)));
        }
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

fn int(k: IntKind) -> &'static str {
    match k {
        IntKind::I8 => "ichar",
        IntKind::I16 => "short",
        IntKind::I32 => "int",
        IntKind::I64 => "long",
        IntKind::U8 => "char",
        IntKind::U16 => "ushort",
        IntKind::U32 => "uint",
        IntKind::U64 => "ulong",
    }
}

/// `WeaponRef` — the typedef for a reference to struct `s`.
fn ref_name(model: &Model, s: &str) -> String {
    format!("{}Ref", model.spelling(s))
}

/// The part of an `Opt…` name a type contributes: `String`, `Vec2`,
/// `FxRef`, `ListFloat`, `OptString`; a spelling's punctuation dropped.
fn mangle(t: &Ty, model: &Model, cx: &Context) -> String {
    match t {
        Ty::Bool => "Bool".into(),
        Ty::Int(k) => names::pascal(int(*k)),
        Ty::F32 => "Float".into(),
        Ty::F64 => "Double".into(),
        Ty::Str => "String".into(),
        Ty::Rgb | Ty::Rgba | Ty::Curve | Ty::Gradient | Ty::Named(_) => {
            ty(t, model, cx).chars().filter(|c| c.is_alphanumeric() || *c == '_').collect()
        }
        Ty::List(inner) => format!("List{}", mangle(inner, model, cx)),
        Ty::Optional(inner, _) => format!("Opt{}", mangle(inner, model, cx)),
        Ty::Ref(s) => ref_name(model, s),
    }
}

/// Every optional instantiation (name, inner type) and every referenced
/// struct under `t`, inner ones first, each once.
fn collect(
    t: &Ty,
    model: &Model,
    cx: &Context,
    opts: &mut Vec<(String, String)>,
    refs: &mut Vec<String>,
) {
    match t {
        Ty::List(inner) => collect(inner, model, cx, opts, refs),
        Ty::Optional(inner, boxed) => {
            collect(inner, model, cx, opts, refs);
            if !*boxed && cx.mapping.generics.optional.is_none() {
                let name = mangle(t, model, cx);
                if !opts.iter().any(|(n, _)| *n == name) {
                    opts.push((name, ty(inner, model, cx)));
                }
            }
        }
        Ty::Ref(s) => {
            if cx.mapping.generics.reference.is_none() && !refs.contains(s) {
                refs.push(s.clone());
            }
        }
        _ => {}
    }
}

pub fn ty(t: &Ty, model: &Model, cx: &Context) -> String {
    match t {
        Ty::Bool => "bool".into(),
        Ty::Int(k) => int(*k).into(),
        Ty::F32 => "float".into(),
        Ty::F64 => "double".into(),
        Ty::Str => "String".into(),
        Ty::Rgb => builtin(Builtin::Rgb, cx),
        Ty::Rgba => builtin(Builtin::Rgba, cx),
        Ty::Curve => builtin(Builtin::Curve, cx),
        Ty::Gradient => builtin(Builtin::Gradient, cx),
        Ty::Named(n) => model.spelling(n).to_string(),
        Ty::List(inner) => match &cx.mapping.generics.list {
            Some(tpl) => tpl.replace("{T}", &ty(inner, model, cx)),
            None => format!("{}[]", ty(inner, model, cx)),
        },
        Ty::Optional(inner, boxed) => {
            if *boxed {
                return format!("{}*", ty(inner, model, cx));
            }
            match &cx.mapping.generics.optional {
                Some(tpl) => tpl.replace("{T}", &ty(inner, model, cx)),
                None => mangle(t, model, cx),
            }
        }
        Ty::Ref(s) => match &cx.mapping.generics.reference {
            Some(tpl) => tpl.replace("{S}", model.spelling(s)),
            None => ref_name(model, s),
        },
    }
}

/// A resolved default as a C3 initializer of type `t`.
pub fn value(t: &Ty, v: &Value, model: &Model, cx: &Context) -> String {
    let external = |key: &str| -> Option<String> {
        let ext = cx.mapping.types.get(key)?;
        Some(ext.default.clone().unwrap_or_else(|| "{}".to_string()))
    };
    match t {
        Ty::Bool => if v.as_bool().unwrap_or(false) { "true" } else { "false" }.into(),
        Ty::Int(_) => lit::int(v),
        Ty::F32 => lit::float_of(v, true, LANG),
        Ty::F64 => lit::float_of(v, false, LANG),
        Ty::Str => lit::string(v.as_str().unwrap_or(""), LANG),
        Ty::Rgb => external("rgb").unwrap_or_else(|| {
            let (r, g, b, _) = lit::colour(v.as_str().unwrap_or(""));
            format!("{{ .r = {r}, .g = {g}, .b = {b} }}")
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
                                "{{ .x = {}, .y = {}, .in = {}, .out = {} }}",
                                n(0),
                                n(1),
                                n(2),
                                n(3)
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("{{ .points = {} }}", list(&points))
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
                            format!("{{ .t = {t}, .color = {c} }}")
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("{{ .stops = {} }}", list(&stops))
        }),
        Ty::Named(n) => {
            if let Some(d) = external(n) {
                return d;
            }
            match model.type_named(n) {
                Some(t) if t.is_enum() => {
                    let wire = v.as_str().unwrap_or("");
                    t.values()
                        .iter()
                        .find(|ev| ev.wire == wire)
                        .or(t.values().first())
                        .map(|ev| ev.name.clone())
                        .unwrap_or_else(|| "_empty".into())
                }
                Some(t) => {
                    let map = v.as_object();
                    let fields: Vec<String> = t
                        .fields()
                        .iter()
                        .map(|f: &Field| {
                            let fv = map.and_then(|m| m.get(&f.wire)).unwrap_or(&f.default);
                            format!(".{} = {}", f.name, value(&f.ty, fv, model, cx))
                        })
                        .collect();
                    list(&fields)
                }
                None => "{}".into(),
            }
        }
        Ty::List(inner) => {
            let items: Vec<String> = v
                .as_array()
                .map(|a| a.iter().map(|x| value(inner, x, model, cx)).collect())
                .unwrap_or_default();
            list(&items)
        }
        Ty::Optional(inner, boxed) => {
            if *boxed {
                return "null".into();
            }
            if v.is_null() {
                "{ .set = false }".into()
            } else {
                format!("{{ .set = true, .value = {} }}", value(inner, v, model, cx))
            }
        }
        Ty::Ref(s) => {
            let id = lit::string(v.as_str().unwrap_or(""), LANG);
            match &cx.mapping.generics.reference {
                Some(_) => id,
                None => format!("({}){id}", ref_name(model, s)),
            }
        }
    }
}

/// `{ a, b }`, or `{}` for nothing.
fn list(items: &[String]) -> String {
    if items.is_empty() { "{}".into() } else { format!("{{ {} }}", items.join(", ")) }
}

fn rgba(s: &str) -> String {
    let (r, g, b, a) = lit::colour(s);
    format!("{{ .r = {r}, .g = {g}, .b = {b}, .a = {a} }}")
}

#[cfg(test)]
mod tests {
    use super::super::super::fixture;
    use super::*;

    fn cx<'a>(m: &'a super::super::super::LangMapping, pkg: Option<&'a str>) -> Context<'a> {
        Context { pkg, mapping: m, dir_name: "out" }
    }

    const EXPECTED: &str = r#"// generated by `bi gen struct` from weapons.bischema — do not edit
module gen::scriptableobjects;

import bi_types;

<* Tier *>
enum Rarity : int {
    COMMON,
    VERY_RARE,
}

const String[2] RARITY_NAMES = { "common", "very rare" };

struct OptString { bool set; String value; }

typedef WeaponRef = String;

struct Vec2 {
    float x;
    float y;
}

fn Vec2 vec2_default()
{
    return {
        .x = 0.0,
        .y = 0.0,
    };
}

<* A thing *>
struct Weapon {
    String name;
    <* 0..999 *>
    int damage;
    Rarity rarity;
    Vec2 offset;
    String[] tags;
    OptString notes;
    Rgb tint;
    <* required *>
    WeaponRef owner;
    bool type;
}

fn Weapon weapon_default()
{
    return {
        .name = "Sword \"x\"",
        .damage = 10,
        .rarity = COMMON,
        .offset = { .x = 0.5, .y = 0.0 },
        .tags = { "a" },
        .notes = { .set = false },
        .tint = { .r = 200, .g = 200, .b = 200 },
        .owner = (WeaponRef)"",
        .type = false,
    };
}

const String WEAPON_RUSTY_SWORD = "rusty_sword";
const String WEAPON_DAGGER = "dagger";
"#;

    #[test]
    fn the_fixture_as_c3() {
        let m = fixture::model(Lang::C3, true, "");
        let lm = Default::default();
        let text = C3.unit(&m.units[0], &m, &cx(&lm, Some("gen.scriptableobjects")));
        assert_eq!(text, EXPECTED);
        let support = C3.support(&m, &cx(&lm, None)).unwrap();
        assert!(support.starts_with(
            "// generated by `bi gen struct` from the bi format's builtins — do not edit\nmodule bi_types;\n\nstruct Rgb {\n"
        ));
        assert!(support.contains("struct Rgb {\n    char r;\n    char g;\n    char b;\n}\n"));
        assert!(!support.contains("struct Curve"));
        assert!(!support.contains("Ref"));
    }

    #[test]
    fn without_a_pkg_the_module_is_the_stem() {
        let m = fixture::model(Lang::C3, false, "");
        let lm = Default::default();
        let text = C3.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.starts_with(
            "// generated by `bi gen struct` from weapons.bischema — do not edit\nmodule weapons;\n\nimport bi_types;\n"
        ));
        assert!(!text.contains("WEAPON_RUSTY_SWORD"));
    }

    #[test]
    fn every_builtin() {
        let m = fixture::model_of(Lang::C3, fixture::EVERY_BUILTIN, &[], "");
        let lm = Default::default();
        let support = C3.support(&m, &cx(&lm, None)).unwrap();
        for s in [
            "struct Rgb {\n",
            "struct Rgba {\n    char r;\n    char g;\n    char b;\n    char a;\n}\n",
            "struct CurvePoint {\n    float x;\n    float y;\n    float in;\n    float out;\n}\n",
            "struct Curve {\n    CurvePoint[] points;\n}\n",
            "struct GradientStop {\n    float t;\n    Rgba color;\n}\n",
            "struct Gradient {\n    GradientStop[] stops;\n}\n",
        ] {
            assert!(support.contains(s), "{s}");
        }
        assert_eq!(support.matches("struct Rgba {").count(), 1);
        assert!(!support.contains("Ref"));
        let text = C3.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("\nstruct OptFxRef { bool set; FxRef value; }\n"));
        assert!(text.contains("\ntypedef FxRef = String;\n"));
        assert!(text.contains("    OptFxRef next;\n"));
        assert!(text.contains("    float[][] rows;\n"));
        assert!(text.contains("    ulong big;\n"));
        assert!(text.contains("        .next = { .set = false },\n"));
        assert!(text.contains("        .rows = { { 1.0, 2.0 }, {} },\n"));
        assert!(text.contains("        .big = 18446744073709551615,\n"));
        assert!(text.contains("        .glow = { .r = 255, .g = 0, .b = 0, .a = 128 },\n"));
        assert!(text.contains("        .falloff = { .points = { { .x = 0.0, .y = 0.0, .in = 1.0, .out = 1.0 }, { .x = 1.0, .y = 1.0, .in = 1.0, .out = 1.0 } } },\n"));
        assert!(text.contains("        .trail = { .stops = { { .t = 0.0, .color = { .r = 0, .g = 0, .b = 0, .a = 255 } }, { .t = 1.0, .color = { .r = 255, .g = 255, .b = 255, .a = 255 } } } },\n"));
        // Gradient brings Rgba along when no field is an rgba.
        let m = fixture::model_of(
            Lang::C3,
            fixture::EVERY_BUILTIN,
            &[],
            "[c3.types]\nrgba = \"MyRgba\"\n",
        );
        let support = C3.support(&m, &cx(&lm, None)).unwrap();
        assert_eq!(support.matches("struct Rgba {").count(), 1);
        assert!(
            support.find("struct Rgba {").unwrap() < support.find("struct GradientStop {").unwrap()
        );
    }

    #[test]
    fn a_cycle_is_a_pointer_and_needs_no_support() {
        let m = fixture::model_of(Lang::C3, fixture::CYCLE, &[], "");
        let lm = Default::default();
        let text = C3.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("    Node* next;\n"));
        assert!(text.contains("    Node[] kids;\n"));
        assert!(text.contains("        .next = null,\n"));
        assert!(text.contains("        .kids = {},\n"));
        assert!(!text.contains("struct Opt"));
        assert!(!text.contains("bi_types"));
        assert!(C3.support(&m, &cx(&lm, None)).is_none());
    }

    #[test]
    fn an_empty_struct_gets_a_pad_member() {
        let schema = r##"{ "$dialect": "bi/1", "types": {
          "Nothing": { "kind": "struct", "fields": [] }
        }}"##;
        let m = fixture::model_of(Lang::C3, schema, &[], "");
        let lm = Default::default();
        let text = C3.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(
            text.contains(
                "struct Nothing {\n    char bi_empty_; // C3 refuses an empty struct\n}\n"
            )
        );
        assert!(text.contains("fn Nothing nothing_default()\n{\n    return {};\n}\n"));
        assert!(C3.support(&m, &cx(&lm, None)).is_none());
    }

    #[test]
    fn an_external_type_is_spelled_not_generated() {
        let mapping = "[c3.types]\nVec2 = { as = \"math::Vec2\", import = \"import math;\" }\n";
        let m = fixture::model(Lang::C3, false, mapping);
        let lm = super::super::super::Mapping::parse(mapping).unwrap().for_lang(Lang::C3);
        let text = C3.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(!text.contains("struct Vec2"));
        assert!(text.contains("    math::Vec2 offset;\n"));
        assert!(text.contains("        .offset = {},\n"));
        assert!(text.contains("module weapons;\n\nimport bi_types;\nimport math;\n"));
        let mapping = "[c3.types]\nVec2 = { as = \"math::Vec2\", default = \"math::VEC2_ZERO\" }\n";
        let m = fixture::model(Lang::C3, false, mapping);
        let lm = super::super::super::Mapping::parse(mapping).unwrap().for_lang(Lang::C3);
        let text = C3.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("        .offset = math::VEC2_ZERO,\n"));
    }

    #[test]
    fn generics_templates_replace_the_generated_helpers() {
        let mapping = "[c3.generics]\nlist = \"List{{T}}\"\noptional = \"Maybe{{T}}\"\nref = \"Handle{{S}}\"\n[c3]\nheader = \"// hello\"\n";
        let m = fixture::model(Lang::C3, false, mapping);
        let lm = super::super::super::Mapping::parse(mapping).unwrap().for_lang(Lang::C3);
        let text = C3.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.starts_with(
            "// generated by `bi gen struct` from weapons.bischema — do not edit\nmodule weapons;\n\nimport bi_types;\n\n// hello\n"
        ));
        assert!(text.contains("    List{String} tags;\n"));
        assert!(text.contains("    Maybe{String} notes;\n"));
        assert!(text.contains("    Handle{Weapon} owner;\n"));
        assert!(text.contains("        .owner = \"\",\n"));
        assert!(!text.contains("struct Opt"));
        assert!(!text.contains("typedef"));
        assert!(C3.support(&m, &cx(&lm, None)).unwrap().contains("struct Rgb {"));
    }
}
