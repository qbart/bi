//! The Go backend: a string type with typed constants per schema enum, a
//! `struct` with `json` tags and a `DefaultS()` per schema struct, ids as
//! an untyped `const` block, and `bi_types.go` for the builtins — all in
//! one package, shaped the way gofmt shapes it. See
//! `docs/specs/gen-struct.md`.

use serde_json::Value;

use super::super::model::{Builtin, Field, Ty, Type};
use super::super::{Backend, Context, Lang, Model, Unit, header, lit};

pub struct Go;

const LANG: Lang = Lang::Go;

impl Backend for Go {
    fn unit(&self, unit: &Unit, model: &Model, cx: &Context) -> String {
        let mut out = header("//", &unit.source);
        if let Some(h) = &cx.mapping.header {
            out.push('\n');
            out.push_str(h);
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&format!("package {}\n", package(cx)));
        let imports = imports(unit, model, cx);
        if !imports.is_empty() {
            out.push_str("\nimport (\n");
            for i in &imports {
                out.push_str(&format!("\t{}\n", lit::string(i, LANG)));
            }
            out.push_str(")\n");
        }
        for t in unit.types.iter().filter(|t| t.external.is_none()) {
            out.push('\n');
            if t.is_enum() {
                write_enum(&mut out, t);
            } else {
                write_struct(&mut out, t, model, cx);
            }
        }
        out
    }

    fn support(&self, model: &Model, cx: &Context) -> Option<String> {
        if model.builtins.is_empty() {
            return None;
        }
        let mut out = header("//", "the bi format's builtins");
        out.push_str(&format!("\npackage {}\n", package(cx)));
        // Gradient needs Rgba even when no field is an rgba.
        let with_rgba =
            model.builtins.contains(&Builtin::Rgba) || model.builtins.contains(&Builtin::Gradient);
        for b in Builtin::ALL {
            let wanted = if b == Builtin::Rgba { with_rgba } else { model.builtins.contains(&b) };
            if !wanted {
                continue;
            }
            out.push('\n');
            out.push_str(match b {
                Builtin::Rgb => {
                    "type Rgb struct {\n\tR uint8 `json:\"r\"`\n\tG uint8 `json:\"g\"`\n\tB uint8 `json:\"b\"`\n}\n"
                }
                Builtin::Rgba => {
                    "type Rgba struct {\n\tR uint8 `json:\"r\"`\n\tG uint8 `json:\"g\"`\n\tB uint8 `json:\"b\"`\n\tA uint8 `json:\"a\"`\n}\n"
                }
                Builtin::Curve => {
                    "// CurvePoint is a point of a tuning curve over 0..1: position and the two tangents as slopes.\ntype CurvePoint struct {\n\tX   float32 `json:\"x\"`\n\tY   float32 `json:\"y\"`\n\tIn  float32 `json:\"in\"`\n\tOut float32 `json:\"out\"`\n}\n\ntype Curve struct {\n\tPoints []CurvePoint `json:\"points\"`\n}\n"
                }
                Builtin::Gradient => {
                    "// GradientStop is a colour stop over 0..1.\ntype GradientStop struct {\n\tT     float32 `json:\"t\"`\n\tColor Rgba    `json:\"color\"`\n}\n\ntype Gradient struct {\n\tStops []GradientStop `json:\"stops\"`\n}\n"
                }
                Builtin::Ref => {
                    "// Ref is a reference to an instance of T by id; a loader turns it into a handle.\ntype Ref[T any] string\n"
                }
            });
        }
        out.into()
    }
}

/// `--pkg gen.scriptableobjects` → `scriptableobjects`; the output
/// directory's name without `--pkg`.
fn package<'a>(cx: &Context<'a>) -> &'a str {
    match cx.pkg {
        Some(p) => p.rsplit('.').next().unwrap_or(p),
        None => cx.dir_name,
    }
}

/// The import paths this unit needs: the mapping's, plus those of the
/// external types and mapped builtins its generated fields mention. Go
/// refuses an unused import, so only what the unit uses is added.
fn imports(unit: &Unit, model: &Model, cx: &Context) -> Vec<String> {
    let mut imports: Vec<String> = cx.mapping.imports.clone();
    let mut named = Vec::new();
    let mut builtins = Vec::new();
    for t in unit.types.iter().filter(|t| t.external.is_none()) {
        for f in t.fields() {
            f.ty.named(&mut named);
            f.ty.builtins(&mut builtins);
        }
    }
    for n in &named {
        if let Some(t) = model.type_named(n)
            && let Some(ext) = &t.external
            && let Some(i) = &ext.import
        {
            imports.push(i.clone());
        }
    }
    for b in builtins {
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

fn doc(out: &mut String, indent: &str, text: Option<&str>) {
    if let Some(d) = text {
        for line in d.lines() {
            out.push_str(&format!("{indent}// {line}\n"));
        }
    }
}

fn width(s: &str) -> usize {
    s.chars().count()
}

/// Lines of cells the way gofmt's tabwriter lays them out: every cell but
/// the last padded to the widest in its column, one space between.
fn aligned(out: &mut String, rows: &[Vec<String>]) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|c| rows.iter().filter_map(|r| r.get(c)).map(|s| width(s)).max().unwrap_or(0))
        .collect();
    for row in rows {
        out.push('\t');
        for (c, cell) in row.iter().enumerate() {
            if c + 1 == row.len() {
                out.push_str(cell);
            } else {
                out.push_str(cell);
                out.push_str(&" ".repeat(widths[c] - width(cell) + 1));
            }
        }
        out.push('\n');
    }
}

fn write_enum(out: &mut String, t: &Type) {
    doc(out, "", t.doc.as_deref());
    out.push_str(&format!("type {} string\n", t.name));
    if !t.values().is_empty() {
        out.push_str("\nconst (\n");
        let rows: Vec<Vec<String>> = t
            .values()
            .iter()
            .map(|v| {
                vec![v.name.clone(), t.name.clone(), format!("= {}", lit::string(&v.wire, LANG))]
            })
            .collect();
        aligned(out, &rows);
        out.push_str(")\n");
    }
    let names: Vec<&str> = t.values().iter().map(|v| v.name.as_str()).collect();
    out.push_str(&format!(
        "\n// {0}Names are the names the data stores, in value order.\nvar {0}Names = []{0}{{{1}}}\n",
        t.name,
        names.join(", ")
    ));
}

fn write_struct(out: &mut String, t: &Type, model: &Model, cx: &Context) {
    doc(out, "", t.doc.as_deref());
    if t.fields().is_empty() {
        out.push_str(&format!("type {} struct{{}}\n", t.name));
    } else {
        out.push_str(&format!("type {} struct {{\n", t.name));
        // A comment line above a field ends gofmt's alignment run.
        let mut run: Vec<Vec<String>> = Vec::new();
        for f in t.fields() {
            let mut comments = String::new();
            doc(&mut comments, "\t", f.doc.as_deref());
            doc(&mut comments, "\t", f.range.as_deref());
            if f.required_ref {
                comments.push_str("\t// required\n");
            }
            if !comments.is_empty() {
                aligned(out, &run);
                run.clear();
                out.push_str(&comments);
            }
            run.push(vec![
                f.name.clone(),
                ty(&f.ty, model, cx),
                format!("`json:{}`", lit::string(&f.wire, LANG)),
            ]);
        }
        aligned(out, &run);
        out.push_str("}\n");
    }
    out.push_str(&format!("\nfunc Default{0}() {0} {{\n", t.name));
    if t.fields().is_empty() {
        out.push_str(&format!("\treturn {}{{}}\n", t.name));
    } else {
        let mut locals = Locals::default();
        let rows: Vec<Vec<String>> = t
            .fields()
            .iter()
            .map(|f| {
                vec![
                    format!("{}:", f.name),
                    format!("{},", value(&f.ty, &f.default, model, cx, &mut locals)),
                ]
            })
            .collect();
        for l in &locals.lines {
            out.push_str(&format!("\t{l}\n"));
        }
        out.push_str(&format!("\treturn {}{{\n", t.name));
        let mut body = String::new();
        aligned(&mut body, &rows);
        for line in body.lines() {
            out.push_str(&format!("\t{line}\n"));
        }
        out.push_str("\t}\n");
    }
    out.push_str("}\n");
    if !t.ids().is_empty() {
        out.push_str("\nconst (\n");
        let rows: Vec<Vec<String>> = t
            .ids()
            .iter()
            .map(|id| vec![id.name.clone(), format!("= {}", lit::string(&id.wire, LANG))])
            .collect();
        aligned(out, &rows);
        out.push_str(")\n");
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

fn int(k: crate::props::schema::IntKind) -> &'static str {
    use crate::props::schema::IntKind::*;
    match k {
        I8 => "int8",
        I16 => "int16",
        I32 => "int32",
        I64 => "int64",
        U8 => "uint8",
        U16 => "uint16",
        U32 => "uint32",
        U64 => "uint64",
    }
}

pub fn ty(t: &Ty, model: &Model, cx: &Context) -> String {
    match t {
        Ty::Bool => "bool".into(),
        Ty::Int(k) => int(*k).into(),
        Ty::F32 => "float32".into(),
        Ty::F64 => "float64".into(),
        Ty::Str => "string".into(),
        Ty::Rgb => builtin(Builtin::Rgb, cx),
        Ty::Rgba => builtin(Builtin::Rgba, cx),
        Ty::Curve => builtin(Builtin::Curve, cx),
        Ty::Gradient => builtin(Builtin::Gradient, cx),
        Ty::Named(n) => model.spelling(n).to_string(),
        Ty::List(inner) => match &cx.mapping.generics.list {
            Some(tpl) => tpl.replace("{T}", &ty(inner, model, cx)),
            None => format!("[]{}", ty(inner, model, cx)),
        },
        // A pointer says null whether or not the edge closes a cycle.
        Ty::Optional(inner, _) => match &cx.mapping.generics.optional {
            Some(tpl) => tpl.replace("{T}", &ty(inner, model, cx)),
            None => format!("*{}", ty(inner, model, cx)),
        },
        Ty::Ref(s) => ref_ty(s, model, cx),
    }
}

fn ref_ty(s: &str, model: &Model, cx: &Context) -> String {
    match &cx.mapping.generics.reference {
        Some(tpl) => tpl.replace("{S}", model.spelling(s)),
        None => format!("Ref[{}]", model.spelling(s)),
    }
}

/// The locals a default function declares before its literal: Go cannot
/// take the address of a literal, so a set optional is `x0 := value` and
/// `&x0` in the literal.
#[derive(Default)]
pub struct Locals {
    lines: Vec<String>,
}

impl Locals {
    fn hoist(&mut self, expr: String) -> String {
        let name = format!("x{}", self.lines.len());
        self.lines.push(format!("{name} := {expr}"));
        format!("&{name}")
    }
}

/// A resolved default as a Go expression of type `t`; a set optional
/// adds a local to `locals`.
pub fn value(t: &Ty, v: &Value, model: &Model, cx: &Context, locals: &mut Locals) -> String {
    let external = |key: &str| -> Option<String> {
        let ext = cx.mapping.types.get(key)?;
        Some(ext.default.clone().unwrap_or_else(|| format!("{}{{}}", ext.spelling)))
    };
    match t {
        Ty::Bool => if v.as_bool().unwrap_or(false) { "true" } else { "false" }.into(),
        Ty::Int(_) => lit::int(v),
        Ty::F32 => lit::float_of(v, true, LANG),
        Ty::F64 => lit::float_of(v, false, LANG),
        Ty::Str => lit::string(v.as_str().unwrap_or(""), LANG),
        Ty::Rgb => external("rgb").unwrap_or_else(|| {
            let (r, g, b, _) = lit::colour(v.as_str().unwrap_or(""));
            format!("Rgb{{R: {r}, G: {g}, B: {b}}}")
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
                            format!("{{X: {}, Y: {}, In: {}, Out: {}}}", n(0), n(1), n(2), n(3))
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("Curve{{Points: []CurvePoint{{{}}}}}", points.join(", "))
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
                            format!("{{T: {t}, Color: {c}}}")
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("Gradient{{Stops: []GradientStop{{{}}}}}", stops.join(", "))
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
                        .unwrap_or_else(|| format!("{}(\"\")", t.name))
                }
                Some(t) => {
                    let map = v.as_object();
                    let fields: Vec<String> = t
                        .fields()
                        .iter()
                        .map(|f: &Field| {
                            let fv = map.and_then(|m| m.get(&f.wire)).unwrap_or(&f.default);
                            format!("{}: {}", f.name, value(&f.ty, fv, model, cx, locals))
                        })
                        .collect();
                    format!("{}{{{}}}", t.name, fields.join(", "))
                }
                None => format!("{n}{{}}"),
            }
        }
        Ty::List(_) => format!("{}{}", ty(t, model, cx), list_body(t, v, model, cx, locals)),
        Ty::Optional(inner, _) => {
            if v.is_null() {
                "nil".into()
            } else {
                let expr = value(inner, v, model, cx, locals);
                // `x := 5` would be an int: numbers get their type spelled.
                let expr = match **inner {
                    Ty::Int(_) | Ty::F32 | Ty::F64 => format!("{}({expr})", ty(inner, model, cx)),
                    _ => expr,
                };
                locals.hoist(expr)
            }
        }
        Ty::Ref(s) => {
            format!("{}({})", ref_ty(s, model, cx), lit::string(v.as_str().unwrap_or(""), LANG))
        }
    }
}

/// `{a, b}` of a list: an inner list's element type is elided, as Go
/// allows — `[][]float32{{1.0, 2.0}, {}}`.
fn list_body(t: &Ty, v: &Value, model: &Model, cx: &Context, locals: &mut Locals) -> String {
    let Ty::List(inner) = t else { return "{}".into() };
    let items: Vec<String> = v
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| match **inner {
                    Ty::List(_) => list_body(inner, x, model, cx, locals),
                    _ => value(inner, x, model, cx, locals),
                })
                .collect()
        })
        .unwrap_or_default();
    format!("{{{}}}", items.join(", "))
}

fn rgba(s: &str) -> String {
    let (r, g, b, a) = lit::colour(s);
    format!("Rgba{{R: {r}, G: {g}, B: {b}, A: {a}}}")
}

#[cfg(test)]
mod tests {
    use super::super::super::fixture;
    use super::*;

    fn cx<'a>(m: &'a super::super::super::LangMapping, pkg: Option<&'a str>) -> Context<'a> {
        Context { pkg, mapping: m, dir_name: "out" }
    }

    fn mapping(text: &str) -> super::super::super::LangMapping {
        super::super::super::Mapping::parse(text).unwrap().for_lang(Lang::Go)
    }

    const EXPECTED: &str = r#"// generated by `bi gen struct` from weapons.bischema — do not edit

package scriptableobjects

// Tier
type Rarity string

const (
	RarityCommon   Rarity = "common"
	RarityVeryRare Rarity = "very rare"
)

// RarityNames are the names the data stores, in value order.
var RarityNames = []Rarity{RarityCommon, RarityVeryRare}

type Vec2 struct {
	X float32 `json:"x"`
	Y float32 `json:"y"`
}

func DefaultVec2() Vec2 {
	return Vec2{
		X: 0.0,
		Y: 0.0,
	}
}

// A thing
type Weapon struct {
	Name string `json:"name"`
	// 0..999
	Damage int32    `json:"damage"`
	Rarity Rarity   `json:"rarity"`
	Offset Vec2     `json:"offset"`
	Tags   []string `json:"tags"`
	Notes  *string  `json:"notes"`
	Tint   Rgb      `json:"tint"`
	// required
	Owner Ref[Weapon] `json:"owner"`
	Type  bool        `json:"type"`
}

func DefaultWeapon() Weapon {
	return Weapon{
		Name:   "Sword \"x\"",
		Damage: 10,
		Rarity: RarityCommon,
		Offset: Vec2{X: 0.5, Y: 0.0},
		Tags:   []string{"a"},
		Notes:  nil,
		Tint:   Rgb{R: 200, G: 200, B: 200},
		Owner:  Ref[Weapon](""),
		Type:   false,
	}
}

const (
	WeaponRustySword = "rusty_sword"
	WeaponDagger     = "dagger"
)
"#;

    #[test]
    fn the_fixture_as_go() {
        let m = fixture::model(Lang::Go, true, "");
        let lm = Default::default();
        let text = Go.unit(&m.units[0], &m, &cx(&lm, Some("gen.scriptableobjects")));
        assert_eq!(text, EXPECTED);
        let support = Go.support(&m, &cx(&lm, Some("gen.scriptableobjects"))).unwrap();
        assert!(support.starts_with(
            "// generated by `bi gen struct` from the bi format's builtins — do not edit\n\npackage scriptableobjects\n\ntype Rgb struct {\n\tR uint8 `json:\"r\"`\n"
        ));
        assert!(support.contains("type Rgb struct"));
        assert!(support.contains("type Ref[T any] string"));
        assert!(!support.contains("type Curve"));
        assert!(!support.contains("type Rgba"));
    }

    #[test]
    fn no_pkg_is_the_directory() {
        let m = fixture::model(Lang::Go, false, "");
        let lm = Default::default();
        let text = Go.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.starts_with(
            "// generated by `bi gen struct` from weapons.bischema — do not edit\n\npackage out\n\n// Tier\n"
        ));
        assert!(!text.contains("import"));
        assert!(!text.contains("WeaponRustySword"), "no data, no ids");
        assert!(Go.support(&m, &cx(&lm, None)).unwrap().contains("\npackage out\n"));
    }

    #[test]
    fn every_builtin() {
        let m = fixture::model_of(Lang::Go, fixture::EVERY_BUILTIN, &[], "");
        let lm = Default::default();
        let support = Go.support(&m, &cx(&lm, None)).unwrap();
        for s in [
            "type Rgb struct {\n\tR uint8 `json:\"r\"`\n\tG uint8 `json:\"g\"`\n\tB uint8 `json:\"b\"`\n}\n",
            "type Rgba struct {\n\tR uint8 `json:\"r\"`\n\tG uint8 `json:\"g\"`\n\tB uint8 `json:\"b\"`\n\tA uint8 `json:\"a\"`\n}\n",
            "type CurvePoint struct {\n\tX   float32 `json:\"x\"`\n\tY   float32 `json:\"y\"`\n\tIn  float32 `json:\"in\"`\n\tOut float32 `json:\"out\"`\n}\n",
            "type Curve struct {\n\tPoints []CurvePoint `json:\"points\"`\n}\n",
            "type GradientStop struct {\n\tT     float32 `json:\"t\"`\n\tColor Rgba    `json:\"color\"`\n}\n",
            "type Gradient struct {\n\tStops []GradientStop `json:\"stops\"`\n}\n",
            "// Ref is a reference to an instance of T by id; a loader turns it into a handle.\ntype Ref[T any] string\n",
        ] {
            assert!(support.contains(s), "{s}");
        }
        assert_eq!(support.matches("type Rgba struct").count(), 1);
        let text = Go.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("\tNext    *Ref[Fx]    `json:\"next\"`\n"), "{text}");
        assert!(text.contains("\tRows    [][]float32 `json:\"rows\"`\n"), "{text}");
        assert!(text.contains("\t\tRows:    [][]float32{{1.0, 2.0}, {}},\n"), "{text}");
        assert!(text.contains("\t\tBig:     18446744073709551615,\n"), "{text}");
        assert!(text.contains("\t\tGlow:    Rgba{R: 255, G: 0, B: 0, A: 128},\n"), "{text}");
        assert!(text.contains("\t\tNext:    nil,\n"), "{text}");
        assert!(
            text.contains("\t\tFalloff: Curve{Points: []CurvePoint{{X: 0.0, Y: 0.0, In: 1.0, Out: 1.0}, {X: 1.0, Y: 1.0, In: 1.0, Out: 1.0}}},\n"),
            "{text}"
        );
        assert!(
            text.contains("\t\tTrail:   Gradient{Stops: []GradientStop{{T: 0.0, Color: Rgba{R: 0, G: 0, B: 0, A: 255}}, {T: 1.0, Color: Rgba{R: 255, G: 255, B: 255, A: 255}}}},\n"),
            "{text}"
        );
        // Gradient mapped away: Rgba stays for its own field, Curve stays.
        let lm = mapping("[go.types]\ngradient = \"G\"\n");
        let m = fixture::model_of(
            Lang::Go,
            fixture::EVERY_BUILTIN,
            &[],
            "[go.types]\ngradient = \"G\"\n",
        );
        let support = Go.support(&m, &cx(&lm, None)).unwrap();
        assert!(!support.contains("type Gradient struct"));
        assert!(support.contains("type Curve struct"));
        assert!(support.contains("type Rgba struct"));
        let text = Go.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("\tTrail   G           `json:\"trail\"`\n"), "{text}");
        assert!(text.contains("\t\tTrail:   G{},\n"), "{text}");
    }

    #[test]
    fn an_external_type_is_spelled_not_generated() {
        let text = "[go.types]\nVec2 = { as = \"mgl32.Vec2\", import = \"github.com/go-gl/mathgl/mgl32\" }\n";
        let m = fixture::model(Lang::Go, false, text);
        let lm = mapping(text);
        let text = Go.unit(&m.units[0], &m, &cx(&lm, Some("gen.scriptableobjects")));
        assert!(!text.contains("type Vec2"));
        assert!(!text.contains("DefaultVec2"));
        assert!(text.contains("\tOffset mgl32.Vec2 `json:\"offset\"`\n"), "{text}");
        assert!(text.contains("\t\tOffset: mgl32.Vec2{},\n"), "{text}");
        assert!(
            text.contains(
                "package scriptableobjects\n\nimport (\n\t\"github.com/go-gl/mathgl/mgl32\"\n)\n\n// Tier\n"
            ),
            "{text}"
        );
        let text = "[go]\nimports = [\"math\"]\n[go.types]\nVec2 = { as = \"mgl32.Vec2\", import = \"github.com/go-gl/mathgl/mgl32\", default = \"mgl32.Vec2{1, 2}\" }\n";
        let m = fixture::model(Lang::Go, false, text);
        let lm = mapping(text);
        let text = Go.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("\t\tOffset: mgl32.Vec2{1, 2},\n"), "{text}");
        assert!(
            text.contains("import (\n\t\"github.com/go-gl/mathgl/mgl32\"\n\t\"math\"\n)\n"),
            "{text}"
        );
    }

    #[test]
    fn generics_templates_and_the_header() {
        let text = "[go.generics]\nlist = \"List[{T}]\"\nref = \"Handle[{S}]\"\n[go]\nheader = \"//go:build !tinygo\"\n";
        let m = fixture::model(Lang::Go, false, text);
        let lm = mapping(text);
        let text = Go.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(
            text.starts_with(
                "// generated by `bi gen struct` from weapons.bischema — do not edit\n\n//go:build !tinygo\n\npackage out\n"
            ),
            "{text}"
        );
        assert!(text.contains("\tTags   List[string] `json:\"tags\"`\n"), "{text}");
        assert!(text.contains("\t\tTags:   List[string]{\"a\"},\n"), "{text}");
        assert!(text.contains("\tOwner Handle[Weapon] `json:\"owner\"`\n"), "{text}");
        assert!(text.contains("\t\tOwner:  Handle[Weapon](\"\"),\n"), "{text}");
        let support = Go.support(&m, &cx(&lm, None)).unwrap();
        assert!(support.contains("type Rgb struct"));
        assert!(!support.contains("Ref[T any]"));
    }

    #[test]
    fn a_set_optional_is_a_hoisted_local() {
        let schema = r#"{"$dialect":"bi/1","types":{
          "S":{"kind":"struct","fields":[
            {"name":"hp","type":"optional<i32>","default":5},
            {"name":"tag","type":"optional<string>","default":"x"},
            {"name":"none","type":"optional<f64>"},
            {"name":"many","type":"list<optional<f32>>","default":[1,null]}
          ]}}}"#;
        let m = fixture::model_of(Lang::Go, schema, &[], "");
        let lm = Default::default();
        let text = Go.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(
            text.contains(
                "func DefaultS() S {\n\tx0 := int32(5)\n\tx1 := \"x\"\n\tx2 := float32(1.0)\n\treturn S{\n\t\tHp:   &x0,\n\t\tTag:  &x1,\n\t\tNone: nil,\n\t\tMany: []*float32{&x2, nil},\n\t}\n}\n"
            ),
            "{text}"
        );
        assert!(text.contains("\tMany []*float32 `json:\"many\"`\n"), "{text}");
        assert!(Go.support(&m, &cx(&lm, None)).is_none());
    }

    #[test]
    fn the_cycle_is_a_pointer_like_any_optional() {
        let m = fixture::model_of(Lang::Go, fixture::CYCLE, &[], "");
        let lm = Default::default();
        let text = Go.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("\tNext *Node  `json:\"next\"`\n"), "{text}");
        assert!(text.contains("\tKids []Node `json:\"kids\"`\n"), "{text}");
        assert!(text.contains("\t\tNext: nil,\n"), "{text}");
        assert!(text.contains("\t\tKids: []Node{},\n"), "{text}");
        assert!(Go.support(&m, &cx(&lm, None)).is_none());
        assert!(!text.contains("import"));
    }
}
