//! The C backend: a C11 header per schema — an `enum` with a `_NAMES`
//! table per schema enum, a `struct` with a `_init` function per schema
//! struct, one `bi_list_<m>` / `bi_opt_<m>` typedef per list and optional
//! instantiation, ids as `#define`s — and `bi_types.h` for the builtins.
//! See `docs/specs/gen-struct.md`.

use serde_json::Value;

use super::super::model::{Builtin, Ty, Type};
use super::super::{Backend, Context, Lang, Model, Unit, header, lit, names};
use crate::props::schema::IntKind;

pub struct C;

const LANG: Lang = Lang::C;

impl Backend for C {
    fn unit(&self, unit: &Unit, model: &Model, cx: &Context) -> String {
        let mut out = header("//", &unit.source);
        let guard = format!("BI_GEN_{}_H", names::screaming(&unit.stem));
        out.push_str(&format!("#ifndef {guard}\n#define {guard}\n"));
        if let Some(h) = &cx.mapping.header {
            out.push('\n');
            out.push_str(h);
            out.push('\n');
        }
        out.push_str("\n#include <stdbool.h>\n#include <stddef.h>\n#include <stdint.h>\n");
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
            imports.insert(0, "#include \"bi_types.h\"".to_string());
        }
        if !imports.is_empty() {
            out.push('\n');
            for i in &imports {
                out.push_str(i);
                out.push('\n');
            }
        }
        let generated: Vec<&Type> = unit.ordered().filter(|t| t.external.is_none()).collect();
        for t in generated.iter().filter(|t| t.is_enum()) {
            out.push('\n');
            write_enum(&mut out, t, cx);
        }
        let structs: Vec<&Type> = generated.iter().copied().filter(|t| t.is_struct()).collect();
        if !structs.is_empty() {
            out.push('\n');
            for t in &structs {
                out.push_str(&format!("struct {};\n", pre(cx, &t.name, false)));
            }
        }
        let mut typedefs = Typedefs::default();
        for t in &structs {
            for f in t.fields() {
                typedefs.want(&f.ty, model, cx);
            }
            typedefs.flush(&mut out);
            out.push('\n');
            write_struct(&mut out, t, model, cx);
            typedefs.complete.push(t.wire.clone());
            typedefs.flush(&mut out);
        }
        out.push_str(&format!("\n#endif /* {guard} */\n"));
        out
    }

    fn support(&self, model: &Model, _cx: &Context) -> Option<String> {
        if model.builtins.is_empty() {
            return None;
        }
        let mut out = header("//", "the bi format's builtins");
        out.push_str("#ifndef BI_TYPES_H\n#define BI_TYPES_H\n\n#include <stddef.h>\n#include <stdint.h>\n\n");
        // Gradient needs bi_rgba even when no field is an rgba.
        let rgba_too = model.builtins.contains(&Builtin::Gradient);
        for b in Builtin::ALL {
            if !model.builtins.contains(&b) && !(b == Builtin::Rgba && rgba_too) {
                continue;
            }
            out.push_str(match b {
                Builtin::Rgb => "typedef struct { uint8_t r, g, b; } bi_rgb;\n",
                Builtin::Rgba => "typedef struct { uint8_t r, g, b, a; } bi_rgba;\n",
                Builtin::Curve => {
                    "/* A point of a tuning curve over 0..1: position and the two tangents as slopes. */\ntypedef struct { float x, y, in_, out; } bi_curve_point;\ntypedef struct { bi_curve_point *points; size_t len; } bi_curve;\n"
                }
                Builtin::Gradient => {
                    "/* A colour stop over 0..1. */\ntypedef struct { float t; bi_rgba color; } bi_gradient_stop;\ntypedef struct { bi_gradient_stop *stops; size_t len; } bi_gradient;\n"
                }
                Builtin::Ref => {
                    "/* A reference to an instance by id; a loader turns it into a handle. */\ntypedef const char *bi_ref;\n"
                }
            });
        }
        out.push_str("\n#endif /* BI_TYPES_H */\n");
        out.into()
    }
}

/// `name` behind the mapping's prefix: `gs_Weapon`, or `GS_RARITY_COMMON`
/// when `upper`.
fn pre(cx: &Context, name: &str, upper: bool) -> String {
    let p = cx.mapping.prefix.as_deref().unwrap_or("");
    if upper { format!("{}{name}", p.to_uppercase()) } else { format!("{p}{name}") }
}

/// A doc comment: one line as `/* … */`, more as a block.
fn doc(out: &mut String, indent: &str, text: Option<&str>) {
    let Some(d) = text else { return };
    let d = d.replace("*/", "* /");
    let lines: Vec<&str> = d.lines().collect();
    match lines.as_slice() {
        [] => {}
        [one] => out.push_str(&format!("{indent}/* {one} */\n")),
        many => {
            out.push_str(&format!("{indent}/*\n"));
            for l in many {
                out.push_str(&format!("{indent} * {l}\n"));
            }
            out.push_str(&format!("{indent} */\n"));
        }
    }
}

/// `T name` — or `T *name` with the star against the name.
fn decl(ty: &str, name: &str) -> String {
    if ty.ends_with('*') { format!("{ty}{name}") } else { format!("{ty} {name}") }
}

/// A pointer to `ty`.
fn ptr(ty: &str) -> String {
    if ty.ends_with('*') { format!("{ty}*") } else { format!("{ty} *") }
}

fn write_enum(out: &mut String, t: &Type, cx: &Context) {
    doc(out, "", t.doc.as_deref());
    out.push_str(&format!("enum {} {{\n", pre(cx, &t.name, false)));
    for v in t.values() {
        out.push_str(&format!("    {},\n", pre(cx, &v.name, true)));
    }
    out.push_str("};\n\n");
    out.push_str("/* The names the data stores, in value order. */\n");
    let wires: Vec<String> = t.values().iter().map(|v| lit::string(&v.wire, LANG)).collect();
    out.push_str(&format!(
        "static const char *const {}_NAMES[{}] = {{{}}};\n",
        pre(cx, &names::screaming(&t.name), true),
        wires.len(),
        wires.join(", ")
    ));
}

fn write_struct(out: &mut String, t: &Type, model: &Model, cx: &Context) {
    let name = pre(cx, &t.name, false);
    doc(out, "", t.doc.as_deref());
    out.push_str(&format!("struct {name} {{\n"));
    if t.fields().is_empty() {
        out.push_str("    char bi_empty_; /* C refuses an empty struct */\n");
    }
    for f in t.fields() {
        doc(out, "    ", f.doc.as_deref());
        doc(out, "    ", f.range.as_deref());
        let mut notes = Vec::new();
        if let Ty::Ref(s) = &f.ty {
            notes.push(format!("ref<{}>", spelling(s, model, cx)));
        }
        if f.required_ref {
            notes.push("required".to_string());
        }
        if f.name != f.wire {
            notes.push(format!("{} in the data", lit::string(&f.wire, LANG)));
        }
        let trailer =
            if notes.is_empty() { String::new() } else { format!(" /* {} */", notes.join(", ")) };
        out.push_str(&format!("    {};{trailer}\n", decl(&ty(&f.ty, model, cx), &f.name)));
    }
    out.push_str("};\n");
    // The init body first: it finds the default arrays it points into.
    let head = pre(cx, &names::screaming(&t.name), true);
    let mut init = Init { model, cx, arrays: Vec::new() };
    let mut body = String::new();
    for f in t.fields() {
        let target = format!("v->{}", f.name);
        let name = format!("{head}_{}", names::screaming(&f.name));
        init.assign(&mut body, &target, &name, &f.ty, &f.default);
    }
    if !init.arrays.is_empty() {
        out.push('\n');
        for a in &init.arrays {
            out.push_str(a);
            out.push('\n');
        }
    }
    out.push_str(&format!(
        "\nstatic inline void {}{}_init(struct {name} *v)\n{{\n",
        cx.mapping.prefix.as_deref().unwrap_or(""),
        names::snake(&t.name)
    ));
    if body.is_empty() {
        out.push_str("    (void)v;\n");
    } else {
        out.push_str(&body);
    }
    out.push_str("}\n");
    if !t.ids().is_empty() {
        out.push('\n');
        for id in t.ids() {
            out.push_str(&format!(
                "#define {} {}\n",
                pre(cx, &id.name, true),
                lit::string(&id.wire, LANG)
            ));
        }
    }
}

/// The spelling of a builtin: the mapping's, or the support file's.
fn builtin(b: Builtin, cx: &Context) -> String {
    match cx.mapping.types.get(b.key()) {
        Some(ext) => ext.spelling.clone(),
        None => match b {
            Builtin::Rgb => "bi_rgb",
            Builtin::Rgba => "bi_rgba",
            Builtin::Curve => "bi_curve",
            Builtin::Gradient => "bi_gradient",
            Builtin::Ref => "bi_ref",
        }
        .to_string(),
    }
}

/// A schema type's name in code, prefixed unless external.
fn spelling(wire: &str, model: &Model, cx: &Context) -> String {
    match model.type_named(wire) {
        Some(t) if t.external.is_some() => t.name.clone(),
        Some(t) => pre(cx, &t.name, false),
        None => wire.to_string(),
    }
}

fn int_ty(k: IntKind) -> &'static str {
    match k {
        IntKind::I8 => "int8_t",
        IntKind::I16 => "int16_t",
        IntKind::I32 => "int32_t",
        IntKind::I64 => "int64_t",
        IntKind::U8 => "uint8_t",
        IntKind::U16 => "uint16_t",
        IntKind::U32 => "uint32_t",
        IntKind::U64 => "uint64_t",
    }
}

pub fn ty(t: &Ty, model: &Model, cx: &Context) -> String {
    match t {
        Ty::Bool => "bool".into(),
        Ty::Int(k) => int_ty(*k).into(),
        Ty::F32 => "float".into(),
        Ty::F64 => "double".into(),
        Ty::Str => "const char *".into(),
        Ty::Rgb => builtin(Builtin::Rgb, cx),
        Ty::Rgba => builtin(Builtin::Rgba, cx),
        Ty::Curve => builtin(Builtin::Curve, cx),
        Ty::Gradient => builtin(Builtin::Gradient, cx),
        Ty::Named(n) => match model.type_named(n) {
            Some(t) if t.external.is_some() => t.name.clone(),
            Some(t) if t.is_enum() => format!("enum {}", pre(cx, &t.name, false)),
            Some(t) => format!("struct {}", pre(cx, &t.name, false)),
            None => n.clone(),
        },
        Ty::List(inner) => match &cx.mapping.generics.list {
            Some(tpl) => tpl.replace("{T}", &ty(inner, model, cx)),
            None => format!("bi_list_{}", mangle(inner, model)),
        },
        Ty::Optional(inner, boxed) => {
            if *boxed {
                return ptr(&ty(inner, model, cx));
            }
            match &cx.mapping.generics.optional {
                Some(tpl) => tpl.replace("{T}", &ty(inner, model, cx)),
                None => format!("bi_opt_{}", mangle(inner, model)),
            }
        }
        Ty::Ref(s) => match &cx.mapping.generics.reference {
            Some(tpl) => tpl.replace("{S}", &spelling(s, model, cx)),
            None => builtin(Builtin::Ref, cx),
        },
    }
}

/// The element part of a typedef's name: `f32`, `string`, `Vec2`,
/// `enum_Rarity`, `ref_Weapon`, `list_f32`, `opt_string`.
fn mangle(t: &Ty, model: &Model) -> String {
    match t {
        Ty::Bool => "bool".into(),
        Ty::Int(k) => k.text().into(),
        Ty::F32 => "f32".into(),
        Ty::F64 => "f64".into(),
        Ty::Str => "string".into(),
        Ty::Rgb => "rgb".into(),
        Ty::Rgba => "rgba".into(),
        Ty::Curve => "curve".into(),
        Ty::Gradient => "gradient".into(),
        Ty::Named(n) => match model.type_named(n) {
            Some(t) if t.is_enum() => format!("enum_{}", names::identifier(&t.name)),
            Some(t) => names::identifier(&t.name),
            None => names::identifier(n),
        },
        Ty::List(inner) => format!("list_{}", mangle(inner, model)),
        Ty::Optional(inner, boxed) => {
            format!("{}_{}", if *boxed { "ptr" } else { "opt" }, mangle(inner, model))
        }
        Ty::Ref(s) => format!("ref_{}", names::identifier(model.spelling(s))),
    }
}

/// Whether a value of `t` is a scalar — a `{0}` is not its zero.
fn scalar(t: &Ty, model: &Model, cx: &Context) -> bool {
    match t {
        Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Str => true,
        Ty::Rgb | Ty::Rgba | Ty::Curve | Ty::Gradient => false,
        Ty::Named(n) => model.type_named(n).is_some_and(|t| t.external.is_none() && t.is_enum()),
        Ty::List(_) => false,
        Ty::Optional(_, boxed) => *boxed,
        Ty::Ref(_) => cx.mapping.generics.reference.is_none(),
    }
}

/// The typedefs a unit needs, each once, each after the struct it embeds.
#[derive(Default)]
struct Typedefs {
    /// Mangled name, the struct wires it needs complete, the line.
    pending: Vec<(String, Vec<String>, String)>,
    done: Vec<String>,
    complete: Vec<String>,
}

impl Typedefs {
    /// Queues the typedefs `t` needs, inner before outer.
    fn want(&mut self, t: &Ty, model: &Model, cx: &Context) {
        let (inner, list) = match t {
            Ty::List(inner) if cx.mapping.generics.list.is_none() => (inner, true),
            Ty::Optional(inner, false) if cx.mapping.generics.optional.is_none() => (inner, false),
            _ => return,
        };
        self.want(inner, model, cx);
        let name = ty(t, model, cx);
        if self.done.contains(&name) || self.pending.iter().any(|p| p.0 == name) {
            return;
        }
        let elem = ty(inner, model, cx);
        let line = if list {
            format!("typedef struct {{ {}; size_t len; }} {name};", decl(&ptr(&elem), "items"))
        } else {
            format!("typedef struct {{ bool set; {}; }} {name};", decl(&elem, "value"))
        };
        let mut needs = Vec::new();
        Self::needs(t, model, &mut needs);
        self.pending.push((name, needs, line));
    }

    /// The generated structs `t` embeds by value: an optional's inner
    /// struct, through lists and optionals.
    fn needs(t: &Ty, model: &Model, out: &mut Vec<String>) {
        match t {
            Ty::Optional(inner, false) => match inner.as_ref() {
                Ty::Named(n) => {
                    if model.type_named(n).is_some_and(|t| t.external.is_none() && t.is_struct()) {
                        out.push(n.clone());
                    }
                }
                other => Self::needs(other, model, out),
            },
            Ty::List(inner) => Self::needs(inner, model, out),
            _ => {}
        }
    }

    /// Writes every pending typedef whose structs are complete.
    fn flush(&mut self, out: &mut String) {
        let mut wrote = false;
        loop {
            let Some(i) =
                self.pending.iter().position(|p| p.1.iter().all(|w| self.complete.contains(w)))
            else {
                break;
            };
            let (name, _, line) = self.pending.remove(i);
            if !wrote {
                out.push('\n');
                wrote = true;
            }
            out.push_str(&line);
            out.push('\n');
            self.done.push(name);
        }
    }
}

/// The defaults of one struct as its init function's body and the
/// static arrays the lists, curves and gradients point into.
struct Init<'a> {
    model: &'a Model,
    cx: &'a Context<'a>,
    arrays: Vec<String>,
}

impl Init<'_> {
    /// `v->a.b = …;` for every leaf of `t` at `target`; `name` is the
    /// screaming path the default arrays are named after.
    fn assign(&mut self, out: &mut String, target: &str, name: &str, t: &Ty, v: &Value) {
        let (model, cx) = (self.model, self.cx);
        match t {
            Ty::Named(n) => {
                if let Some(d) = self.external(n) {
                    out.push_str(&format!("    {target} = {d};\n"));
                    return;
                }
                match model.type_named(n) {
                    Some(st) if st.is_struct() => {
                        let map = v.as_object();
                        for f in st.fields() {
                            let fv = map.and_then(|m| m.get(&f.wire)).unwrap_or(&f.default);
                            self.assign(
                                out,
                                &format!("{target}.{}", f.name),
                                &format!("{name}_{}", names::screaming(&f.name)),
                                &f.ty,
                                fv,
                            );
                        }
                    }
                    _ => {
                        out.push_str(&format!("    {target} = {};\n", self.expr(t, v, name, false)))
                    }
                }
            }
            Ty::Optional(inner, false) if cx.mapping.generics.optional.is_none() => {
                if v.is_null() {
                    out.push_str(&format!("    {target}.set = false;\n"));
                } else {
                    out.push_str(&format!("    {target}.set = true;\n"));
                    self.assign(out, &format!("{target}.value"), name, inner, v);
                }
            }
            Ty::List(inner) if cx.mapping.generics.list.is_none() => {
                let items: Vec<Value> = v.as_array().cloned().unwrap_or_default();
                let (arr, n) = self.array(&format!("{name}_DEFAULT"), inner, &items);
                out.push_str(&format!("    {target}.items = {arr};\n    {target}.len = {n};\n"));
            }
            Ty::Curve if !cx.mapping.types.contains_key("curve") => {
                let (arr, n) = self.curve(&format!("{name}_DEFAULT"), v);
                out.push_str(&format!("    {target}.points = {arr};\n    {target}.len = {n};\n"));
            }
            Ty::Gradient if !cx.mapping.types.contains_key("gradient") => {
                let (arr, n) = self.gradient(&format!("{name}_DEFAULT"), v);
                out.push_str(&format!("    {target}.stops = {arr};\n    {target}.len = {n};\n"));
            }
            _ => out.push_str(&format!("    {target} = {};\n", self.expr(t, v, name, false))),
        }
    }

    /// An external type's default: the mapping's, or its zero.
    fn external(&self, key: &str) -> Option<String> {
        let ext = self.cx.mapping.types.get(key)?;
        Some(ext.default.clone().unwrap_or_else(|| format!("({}){{0}}", ext.spelling)))
    }

    /// The zero of `t`: a literal for a scalar, `{0}` inside an aggregate,
    /// a compound literal elsewhere.
    fn zero(&self, t: &Ty, agg: bool) -> String {
        let (model, cx) = (self.model, self.cx);
        match t {
            Ty::Bool => "false".into(),
            Ty::Int(_) => "0".into(),
            Ty::F32 => "0.0f".into(),
            Ty::F64 => "0.0".into(),
            Ty::Str => "NULL".into(),
            Ty::Optional(_, true) => "NULL".into(),
            Ty::Ref(_) if cx.mapping.generics.reference.is_none() => "NULL".into(),
            _ if scalar(t, model, cx) => "0".into(),
            _ if agg => "{0}".into(),
            _ => format!("({}){{0}}", ty(t, model, cx)),
        }
    }

    /// A resolved default as a C expression of type `t`: an initializer
    /// inside an aggregate when `agg`, a compound literal otherwise.
    fn expr(&mut self, t: &Ty, v: &Value, name: &str, agg: bool) -> String {
        let (model, cx) = (self.model, self.cx);
        let literal = |t: &Ty, body: String| {
            if agg { format!("{{{body}}}") } else { format!("({}){{{body}}}", ty(t, model, cx)) }
        };
        match t {
            Ty::Bool => if v.as_bool().unwrap_or(false) { "true" } else { "false" }.into(),
            Ty::Int(k) => int_lit(*k, v),
            Ty::F32 => lit::float_of(v, true, LANG),
            Ty::F64 => lit::float_of(v, false, LANG),
            Ty::Str => lit::string(v.as_str().unwrap_or(""), LANG),
            Ty::Rgb => self.external("rgb").unwrap_or_else(|| {
                let (r, g, b, _) = lit::colour(v.as_str().unwrap_or(""));
                literal(t, format!("{r}, {g}, {b}"))
            }),
            Ty::Rgba => self.external("rgba").unwrap_or_else(|| literal(t, rgba(v))),
            Ty::Curve => match self.external("curve") {
                Some(d) => d,
                None => {
                    let (arr, n) = self.curve(name, v);
                    literal(t, format!("{arr}, {n}"))
                }
            },
            Ty::Gradient => match self.external("gradient") {
                Some(d) => d,
                None => {
                    let (arr, n) = self.gradient(name, v);
                    literal(t, format!("{arr}, {n}"))
                }
            },
            Ty::Named(n) => {
                if let Some(d) = self.external(n) {
                    return d;
                }
                match model.type_named(n) {
                    Some(st) if st.is_enum() => {
                        let wire = v.as_str().unwrap_or("");
                        let variant = st
                            .values()
                            .iter()
                            .find(|ev| ev.wire == wire)
                            .or(st.values().first())
                            .map(|ev| ev.name.as_str())
                            .unwrap_or("_empty");
                        pre(cx, variant, true)
                    }
                    Some(st) => {
                        let map = v.as_object();
                        let mut fields = Vec::new();
                        for f in st.fields() {
                            let fv = map.and_then(|m| m.get(&f.wire)).unwrap_or(&f.default);
                            let fname = format!("{name}_{}", names::screaming(&f.name));
                            fields.push(self.expr(&f.ty, fv, &fname, true));
                        }
                        if fields.is_empty() {
                            self.zero(t, agg)
                        } else {
                            literal(t, fields.join(", "))
                        }
                    }
                    None => self.zero(t, agg),
                }
            }
            Ty::List(inner) => {
                if cx.mapping.generics.list.is_some() {
                    return self.zero(t, agg);
                }
                let items: Vec<Value> = v.as_array().cloned().unwrap_or_default();
                let (arr, n) = self.array(name, inner, &items);
                literal(t, format!("{arr}, {n}"))
            }
            Ty::Optional(inner, boxed) => {
                if *boxed || cx.mapping.generics.optional.is_some() {
                    return self.zero(t, agg);
                }
                if v.is_null() {
                    literal(t, format!("false, {}", self.zero(inner, true)))
                } else {
                    let inner = self.expr(inner, v, name, true);
                    literal(t, format!("true, {inner}"))
                }
            }
            Ty::Ref(_) => {
                if cx.mapping.generics.reference.is_some() {
                    return self.zero(t, agg);
                }
                lit::string(v.as_str().unwrap_or(""), LANG)
            }
        }
    }

    /// Declares `static T name[n] = {…};` for `items` and hands back what
    /// the list points at and how many — `NULL, 0` when there is nothing.
    fn array(&mut self, name: &str, elem: &Ty, items: &[Value]) -> (String, usize) {
        if items.is_empty() {
            return ("NULL".into(), 0);
        }
        let mut inits = Vec::new();
        for (i, item) in items.iter().enumerate() {
            inits.push(self.expr(elem, item, &format!("{name}_{i}"), true));
        }
        self.declare(&ty(elem, self.model, self.cx), name, &inits);
        (name.to_string(), items.len())
    }

    fn curve(&mut self, name: &str, v: &Value) -> (String, usize) {
        let points: Vec<String> = v
            .as_array()
            .map(|pts| {
                pts.iter()
                    .map(|p| {
                        let n = |i: usize| {
                            lit::float(p.get(i).and_then(Value::as_f64).unwrap_or(0.0), true, LANG)
                        };
                        format!("{{{}, {}, {}, {}}}", n(0), n(1), n(2), n(3))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if points.is_empty() {
            return ("NULL".into(), 0);
        }
        self.declare("bi_curve_point", name, &points);
        (name.to_string(), points.len())
    }

    fn gradient(&mut self, name: &str, v: &Value) -> (String, usize) {
        let stops: Vec<String> = v
            .as_array()
            .map(|st| {
                st.iter()
                    .map(|s| {
                        let t =
                            lit::float(s.get(0).and_then(Value::as_f64).unwrap_or(0.0), true, LANG);
                        format!("{{{t}, {{{}}}}}", rgba(s.get(1).unwrap_or(&Value::Null)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if stops.is_empty() {
            return ("NULL".into(), 0);
        }
        self.declare("bi_gradient_stop", name, &stops);
        (name.to_string(), stops.len())
    }

    fn declare(&mut self, elem: &str, name: &str, inits: &[String]) {
        self.arrays.push(format!(
            "static {} = {{{}}};",
            decl(elem, &format!("{name}[{}]", inits.len())),
            inits.join(", ")
        ));
    }
}

/// The channels of an rgba colour, `r, g, b, a`.
fn rgba(v: &Value) -> String {
    let (r, g, b, a) = lit::colour(v.as_str().unwrap_or(""));
    format!("{r}, {g}, {b}, {a}")
}

/// An integer literal that fits its type: 64-bit ones carry `LL`/`ULL`,
/// and the most negative one is spelled the way C can read it.
fn int_lit(k: IntKind, v: &Value) -> String {
    let s = lit::int(v);
    match k {
        IntKind::I64 if s == "-9223372036854775808" => "(-9223372036854775807LL - 1)".into(),
        IntKind::I64 => format!("{s}LL"),
        IntKind::U64 => format!("{s}ULL"),
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::fixture;
    use super::*;

    fn cx<'a>(m: &'a super::super::super::LangMapping, pkg: Option<&'a str>) -> Context<'a> {
        Context { pkg, mapping: m, dir_name: "out" }
    }

    fn mapped(text: &str) -> super::super::super::LangMapping {
        super::super::super::Mapping::parse(text).unwrap().for_lang(Lang::C)
    }

    const EXPECTED: &str = r#"// generated by `bi gen struct` from weapons.bischema — do not edit
#ifndef BI_GEN_WEAPONS_H
#define BI_GEN_WEAPONS_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "bi_types.h"

/* Tier */
enum Rarity {
    RARITY_COMMON,
    RARITY_VERY_RARE,
};

/* The names the data stores, in value order. */
static const char *const RARITY_NAMES[2] = {"common", "very rare"};

struct Vec2;
struct Weapon;

struct Vec2 {
    float x;
    float y;
};

static inline void vec2_init(struct Vec2 *v)
{
    v->x = 0.0f;
    v->y = 0.0f;
}

typedef struct { const char **items; size_t len; } bi_list_string;
typedef struct { bool set; const char *value; } bi_opt_string;

/* A thing */
struct Weapon {
    const char *name;
    /* 0..999 */
    int32_t damage;
    enum Rarity rarity;
    struct Vec2 offset;
    bi_list_string tags;
    bi_opt_string notes;
    bi_rgb tint;
    bi_ref owner; /* ref<Weapon>, required */
    bool type;
};

static const char *WEAPON_TAGS_DEFAULT[1] = {"a"};

static inline void weapon_init(struct Weapon *v)
{
    v->name = "Sword \"x\"";
    v->damage = 10;
    v->rarity = RARITY_COMMON;
    v->offset.x = 0.5f;
    v->offset.y = 0.0f;
    v->tags.items = WEAPON_TAGS_DEFAULT;
    v->tags.len = 1;
    v->notes.set = false;
    v->tint = (bi_rgb){200, 200, 200};
    v->owner = "";
    v->type = false;
}

#define WEAPON_RUSTY_SWORD "rusty_sword"
#define WEAPON_DAGGER "dagger"

#endif /* BI_GEN_WEAPONS_H */
"#;

    #[test]
    fn the_fixture_as_c() {
        let m = fixture::model(Lang::C, true, "");
        let lm = Default::default();
        let text = C.unit(&m.units[0], &m, &cx(&lm, None));
        assert_eq!(text, EXPECTED);
        let support = C.support(&m, &cx(&lm, None)).unwrap();
        assert!(support.starts_with(
            "// generated by `bi gen struct` from the bi format's builtins — do not edit\n#ifndef BI_TYPES_H\n#define BI_TYPES_H\n"
        ));
        assert!(support.contains("typedef struct { uint8_t r, g, b; } bi_rgb;\n"));
        assert!(support.contains("typedef const char *bi_ref;\n"));
        assert!(!support.contains("bi_curve"));
        assert!(!support.contains("bi_rgba"));
        assert!(support.ends_with("\n#endif /* BI_TYPES_H */\n"));
    }

    #[test]
    fn the_prefix_goes_on_every_name() {
        let mapping = "[c]\nprefix = \"gs_\"\n";
        let m = fixture::model(Lang::C, true, mapping);
        let lm = mapped(mapping);
        let text = C.unit(&m.units[0], &m, &cx(&lm, None));
        for s in [
            "enum gs_Rarity {\n    GS_RARITY_COMMON,\n    GS_RARITY_VERY_RARE,\n};",
            "static const char *const GS_RARITY_NAMES[2] = {\"common\", \"very rare\"};",
            "struct gs_Vec2;\nstruct gs_Weapon;\n",
            "struct gs_Weapon {\n",
            "    enum gs_Rarity rarity;\n",
            "    struct gs_Vec2 offset;\n",
            "    bi_ref owner; /* ref<gs_Weapon>, required */\n",
            "static const char *GS_WEAPON_TAGS_DEFAULT[1] = {\"a\"};",
            "static inline void gs_weapon_init(struct gs_Weapon *v)\n",
            "    v->rarity = GS_RARITY_COMMON;\n",
            "    v->tags.items = GS_WEAPON_TAGS_DEFAULT;\n",
            "#define GS_WEAPON_RUSTY_SWORD \"rusty_sword\"\n",
        ] {
            assert!(text.contains(s), "{s}\n---\n{text}");
        }
        assert!(text.contains("bi_list_string tags;"), "support names take no prefix");
        assert!(text.contains("bi_rgb tint;"));
        assert!(!text.contains("struct Weapon"));
    }

    #[test]
    fn every_builtin() {
        let m = fixture::model_of(Lang::C, fixture::EVERY_BUILTIN, &[], "");
        let lm = Default::default();
        let support = C.support(&m, &cx(&lm, None)).unwrap();
        for s in [
            "typedef struct { uint8_t r, g, b; } bi_rgb;\n",
            "typedef struct { uint8_t r, g, b, a; } bi_rgba;\n",
            "typedef struct { float x, y, in_, out; } bi_curve_point;\n",
            "typedef struct { bi_curve_point *points; size_t len; } bi_curve;\n",
            "typedef struct { float t; bi_rgba color; } bi_gradient_stop;\n",
            "typedef struct { bi_gradient_stop *stops; size_t len; } bi_gradient;\n",
            "typedef const char *bi_ref;\n",
        ] {
            assert!(support.contains(s), "{s}");
        }
        assert_eq!(support.matches("} bi_rgba;").count(), 1);
        assert!(support.find("} bi_rgba;").unwrap() < support.find("bi_gradient_stop").unwrap());
        let text = C.unit(&m.units[0], &m, &cx(&lm, None));
        for s in [
            "typedef struct { bool set; bi_ref value; } bi_opt_ref_Fx;\n",
            "typedef struct { float *items; size_t len; } bi_list_f32;\ntypedef struct { bi_list_f32 *items; size_t len; } bi_list_list_f32;\n",
            "    bi_rgb tint;\n",
            "    bi_rgba glow;\n",
            "    bi_curve falloff;\n",
            "    bi_gradient trail;\n",
            "    bi_opt_ref_Fx next;\n",
            "    bi_list_list_f32 rows;\n",
            "    uint64_t big;\n",
            "static bi_curve_point FX_FALLOFF_DEFAULT[2] = {{0.0f, 0.0f, 1.0f, 1.0f}, {1.0f, 1.0f, 1.0f, 1.0f}};\n",
            "static bi_gradient_stop FX_TRAIL_DEFAULT[2] = {{0.0f, {0, 0, 0, 255}}, {1.0f, {255, 255, 255, 255}}};\n",
            "static float FX_ROWS_DEFAULT_0[2] = {1.0f, 2.0f};\nstatic bi_list_f32 FX_ROWS_DEFAULT[2] = {{FX_ROWS_DEFAULT_0, 2}, {NULL, 0}};\n",
            "    v->tint = (bi_rgb){0, 0, 0};\n",
            "    v->glow = (bi_rgba){255, 0, 0, 128};\n",
            "    v->falloff.points = FX_FALLOFF_DEFAULT;\n    v->falloff.len = 2;\n",
            "    v->trail.stops = FX_TRAIL_DEFAULT;\n    v->trail.len = 2;\n",
            "    v->next.set = false;\n",
            "    v->rows.items = FX_ROWS_DEFAULT;\n    v->rows.len = 2;\n",
            "    v->big = 18446744073709551615ULL;\n",
        ] {
            assert!(text.contains(s), "{s}\n---\n{text}");
        }
        // The arrays are declared before the function that points into them.
        assert!(
            text.find("FX_ROWS_DEFAULT_0[2]").unwrap() < text.find("FX_ROWS_DEFAULT[2]").unwrap()
        );
        assert!(text.find("FX_ROWS_DEFAULT[2]").unwrap() < text.find("fx_init").unwrap());

        // A mapped gradient leaves the support file without it, curve stays.
        let mapping = "[c.types]\ngradient = \"grad_t\"\n";
        let m = fixture::model_of(Lang::C, fixture::EVERY_BUILTIN, &[], mapping);
        let lm = mapped(mapping);
        let support = C.support(&m, &cx(&lm, None)).unwrap();
        assert!(!support.contains("bi_gradient"));
        assert!(support.contains("bi_curve"));
        let text = C.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("    grad_t trail;\n"));
        assert!(text.contains("    v->trail = (grad_t){0};\n"));
    }

    #[test]
    fn a_cycle_is_a_pointer_and_a_list_is_forward_declared() {
        let m = fixture::model_of(Lang::C, fixture::CYCLE, &[], "");
        let lm = Default::default();
        let text = C.unit(&m.units[0], &m, &cx(&lm, None));
        for s in [
            "struct Node;\n",
            "typedef struct { struct Node *items; size_t len; } bi_list_Node;\n",
            "struct Node {\n    struct Node *next;\n    bi_list_Node kids;\n};\n",
            "    v->next = NULL;\n",
            "    v->kids.items = NULL;\n    v->kids.len = 0;\n",
        ] {
            assert!(text.contains(s), "{s}\n---\n{text}");
        }
        assert!(text.find("bi_list_Node;").unwrap() < text.find("struct Node {").unwrap());
        assert!(C.support(&m, &cx(&lm, None)).is_none());
        assert!(!text.contains("bi_types"));
    }

    #[test]
    fn an_optional_struct_is_typedefed_after_the_struct_it_holds() {
        let s = r#"{"$dialect":"bi/1","types":{
          "W":{"kind":"struct","fields":[
            {"name":"o","type":"optional<V>","default":{"x":2}},
            {"name":"vs","type":"list<V>","default":[{"x":1},{"x":3}]},
            {"name":"r","type":"optional<Rarity>"}]},
          "V":{"kind":"struct","fields":[{"name":"x","type":"f32"},{"name":"ns","type":"list<i32>","default":[1]}]},
          "Rarity":{"kind":"enum","values":["common","rare"]}}}"#;
        let m = fixture::model_of(Lang::C, s, &[], "");
        let lm = Default::default();
        let text = C.unit(&m.units[0], &m, &cx(&lm, None));
        let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s}\n---\n{text}"));
        assert!(at("struct V {") < at("typedef struct { bool set; struct V value; } bi_opt_V;"));
        assert!(at("bi_opt_V;") < at("struct W {"));
        assert!(
            at("typedef struct { struct V *items; size_t len; } bi_list_V;") < at("struct W {")
        );
        assert!(
            text.contains("typedef struct { bool set; enum Rarity value; } bi_opt_enum_Rarity;")
        );
        assert!(text.contains("    v->o.set = true;\n    v->o.value.x = 2.0f;\n    v->o.value.ns.items = W_O_NS_DEFAULT;\n    v->o.value.ns.len = 1;\n"));
        assert!(text.contains("static int32_t W_O_NS_DEFAULT[1] = {1};\n"));
        assert!(text.contains("static int32_t W_VS_DEFAULT_0_NS[1] = {1};\nstatic int32_t W_VS_DEFAULT_1_NS[1] = {1};\nstatic struct V W_VS_DEFAULT[2] = {{1.0f, {W_VS_DEFAULT_0_NS, 1}}, {3.0f, {W_VS_DEFAULT_1_NS, 1}}};\n"));
        assert!(text.contains("    v->r.set = false;\n"));
    }

    #[test]
    fn an_empty_struct_is_padded_and_wire_names_are_kept() {
        let s = r#"{"$dialect":"bi/1","types":{
          "E":{"kind":"struct","fields":[]},
          "S":{"kind":"struct","fields":[{"name":"default","type":"i32"}]}}}"#;
        let m = fixture::model_of(Lang::C, s, &[], "");
        let lm = Default::default();
        let text = C.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(
            text.contains("struct E {\n    char bi_empty_; /* C refuses an empty struct */\n};\n")
        );
        assert!(text.contains("static inline void e_init(struct E *v)\n{\n    (void)v;\n}\n"));
        assert!(text.contains("    int32_t default_; /* \"default\" in the data */\n"));
        assert!(text.contains("    v->default_ = 0;\n"));
        assert!(C.support(&m, &cx(&lm, None)).is_none());
    }

    #[test]
    fn an_external_type_is_spelled_not_generated() {
        let mapping =
            "[c.types]\nVec2 = { as = \"vec2\", import = \"#include \\\"math/vec2.h\\\"\" }\n";
        let m = fixture::model(Lang::C, false, mapping);
        let lm = mapped(mapping);
        let text = C.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(!text.contains("struct Vec2"));
        assert!(text.contains("    vec2 offset;\n"));
        assert!(text.contains("    v->offset = (vec2){0};\n"));
        assert!(text.contains("#include \"bi_types.h\"\n#include \"math/vec2.h\"\n"));
        let mapping = "[c.types]\nVec2 = { as = \"vec2\", default = \"vec2_zero()\" }\n[c]\nheader = \"#pragma GCC diagnostic ignored \\\"-Wunused\\\"\"\n";
        let m = fixture::model(Lang::C, false, mapping);
        let lm = mapped(mapping);
        let text = C.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("    v->offset = vec2_zero();\n"));
        assert!(text.starts_with("// generated by `bi gen struct` from weapons.bischema — do not edit\n#ifndef BI_GEN_WEAPONS_H\n#define BI_GEN_WEAPONS_H\n\n#pragma GCC diagnostic ignored \"-Wunused\"\n\n#include <stdbool.h>\n"));
    }

    #[test]
    fn generics_templates_replace_the_typedefs() {
        let mapping = "[c.generics]\nlist = \"arr_{T}\"\nref = \"{S}_handle\"\n";
        let m = fixture::model(Lang::C, false, mapping);
        let lm = mapped(mapping);
        let text = C.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("    arr_const char *tags;\n"));
        assert!(text.contains("    v->tags = (arr_const char *){0};\n"));
        assert!(!text.contains("bi_list_string"));
        assert!(text.contains("typedef struct { bool set; const char *value; } bi_opt_string;\n"));
        assert!(text.contains("    Weapon_handle owner; /* ref<Weapon>, required */\n"));
        assert!(text.contains("    v->owner = (Weapon_handle){0};\n"));
        let support = C.support(&m, &cx(&lm, None)).unwrap();
        assert!(support.contains("bi_rgb"));
        assert!(!support.contains("bi_ref"));
    }
}
