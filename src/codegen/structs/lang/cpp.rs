//! The C++ backend: an `enum class` with a `_NAMES` table per schema
//! enum, a `struct` with default member initialisers per schema struct,
//! ids as `static constexpr` members, every struct forward-declared and
//! defined in dependency order so an embedded one is complete, and
//! `bi_types.hpp` in `namespace bi` for the builtins. C++17.
//! See `docs/specs/gen-struct.md`.

use serde_json::Value;

use super::super::model::{Builtin, Ty, Type};
use super::super::{Backend, Context, Lang, Model, Unit, header, lit, names};
use crate::props::schema::IntKind;

pub struct Cpp;

const LANG: Lang = Lang::Cpp;

impl Backend for Cpp {
    fn unit(&self, unit: &Unit, model: &Model, cx: &Context) -> String {
        let mut out = header("//", &unit.source);
        out.push_str("\n#pragma once\n\n");
        for line in includes(unit, model, cx) {
            out.push_str(&line);
            out.push('\n');
        }
        if let Some(h) = &cx.mapping.header {
            out.push('\n');
            out.push_str(h);
            out.push('\n');
        }
        let ns = cx.pkg.map(|p| p.replace('.', "::"));
        if let Some(ns) = &ns {
            out.push_str(&format!("\nnamespace {ns} {{\n"));
        }
        let generated = || unit.ordered().filter(|t| t.external.is_none());
        for t in generated().filter(|t| t.is_enum()) {
            out.push('\n');
            write_enum(&mut out, t);
        }
        let structs: Vec<&Type> = generated().filter(|t| t.is_struct()).collect();
        if !structs.is_empty() {
            out.push('\n');
            for t in &structs {
                out.push_str(&format!("struct {};\n", t.name));
            }
        }
        for t in &structs {
            out.push('\n');
            write_struct(&mut out, t, model, cx);
        }
        if let Some(ns) = &ns {
            out.push_str(&format!("\n}}  // namespace {ns}\n"));
        }
        out
    }

    fn support(&self, model: &Model, _cx: &Context) -> Option<String> {
        if model.builtins.is_empty() {
            return None;
        }
        // Gradient needs Rgba even when no field is an rgba.
        let mut builtins = model.builtins.clone();
        if builtins.contains(&Builtin::Gradient) && !builtins.contains(&Builtin::Rgba) {
            builtins.push(Builtin::Rgba);
            builtins.sort();
        }
        let vectors = builtins.iter().any(|b| matches!(b, Builtin::Curve | Builtin::Gradient));
        let mut out = header("//", "the bi format's builtins");
        out.push_str("\n#pragma once\n\n#include <cstdint>\n#include <string>\n");
        if vectors {
            out.push_str("#include <vector>\n");
        }
        out.push_str("\nnamespace bi {\n");
        for b in &builtins {
            out.push('\n');
            out.push_str(match b {
                Builtin::Rgb => {
                    "struct Rgb {\n    std::uint8_t r{};\n    std::uint8_t g{};\n    std::uint8_t b{};\n};\n"
                }
                Builtin::Rgba => {
                    "struct Rgba {\n    std::uint8_t r{};\n    std::uint8_t g{};\n    std::uint8_t b{};\n    std::uint8_t a{};\n};\n"
                }
                Builtin::Curve => {
                    "/// A point of a tuning curve over 0..1: position and the two tangents as slopes.\nstruct CurvePoint {\n    float x{};\n    float y{};\n    float in{};\n    float out{};\n};\n\nstruct Curve {\n    std::vector<CurvePoint> points;\n};\n"
                }
                Builtin::Gradient => {
                    "/// A colour stop over 0..1.\nstruct GradientStop {\n    float t{};\n    Rgba color;\n};\n\nstruct Gradient {\n    std::vector<GradientStop> stops;\n};\n"
                }
                Builtin::Ref => {
                    "/// A reference to an instance of T by id; a loader turns it into a handle.\ntemplate <class T>\nstruct Ref {\n    std::string id;\n};\n"
                }
            });
        }
        out.push_str("\n}  // namespace bi\n");
        out.into()
    }
}

/// The `#include` lines: the standard headers the unit's fields need,
/// the support file, then the mapping's, sorted and not repeating the
/// first two groups.
fn includes(unit: &Unit, model: &Model, cx: &Context) -> Vec<String> {
    let mut vector = false;
    let mut optional = false;
    let mut memory = false;
    fn needs(t: &Ty, vector: &mut bool, optional: &mut bool, memory: &mut bool) {
        match t {
            Ty::List(inner) => {
                *vector = true;
                needs(inner, vector, optional, memory);
            }
            Ty::Optional(inner, boxed) => {
                if *boxed {
                    *memory = true;
                } else {
                    *optional = true;
                }
                needs(inner, vector, optional, memory);
            }
            _ => {}
        }
    }
    for t in unit.types.iter().filter(|t| t.external.is_none()) {
        for f in t.fields() {
            needs(&f.ty, &mut vector, &mut optional, &mut memory);
        }
    }
    // A mapped generic brings its own header through `imports`.
    vector &= cx.mapping.generics.list.is_none();
    optional &= cx.mapping.generics.optional.is_none();
    let mut std_headers = vec!["cstdint", "string"];
    if memory {
        std_headers.push("memory");
    }
    if optional {
        std_headers.push("optional");
    }
    if vector {
        std_headers.push("vector");
    }
    std_headers.sort_unstable();
    let mut lines: Vec<String> = std_headers.iter().map(|h| format!("#include <{h}>")).collect();
    if model.uses_support(unit) {
        lines.push("#include \"bi_types.hpp\"".to_string());
    }
    let mut extra: Vec<String> = cx.mapping.imports.clone();
    for t in &unit.types {
        if let Some(ext) = &t.external
            && let Some(i) = &ext.import
        {
            extra.push(i.clone());
        }
    }
    for b in Builtin::ALL {
        if let Some(ext) = cx.mapping.types.get(b.key())
            && let Some(i) = &ext.import
        {
            extra.push(i.clone());
        }
    }
    extra.sort();
    extra.dedup();
    extra.retain(|i| !lines.contains(i));
    lines.extend(extra);
    lines
}

fn doc(out: &mut String, indent: &str, text: Option<&str>) {
    if let Some(d) = text {
        for line in d.lines() {
            out.push_str(&format!("{indent}/// {line}\n"));
        }
    }
}

fn write_enum(out: &mut String, t: &Type) {
    doc(out, "", t.doc.as_deref());
    out.push_str(&format!("enum class {} {{\n", t.name));
    for v in t.values() {
        out.push_str(&format!("    {},\n", v.name));
    }
    out.push_str("};\n\n");
    out.push_str("/// The names the data stores, in value order.\n");
    let wires: Vec<String> = t.values().iter().map(|v| lit::string(&v.wire, LANG)).collect();
    out.push_str(&format!(
        "inline constexpr const char *{}_NAMES[{}] = {{{}}};\n",
        names::screaming(&t.name),
        wires.len(),
        wires.join(", ")
    ));
}

fn write_struct(out: &mut String, t: &Type, model: &Model, cx: &Context) {
    doc(out, "", t.doc.as_deref());
    out.push_str(&format!("struct {} {{\n", t.name));
    for f in t.fields() {
        doc(out, "    ", f.doc.as_deref());
        doc(out, "    ", f.range.as_deref());
        if f.required_ref {
            out.push_str("    /// required\n");
        }
        out.push_str(&format!(
            "    {} {} = {};\n",
            ty(&f.ty, model, cx),
            f.name,
            value(&f.ty, &f.default, model, cx)
        ));
    }
    if !t.ids().is_empty() {
        out.push_str("\n    /// ids\n");
        for id in t.ids() {
            out.push_str(&format!(
                "    static constexpr const char *{} = {};\n",
                id.name,
                lit::string(&id.wire, LANG)
            ));
        }
    }
    out.push_str("};\n");
}

/// The spelling of a builtin: the mapping's, or the support file's.
fn builtin(b: Builtin, cx: &Context) -> String {
    match cx.mapping.types.get(b.key()) {
        Some(ext) => ext.spelling.clone(),
        None => match b {
            Builtin::Rgb => "bi::Rgb",
            Builtin::Rgba => "bi::Rgba",
            Builtin::Curve => "bi::Curve",
            Builtin::Gradient => "bi::Gradient",
            Builtin::Ref => "bi::Ref",
        }
        .to_string(),
    }
}

fn int_ty(k: IntKind) -> &'static str {
    match k {
        IntKind::I8 => "std::int8_t",
        IntKind::I16 => "std::int16_t",
        IntKind::I32 => "std::int32_t",
        IntKind::I64 => "std::int64_t",
        IntKind::U8 => "std::uint8_t",
        IntKind::U16 => "std::uint16_t",
        IntKind::U32 => "std::uint32_t",
        IntKind::U64 => "std::uint64_t",
    }
}

pub fn ty(t: &Ty, model: &Model, cx: &Context) -> String {
    match t {
        Ty::Bool => "bool".into(),
        Ty::Int(k) => int_ty(*k).into(),
        Ty::F32 => "float".into(),
        Ty::F64 => "double".into(),
        Ty::Str => "std::string".into(),
        Ty::Rgb => builtin(Builtin::Rgb, cx),
        Ty::Rgba => builtin(Builtin::Rgba, cx),
        Ty::Curve => builtin(Builtin::Curve, cx),
        Ty::Gradient => builtin(Builtin::Gradient, cx),
        Ty::Named(n) => model.spelling(n).to_string(),
        Ty::List(inner) => match &cx.mapping.generics.list {
            Some(tpl) => tpl.replace("{T}", &ty(inner, model, cx)),
            None => format!("std::vector<{}>", ty(inner, model, cx)),
        },
        // The edge that closes a cycle is a pointer whatever the mapping
        // says: an optional of an incomplete type has no size.
        Ty::Optional(inner, true) => format!("std::unique_ptr<{}>", ty(inner, model, cx)),
        Ty::Optional(inner, false) => match &cx.mapping.generics.optional {
            Some(tpl) => tpl.replace("{T}", &ty(inner, model, cx)),
            None => format!("std::optional<{}>", ty(inner, model, cx)),
        },
        Ty::Ref(s) => match &cx.mapping.generics.reference {
            Some(tpl) => tpl.replace("{S}", model.spelling(s)),
            None => format!("bi::Ref<{}>", model.spelling(s)),
        },
    }
}

/// A resolved default as a C++ expression of type `t`, one that also
/// serves as an element of a braced aggregate.
pub fn value(t: &Ty, v: &Value, model: &Model, cx: &Context) -> String {
    let external = |key: &str| -> Option<String> {
        let ext = cx.mapping.types.get(key)?;
        Some(ext.default.clone().unwrap_or_else(|| "{}".to_string()))
    };
    match t {
        Ty::Bool => if v.as_bool().unwrap_or(false) { "true" } else { "false" }.into(),
        Ty::Int(k) => {
            // The wide ones get a suffix so the literal has their type.
            let suffix = match k {
                IntKind::U64 => "ULL",
                IntKind::I64 => "LL",
                _ => "",
            };
            format!("{}{suffix}", lit::int(v))
        }
        Ty::F32 => lit::float_of(v, true, LANG),
        Ty::F64 => lit::float_of(v, false, LANG),
        Ty::Str => lit::string(v.as_str().unwrap_or(""), LANG),
        Ty::Rgb => external("rgb").unwrap_or_else(|| {
            let (r, g, b, _) = lit::colour(v.as_str().unwrap_or(""));
            format!("bi::Rgb{{{r}, {g}, {b}}}")
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
                            format!("bi::CurvePoint{{{}, {}, {}, {}}}", n(0), n(1), n(2), n(3))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if points.is_empty() {
                "bi::Curve{}".into()
            } else {
                format!("bi::Curve{{{{{}}}}}", points.join(", "))
            }
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
                            format!("bi::GradientStop{{{t}, {c}}}")
                        })
                        .collect()
                })
                .unwrap_or_default();
            if stops.is_empty() {
                "bi::Gradient{}".into()
            } else {
                format!("bi::Gradient{{{{{}}}}}", stops.join(", "))
            }
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
                    // A positional aggregate, in field order.
                    let map = v.as_object();
                    let fields: Vec<String> = t
                        .fields()
                        .iter()
                        .map(|f| {
                            let fv = map.and_then(|m| m.get(&f.wire)).unwrap_or(&f.default);
                            value(&f.ty, fv, model, cx)
                        })
                        .collect();
                    format!("{}{{{}}}", t.name, fields.join(", "))
                }
                None => "{}".into(),
            }
        }
        Ty::List(inner) => {
            let items: Vec<String> = v
                .as_array()
                .map(|a| a.iter().map(|x| value(inner, x, model, cx)).collect())
                .unwrap_or_default();
            format!("{{{}}}", items.join(", "))
        }
        Ty::Optional(inner, boxed) => {
            if v.is_null() {
                if *boxed {
                    "nullptr".into()
                } else if cx.mapping.generics.optional.is_some() {
                    "{}".into()
                } else {
                    "std::nullopt".into()
                }
            } else {
                let value = value(inner, v, model, cx);
                if *boxed {
                    format!("std::make_unique<{}>({value})", ty(inner, model, cx))
                } else {
                    value
                }
            }
        }
        Ty::Ref(_) => {
            format!("{}{{{}}}", ty(t, model, cx), lit::string(v.as_str().unwrap_or(""), LANG))
        }
    }
}

fn rgba(s: &str) -> String {
    let (r, g, b, a) = lit::colour(s);
    format!("bi::Rgba{{{r}, {g}, {b}, {a}}}")
}

#[cfg(test)]
mod tests {
    use super::super::super::fixture;
    use super::*;

    fn cx<'a>(m: &'a super::super::super::LangMapping, pkg: Option<&'a str>) -> Context<'a> {
        Context { pkg, mapping: m, dir_name: "out" }
    }

    fn mapping(text: &str) -> super::super::super::LangMapping {
        super::super::super::Mapping::parse(text).unwrap().for_lang(Lang::Cpp)
    }

    const EXPECTED: &str = r#"// generated by `bi gen struct` from weapons.bischema — do not edit

#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>
#include "bi_types.hpp"

namespace gen::scriptableobjects {

/// Tier
enum class Rarity {
    common,
    very_rare,
};

/// The names the data stores, in value order.
inline constexpr const char *RARITY_NAMES[2] = {"common", "very rare"};

struct Vec2;
struct Weapon;

struct Vec2 {
    float x = 0.0f;
    float y = 0.0f;
};

/// A thing
struct Weapon {
    std::string name = "Sword \"x\"";
    /// 0..999
    std::int32_t damage = 10;
    Rarity rarity = Rarity::common;
    Vec2 offset = Vec2{0.5f, 0.0f};
    std::vector<std::string> tags = {"a"};
    std::optional<std::string> notes = std::nullopt;
    bi::Rgb tint = bi::Rgb{200, 200, 200};
    /// required
    bi::Ref<Weapon> owner = bi::Ref<Weapon>{""};
    bool type = false;

    /// ids
    static constexpr const char *RUSTY_SWORD = "rusty_sword";
    static constexpr const char *DAGGER = "dagger";
};

}  // namespace gen::scriptableobjects
"#;

    #[test]
    fn the_fixture_as_cpp() {
        let m = fixture::model(Lang::Cpp, true, "");
        let lm = Default::default();
        let text = Cpp.unit(&m.units[0], &m, &cx(&lm, Some("gen.scriptableobjects")));
        assert_eq!(text, EXPECTED);
        let support = Cpp.support(&m, &cx(&lm, None)).unwrap();
        assert!(support.starts_with(
            "// generated by `bi gen struct` from the bi format's builtins — do not edit\n\n#pragma once\n\n#include <cstdint>\n#include <string>\n\nnamespace bi {\n"
        ));
        assert!(support.contains("struct Rgb {"));
        assert!(support.contains("template <class T>\nstruct Ref {"));
        assert!(!support.contains("struct Curve {"));
        assert!(!support.contains("<vector>"));
        assert!(support.ends_with("\n}  // namespace bi\n"));
    }

    #[test]
    fn no_pkg_means_no_namespace() {
        let m = fixture::model(Lang::Cpp, true, "");
        let lm = Default::default();
        let text = Cpp.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(!text.contains("namespace"));
        assert!(text.contains("#include \"bi_types.hpp\"\n\n/// Tier\nenum class Rarity {"));
        assert!(text.ends_with("    static constexpr const char *DAGGER = \"dagger\";\n};\n"));
    }

    #[test]
    fn every_builtin_in_the_support_file() {
        let m = fixture::model_of(Lang::Cpp, fixture::EVERY_BUILTIN, &[], "");
        let lm = Default::default();
        let support = Cpp.support(&m, &cx(&lm, None)).unwrap();
        for s in [
            "#include <vector>",
            "struct Rgb {",
            "struct Rgba {",
            "struct CurvePoint {",
            "struct Curve {",
            "struct GradientStop {",
            "struct Gradient {",
            "template <class T>\nstruct Ref {",
        ] {
            assert!(support.contains(s), "{s}");
        }
        assert_eq!(support.matches("struct Rgba {").count(), 1);
        let text = Cpp.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("#include <optional>\n"));
        assert!(text.contains("#include <vector>\n"));
        assert!(!text.contains("<memory>"));
        assert!(text.contains("    std::optional<bi::Ref<Fx>> next = std::nullopt;\n"));
        assert!(text.contains("    std::vector<std::vector<float>> rows = {{1.0f, 2.0f}, {}};\n"));
        assert!(text.contains("    std::uint64_t big = 18446744073709551615ULL;\n"));
        assert!(text.contains("    bi::Rgb tint = bi::Rgb{0, 0, 0};\n"));
        assert!(text.contains("    bi::Rgba glow = bi::Rgba{255, 0, 0, 128};\n"));
        assert!(text.contains("    bi::Curve falloff = bi::Curve{{bi::CurvePoint{0.0f, 0.0f, 1.0f, 1.0f}, bi::CurvePoint{1.0f, 1.0f, 1.0f, 1.0f}}};\n"));
        assert!(text.contains("    bi::Gradient trail = bi::Gradient{{bi::GradientStop{0.0f, bi::Rgba{0, 0, 0, 255}}, bi::GradientStop{1.0f, bi::Rgba{255, 255, 255, 255}}}};\n"));

        // Gradient mapped away: Rgba still comes with the rgba field.
        let m = fixture::model_of(
            Lang::Cpp,
            fixture::EVERY_BUILTIN,
            &[],
            "[cpp.types]\ngradient = \"G\"\n",
        );
        let support = Cpp.support(&m, &cx(&lm, None)).unwrap();
        assert!(!support.contains("struct Gradient {"));
        assert!(support.contains("struct Curve {"));
        assert!(support.contains("struct Rgba {"));

        // Rgba mapped away: Gradient still brings its own.
        let m = fixture::model_of(
            Lang::Cpp,
            fixture::EVERY_BUILTIN,
            &[],
            "[cpp.types]\nrgba = \"C\"\n",
        );
        let support = Cpp.support(&m, &cx(&lm, None)).unwrap();
        assert_eq!(support.matches("struct Rgba {").count(), 1);
        assert!(
            support.find("struct Rgba {").unwrap() < support.find("struct GradientStop {").unwrap()
        );
    }

    #[test]
    fn an_external_type_is_spelled_not_generated() {
        let text =
            "[cpp.types]\nVec2 = { as = \"glm::vec2\", import = \"#include <glm/vec2.hpp>\" }\n";
        let m = fixture::model(Lang::Cpp, false, text);
        let lm = mapping(text);
        let text = Cpp.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(!text.contains("struct Vec2"));
        assert!(text.contains("    glm::vec2 offset = {};\n"));
        assert!(text.contains("#include \"bi_types.hpp\"\n#include <glm/vec2.hpp>\n"));
        let text =
            "[cpp.types]\nVec2 = { as = \"glm::vec2\", default = \"glm::vec2(0.5f, 0.0f)\" }\n";
        let m = fixture::model(Lang::Cpp, false, text);
        let lm = mapping(text);
        let text = Cpp.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("    glm::vec2 offset = glm::vec2(0.5f, 0.0f);\n"));
    }

    #[test]
    fn generics_templates_and_boxed_cycles() {
        let text = "[cpp.generics]\nlist = \"SmallVec<{T}>\"\nref = \"Handle<{S}>\"\n[cpp]\nheader = \"// mine\"\nimports = [\"#include <small_vec.hpp>\", \"#include <string>\"]\n";
        let m = fixture::model(Lang::Cpp, false, text);
        let lm = mapping(text);
        let text = Cpp.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.starts_with(
            "// generated by `bi gen struct` from weapons.bischema — do not edit\n\n#pragma once\n\n#include <cstdint>\n#include <optional>\n#include <string>\n#include \"bi_types.hpp\"\n#include <small_vec.hpp>\n\n// mine\n\n/// Tier\n"
        ));
        assert!(!text.contains("<vector>"));
        assert_eq!(text.matches("#include <string>").count(), 1);
        assert!(text.contains("    SmallVec<std::string> tags = {\"a\"};\n"));
        assert!(text.contains("    Handle<Weapon> owner = Handle<Weapon>{\"\"};\n"));
        let support = Cpp.support(&m, &cx(&lm, None)).unwrap();
        assert!(support.contains("struct Rgb {"));
        assert!(!support.contains("struct Ref {"));

        let m = fixture::model_of(Lang::Cpp, fixture::CYCLE, &[], "");
        let lm = Default::default();
        let text = Cpp.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("#include <memory>\n"));
        assert!(!text.contains("<optional>"));
        assert!(text.contains("struct Node;\n\nstruct Node {\n"));
        assert!(text.contains("    std::unique_ptr<Node> next = nullptr;\n"));
        assert!(text.contains("    std::vector<Node> kids = {};\n"));
        assert!(Cpp.support(&m, &cx(&lm, None)).is_none());
        assert!(!text.contains("bi_types"));
    }
}
