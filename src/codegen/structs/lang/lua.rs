//! The Lua backend: a module returning a table of the types, each struct
//! a [LuaLS](https://luals.github.io/)-annotated class with a `new(t)`
//! that copies `t` and fills what it lacks from the defaults, each enum
//! a table of its names, ids as a table under the type, and
//! `bi_types.lua` for the builtins. Lua 5.3+; on 5.1 and LuaJIT a `u64`
//! is a double, which its doc comment says. See
//! `docs/specs/gen-struct.md`.

use serde_json::Value;

use super::super::model::{Builtin, Field, Ty, Type};
use super::super::{
    Backend, Context, DataUnit, Lang, Model, SUPPORT_STEM, Unit, header, lit, names,
};
use crate::props::schema::IntKind;

pub struct Lua;

const LANG: Lang = Lang::Lua;

impl Backend for Lua {
    fn unit(&self, unit: &Unit, model: &Model, cx: &Context) -> String {
        let mut out = header("--", &unit.source);
        out.push('\n');
        // A ref is a string: only the other builtins live in the support file.
        if model.builtins_in(unit).iter().any(|b| *b != Builtin::Ref) {
            out.push_str(&format!(
                "local bi = require({})\n",
                lit::string(&module_path(SUPPORT_STEM, cx), LANG)
            ));
        }
        for i in &imports(unit, cx) {
            out.push_str(i);
            out.push('\n');
        }
        if let Some(h) = &cx.mapping.header {
            out.push_str(h);
            out.push('\n');
        }
        out.push_str("local M = {}\n");
        for t in unit.types.iter().filter(|t| t.external.is_none()) {
            out.push('\n');
            if t.is_enum() {
                write_enum(&mut out, t);
            } else {
                write_struct(&mut out, t, model, cx);
            }
        }
        out.push_str("\nreturn M\n");
        out
    }

    fn data(&self, data: &DataUnit, unit: &Unit, model: &Model, cx: &Context) -> String {
        let mut out = header("--", &data.source);
        out.push('\n');
        let module = module_local(&unit.stem);
        out.push_str(&format!(
            "local {module} = require({})\n",
            lit::string(&module_path(&unit.stem, cx), LANG)
        ));
        if model.builtins_in(unit).iter().any(|b| *b != Builtin::Ref) {
            out.push_str(&format!(
                "local bi = require({})\n",
                lit::string(&module_path(SUPPORT_STEM, cx), LANG)
            ));
        }
        for i in &imports(unit, cx) {
            out.push_str(i);
            out.push('\n');
        }
        if let Some(h) = &cx.mapping.header {
            out.push_str(h);
            out.push('\n');
        }
        out.push_str("local M = {}\n");
        for wire in data.types() {
            let Some(t) = model.type_named(wire) else { continue };
            let ty = Ty::Named(wire.to_string());
            let base = format!("M.{}", t.name);
            out.push_str(&format!("\n{base} = {{}}\n\n"));
            for inst in data.of(wire) {
                out.push_str(&format!(
                    "{} = {}\n",
                    access(&base, &inst.name),
                    value_in(&ty, &inst.value, model, cx, &module)
                ));
            }
            out.push_str(&format!(
                "\n---Every {} of {}, in its order, with its id.\n{base}.all = {{\n",
                t.name, data.source
            ));
            for inst in data.of(wire) {
                out.push_str(&format!(
                    "  {{ id = {}, value = {} }},\n",
                    lit::string(&inst.wire, LANG),
                    access(&base, &inst.name)
                ));
            }
            out.push_str("}\n");
            out.push_str(&format!(
                "\n---@param id string\n---@return {}?\nfunction {base}.find(id)\n  for _, entry in ipairs({base}.all) do\n    if entry.id == id then return entry.value end\n  end\n  return nil\nend\n",
                t.name
            ));
        }
        out.push_str("\nreturn M\n");
        out
    }

    fn support(&self, model: &Model, _cx: &Context) -> Option<String> {
        // A ref is a string: it needs nothing. Gradient needs Rgba even
        // when no field is an rgba.
        let wanted: Vec<Builtin> = Builtin::ALL
            .into_iter()
            .filter(|b| match b {
                Builtin::Ref => false,
                Builtin::Rgba => {
                    model.builtins.contains(b) || model.builtins.contains(&Builtin::Gradient)
                }
                other => model.builtins.contains(other),
            })
            .collect();
        if wanted.is_empty() {
            return None;
        }
        let mut out = header("--", "the bi format's builtins");
        out.push_str("\nlocal M = {}\n");
        for b in wanted {
            out.push('\n');
            out.push_str(match b {
                Builtin::Rgb => RGB,
                Builtin::Rgba => RGBA,
                Builtin::Curve => CURVE,
                Builtin::Gradient => GRADIENT,
                Builtin::Ref => unreachable!(),
            });
        }
        out.push_str("\nreturn M\n");
        out.into()
    }
}

const RGB: &str = "---@class Rgb
---@field r integer
---@field g integer
---@field b integer
M.Rgb = {}
M.Rgb.__index = M.Rgb

---@param t table?
---@return Rgb
function M.Rgb.new(t)
  t = t or {}
  local v = setmetatable({}, M.Rgb)
  if t.r ~= nil then v.r = t.r else v.r = 0 end
  if t.g ~= nil then v.g = t.g else v.g = 0 end
  if t.b ~= nil then v.b = t.b else v.b = 0 end
  return v
end
";

const RGBA: &str = "---@class Rgba
---@field r integer
---@field g integer
---@field b integer
---@field a integer
M.Rgba = {}
M.Rgba.__index = M.Rgba

---@param t table?
---@return Rgba
function M.Rgba.new(t)
  t = t or {}
  local v = setmetatable({}, M.Rgba)
  if t.r ~= nil then v.r = t.r else v.r = 0 end
  if t.g ~= nil then v.g = t.g else v.g = 0 end
  if t.b ~= nil then v.b = t.b else v.b = 0 end
  if t.a ~= nil then v.a = t.a else v.a = 255 end
  return v
end
";

const CURVE: &str =
    "---A point of a tuning curve over 0..1: position and the two tangents as slopes.
---@class CurvePoint
---@field x number
---@field y number
---@field [\"in\"] number
---@field out number
M.CurvePoint = {}
M.CurvePoint.__index = M.CurvePoint

---@param t table?
---@return CurvePoint
function M.CurvePoint.new(t)
  t = t or {}
  local v = setmetatable({}, M.CurvePoint)
  if t.x ~= nil then v.x = t.x else v.x = 0.0 end
  if t.y ~= nil then v.y = t.y else v.y = 0.0 end
  if t[\"in\"] ~= nil then v[\"in\"] = t[\"in\"] else v[\"in\"] = 0.0 end
  if t.out ~= nil then v.out = t.out else v.out = 0.0 end
  return v
end

---@class Curve
---@field points CurvePoint[]
M.Curve = {}
M.Curve.__index = M.Curve

---@param t table?
---@return Curve
function M.Curve.new(t)
  t = t or {}
  local v = setmetatable({}, M.Curve)
  if t.points ~= nil then v.points = t.points else v.points = {} end
  return v
end
";

const GRADIENT: &str = "---A colour stop over 0..1.
---@class GradientStop
---@field t number
---@field color Rgba
M.GradientStop = {}
M.GradientStop.__index = M.GradientStop

---@param t table?
---@return GradientStop
function M.GradientStop.new(t)
  t = t or {}
  local v = setmetatable({}, M.GradientStop)
  if t.t ~= nil then v.t = t.t else v.t = 0.0 end
  v.color = M.Rgba.new(t.color)
  return v
end

---@class Gradient
---@field stops GradientStop[]
M.Gradient = {}
M.Gradient.__index = M.Gradient

---@param t table?
---@return Gradient
function M.Gradient.new(t)
  t = t or {}
  local v = setmetatable({}, M.Gradient)
  if t.stops ~= nil then v.stops = t.stops else v.stops = {} end
  return v
end
";

/// What `require` takes for a generated file: `pkg.stem`, or the stem.
fn module_path(stem: &str, cx: &Context) -> String {
    match cx.pkg {
        Some(p) => format!("{p}.{stem}"),
        None => stem.to_string(),
    }
}

/// The local a data file binds its schema's module to: the stem as an
/// identifier, kept clear of keywords and of the file's own `bi` and `M`.
fn module_local(stem: &str) -> String {
    let name = names::snake(stem);
    if name == "bi" || name == "M" || names::reserved(LANG, &name) {
        format!("{name}_")
    } else {
        name
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

/// Whether `s` can follow a `.` and stand bare in a table constructor.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !names::reserved(LANG, s)
}

/// `base.key`, or `base["key"]` when the key is not an identifier.
fn access(base: &str, key: &str) -> String {
    if is_identifier(key) {
        format!("{base}.{key}")
    } else {
        format!("{base}[{}]", lit::string(key, LANG))
    }
}

/// `key`, or `["key"]`, on the left of `=` in a table constructor.
fn table_key(key: &str) -> String {
    if is_identifier(key) { key.to_string() } else { format!("[{}]", lit::string(key, LANG)) }
}

fn doc(out: &mut String, text: Option<&str>) {
    if let Some(d) = text {
        for line in d.lines() {
            out.push_str(&format!("---{line}\n"));
        }
    }
}

fn write_enum(out: &mut String, t: &Type) {
    doc(out, t.doc.as_deref());
    let wires: Vec<String> = t.values().iter().map(|v| lit::string(&v.wire, LANG)).collect();
    let alias = if wires.is_empty() { "string".to_string() } else { wires.join("|") };
    out.push_str(&format!("---@alias {} {alias}\n", t.name));
    out.push_str(&format!("M.{} = {{\n", t.name));
    for v in t.values() {
        out.push_str(&format!("  {} = {},\n", table_key(&v.name), lit::string(&v.wire, LANG)));
    }
    out.push_str("}\n");
    out.push_str("---The names the data stores, in value order.\n");
    if wires.is_empty() {
        out.push_str(&format!("M.{}Names = {{}}\n", t.name));
    } else {
        out.push_str(&format!("M.{}Names = {{ {} }}\n", t.name, wires.join(", ")));
    }
}

/// The `ref` a field's type reaches, through lists and optionals.
fn ref_target(t: &Ty) -> Option<&str> {
    match t {
        Ty::Ref(s) => Some(s),
        Ty::List(inner) | Ty::Optional(inner, _) => ref_target(inner),
        _ => None,
    }
}

fn mentions_u64(t: &Ty) -> bool {
    match t {
        Ty::Int(IntKind::U64) => true,
        Ty::List(inner) | Ty::Optional(inner, _) => mentions_u64(inner),
        _ => false,
    }
}

/// What follows `# ` on a field's annotation: the doc, the range, the
/// ref's target and whether it is required, the `u64` caveat.
fn field_note(f: &Field, model: &Model) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(d) = &f.doc {
        parts.push(d.lines().collect::<Vec<_>>().join(" "));
    }
    if let Some(r) = &f.range {
        parts.push(r.clone());
    }
    if let Some(s) = ref_target(&f.ty) {
        let mut r = format!("ref<{}>", model.spelling(s));
        if f.required_ref {
            r.push_str(", required");
        }
        parts.push(r);
    }
    if mentions_u64(&f.ty) {
        parts.push("u64: exact on Lua 5.3+, a double on 5.1 and LuaJIT".to_string());
    }
    if parts.is_empty() { None } else { Some(parts.join("; ")) }
}

fn write_struct(out: &mut String, t: &Type, model: &Model, cx: &Context) {
    doc(out, t.doc.as_deref());
    out.push_str(&format!("---@class {}\n", t.name));
    for f in t.fields() {
        out.push_str(&format!("---@field {} {}", table_key(&f.wire), ty(&f.ty, model, cx)));
        if let Some(n) = field_note(f, model) {
            out.push_str(&format!(" # {n}"));
        }
        out.push('\n');
    }
    out.push_str(&format!("M.{} = {{}}\n", t.name));
    out.push_str(&format!("M.{0}.__index = M.{0}\n\n", t.name));
    out.push_str(&format!("---@param t table?\n---@return {}\n", t.name));
    out.push_str(&format!("function M.{}.new(t)\n", t.name));
    out.push_str("  t = t or {}\n");
    out.push_str(&format!("  local v = setmetatable({{}}, M.{})\n", t.name));
    for f in t.fields() {
        let given = access("t", &f.wire);
        let slot = access("v", &f.wire);
        let default = value(&f.ty, &f.default, model, cx);
        let taken = wrap(&f.ty, &given, model, cx).unwrap_or_else(|| given.clone());
        let line = match (default.as_str(), taken == given) {
            ("nil", true) => format!("{slot} = {given}"),
            ("nil", false) => format!("if {given} ~= nil then {slot} = {taken} end"),
            _ => format!("if {given} ~= nil then {slot} = {taken} else {slot} = {default} end"),
        };
        out.push_str(&format!("  {line}\n"));
    }
    out.push_str("  return v\nend\n");
    if !t.ids().is_empty() {
        out.push_str(&format!("\nM.{}.ids = {{\n", t.name));
        for id in t.ids() {
            out.push_str(&format!(
                "  {} = {},\n",
                table_key(&id.name),
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
        None => support_name(b).to_string(),
    }
}

fn support_name(b: Builtin) -> &'static str {
    match b {
        Builtin::Rgb => "Rgb",
        Builtin::Rgba => "Rgba",
        Builtin::Curve => "Curve",
        Builtin::Gradient => "Gradient",
        Builtin::Ref => "string",
    }
}

/// The LuaLS annotation type of `t`.
pub fn ty(t: &Ty, model: &Model, cx: &Context) -> String {
    match t {
        Ty::Bool => "boolean".into(),
        Ty::Int(_) => "integer".into(),
        Ty::F32 | Ty::F64 => "number".into(),
        Ty::Str => "string".into(),
        Ty::Rgb => builtin(Builtin::Rgb, cx),
        Ty::Rgba => builtin(Builtin::Rgba, cx),
        Ty::Curve => builtin(Builtin::Curve, cx),
        Ty::Gradient => builtin(Builtin::Gradient, cx),
        Ty::Named(n) => model.spelling(n).to_string(),
        Ty::List(inner) => match &cx.mapping.generics.list {
            Some(tpl) => tpl.replace("{T}", &ty(inner, model, cx)),
            None => format!("{}[]", ty(inner, model, cx)),
        },
        // A box means nothing in Lua: a table is a reference already.
        Ty::Optional(inner, _) => match &cx.mapping.generics.optional {
            Some(tpl) => tpl.replace("{T}", &ty(inner, model, cx)),
            None => format!("{}?", ty(inner, model, cx)),
        },
        Ty::Ref(s) => match &cx.mapping.generics.reference {
            Some(tpl) => tpl.replace("{S}", model.spelling(s)),
            None => "string".into(),
        },
    }
}

/// The constructor call that gives a value of type `t` its shape and
/// defaults — `M.Vec2.new(expr)`, `bi.Rgb.new(expr)` — when it has one:
/// a generated struct or an unmapped builtin, itself or through an
/// optional. Lists and everything else are taken as they come.
fn wrap(t: &Ty, expr: &str, model: &Model, cx: &Context) -> Option<String> {
    match t {
        Ty::Optional(inner, _) => wrap(inner, expr, model, cx),
        Ty::Rgb | Ty::Rgba | Ty::Curve | Ty::Gradient => {
            let b = t.builtin()?;
            if cx.mapping.types.contains_key(b.key()) {
                None
            } else {
                Some(format!("bi.{}.new({expr})", support_name(b)))
            }
        }
        Ty::Named(n) => match model.type_named(n) {
            Some(t) if t.is_struct() && t.external.is_none() => {
                Some(format!("M.{}.new({expr})", t.name))
            }
            _ => None,
        },
        _ => None,
    }
}

/// An integer literal Lua 5.3+ reads back to the same bits: a `u64`
/// above `i64::MAX` as hex, which wraps, where the decimal would become a
/// float.
fn int(v: &Value) -> String {
    match v.as_u64() {
        Some(n) if n > i64::MAX as u64 => format!("0x{n:x}"),
        _ => lit::int(v),
    }
}

/// A resolved default as a Lua expression of type `t`, the unit's own
/// structs built through `M`.
pub fn value(t: &Ty, v: &Value, model: &Model, cx: &Context) -> String {
    value_in(t, v, model, cx, "M")
}

/// A resolved value as a Lua expression of type `t`; `module` is what
/// precedes `.Vec2.new` for the schema's structs — `M` in their own
/// file, the required module's local in a data file.
pub fn value_in(t: &Ty, v: &Value, model: &Model, cx: &Context, module: &str) -> String {
    let external = |key: &str| -> Option<String> {
        let ext = cx.mapping.types.get(key)?;
        Some(ext.default.clone().unwrap_or_else(|| "nil".to_string()))
    };
    let float = |v: &Value| lit::float_of(v, true, LANG);
    match t {
        Ty::Bool => if v.as_bool().unwrap_or(false) { "true" } else { "false" }.into(),
        Ty::Int(_) => int(v),
        Ty::F32 | Ty::F64 => float(v),
        Ty::Str => lit::string(v.as_str().unwrap_or(""), LANG),
        Ty::Rgb => external("rgb").unwrap_or_else(|| {
            let (r, g, b, _) = lit::colour(v.as_str().unwrap_or(""));
            format!("bi.Rgb.new({{ r = {r}, g = {g}, b = {b} }})")
        }),
        Ty::Rgba => external("rgba").unwrap_or_else(|| rgba(v.as_str().unwrap_or(""))),
        Ty::Curve => external("curve").unwrap_or_else(|| {
            let points: Vec<String> = v
                .as_array()
                .map(|pts| {
                    pts.iter()
                        .map(|p| {
                            let n = |i: usize| float(p.get(i).unwrap_or(&Value::Null));
                            format!(
                                "bi.CurvePoint.new({{ x = {}, y = {}, [\"in\"] = {}, out = {} }})",
                                n(0),
                                n(1),
                                n(2),
                                n(3)
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("bi.Curve.new({{ points = {} }})", table(&points))
        }),
        Ty::Gradient => external("gradient").unwrap_or_else(|| {
            let stops: Vec<String> = v
                .as_array()
                .map(|st| {
                    st.iter()
                        .map(|s| {
                            let t = float(s.get(0).unwrap_or(&Value::Null));
                            let c = rgba(s.get(1).and_then(Value::as_str).unwrap_or(""));
                            format!("bi.GradientStop.new({{ t = {t}, color = {c} }})")
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("bi.Gradient.new({{ stops = {} }})", table(&stops))
        }),
        Ty::Named(n) => {
            if let Some(d) = external(n) {
                return d;
            }
            match model.type_named(n) {
                Some(t) if t.is_enum() => {
                    let wire = v.as_str().unwrap_or("");
                    let wire = t
                        .values()
                        .iter()
                        .find(|ev| ev.wire == wire)
                        .or(t.values().first())
                        .map(|ev| ev.wire.as_str())
                        .unwrap_or("");
                    lit::string(wire, LANG)
                }
                Some(t) => {
                    let map = v.as_object();
                    let fields: Vec<String> = t
                        .fields()
                        .iter()
                        .map(|f: &Field| {
                            let fv = map.and_then(|m| m.get(&f.wire)).unwrap_or(&f.default);
                            format!(
                                "{} = {}",
                                table_key(&f.wire),
                                value_in(&f.ty, fv, model, cx, module)
                            )
                        })
                        .collect();
                    format!("{module}.{}.new({})", t.name, table(&fields))
                }
                None => "nil".into(),
            }
        }
        Ty::List(inner) => {
            let items: Vec<String> = v
                .as_array()
                .map(|a| a.iter().map(|x| value_in(inner, x, model, cx, module)).collect())
                .unwrap_or_default();
            table(&items)
        }
        Ty::Optional(inner, _) => {
            if v.is_null() {
                "nil".into()
            } else {
                value_in(inner, v, model, cx, module)
            }
        }
        Ty::Ref(_) => lit::string(v.as_str().unwrap_or(""), LANG),
    }
}

/// `{ a, b }`, or `{}`.
fn table(items: &[String]) -> String {
    if items.is_empty() { "{}".to_string() } else { format!("{{ {} }}", items.join(", ")) }
}

fn rgba(s: &str) -> String {
    let (r, g, b, a) = lit::colour(s);
    format!("bi.Rgba.new({{ r = {r}, g = {g}, b = {b}, a = {a} }})")
}

#[cfg(test)]
mod tests {
    use super::super::super::fixture;
    use super::*;

    fn cx<'a>(m: &'a super::super::super::LangMapping, pkg: Option<&'a str>) -> Context<'a> {
        Context { pkg, mapping: m, dir_name: "out" }
    }

    const EXPECTED: &str = r#"-- generated by `bi gen struct` from weapons.bischema — do not edit

local bi = require("gen.scriptableobjects.bi_types")
local M = {}

---Tier
---@alias Rarity "common"|"very rare"
M.Rarity = {
  common = "common",
  very_rare = "very rare",
}
---The names the data stores, in value order.
M.RarityNames = { "common", "very rare" }

---@class Vec2
---@field x number
---@field y number
M.Vec2 = {}
M.Vec2.__index = M.Vec2

---@param t table?
---@return Vec2
function M.Vec2.new(t)
  t = t or {}
  local v = setmetatable({}, M.Vec2)
  if t.x ~= nil then v.x = t.x else v.x = 0.0 end
  if t.y ~= nil then v.y = t.y else v.y = 0.0 end
  return v
end

---A thing
---@class Weapon
---@field name string
---@field damage integer # 0..999
---@field rarity Rarity
---@field offset Vec2
---@field tags string[]
---@field notes string?
---@field tint Rgb
---@field owner string # ref<Weapon>, required
---@field type boolean
M.Weapon = {}
M.Weapon.__index = M.Weapon

---@param t table?
---@return Weapon
function M.Weapon.new(t)
  t = t or {}
  local v = setmetatable({}, M.Weapon)
  if t.name ~= nil then v.name = t.name else v.name = "Sword \"x\"" end
  if t.damage ~= nil then v.damage = t.damage else v.damage = 10 end
  if t.rarity ~= nil then v.rarity = t.rarity else v.rarity = "common" end
  if t.offset ~= nil then v.offset = M.Vec2.new(t.offset) else v.offset = M.Vec2.new({ x = 0.5, y = 0.0 }) end
  if t.tags ~= nil then v.tags = t.tags else v.tags = { "a" } end
  v.notes = t.notes
  if t.tint ~= nil then v.tint = bi.Rgb.new(t.tint) else v.tint = bi.Rgb.new({ r = 200, g = 200, b = 200 }) end
  if t.owner ~= nil then v.owner = t.owner else v.owner = "" end
  if t.type ~= nil then v.type = t.type else v.type = false end
  return v
end

M.Weapon.ids = {
  rusty_sword = "rusty_sword",
  dagger = "dagger",
}

return M
"#;

    const SUPPORT_RGB: &str = r#"-- generated by `bi gen struct` from the bi format's builtins — do not edit

local M = {}

---@class Rgb
---@field r integer
---@field g integer
---@field b integer
M.Rgb = {}
M.Rgb.__index = M.Rgb

---@param t table?
---@return Rgb
function M.Rgb.new(t)
  t = t or {}
  local v = setmetatable({}, M.Rgb)
  if t.r ~= nil then v.r = t.r else v.r = 0 end
  if t.g ~= nil then v.g = t.g else v.g = 0 end
  if t.b ~= nil then v.b = t.b else v.b = 0 end
  return v
end

return M
"#;

    #[test]
    fn the_fixture_as_lua() {
        let m = fixture::model(Lang::Lua, true, "");
        let lm = Default::default();
        let text = Lua.unit(&m.units[0], &m, &cx(&lm, Some("gen.scriptableobjects")));
        assert_eq!(text, EXPECTED);
        let support = Lua.support(&m, &cx(&lm, Some("gen.scriptableobjects"))).unwrap();
        assert_eq!(support, SUPPORT_RGB);
        assert!(support.contains("---@class Rgb\n"));
        assert!(!support.contains("---@class Curve\n"));
        // Ref is a string: nothing in the support file for it.
        assert!(!support.contains("Ref"));

        let text = Lua.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("\nlocal bi = require(\"bi_types\")\nlocal M = {}\n"));
    }

    const EXPECTED_DATA: &str = r#"-- generated by `bi gen struct` from level1.bidata — do not edit

local weapons = require("gen.scriptableobjects.weapons")
local bi = require("gen.scriptableobjects.bi_types")
local M = {}

M.Weapon = {}

M.Weapon.rusty_sword = weapons.Weapon.new({ name = "Sword \"x\"", damage = 10, rarity = "common", offset = weapons.Vec2.new({ x = 0.5, y = 0.0 }), tags = { "a" }, notes = nil, tint = bi.Rgb.new({ r = 200, g = 200, b = 200 }), owner = "rusty_sword", type = false })
M.Weapon.dagger = weapons.Weapon.new({ name = "Sword \"x\"", damage = 10, rarity = "common", offset = weapons.Vec2.new({ x = 0.5, y = 0.0 }), tags = { "a" }, notes = nil, tint = bi.Rgb.new({ r = 200, g = 200, b = 200 }), owner = "rusty_sword", type = false })

---Every Weapon of level1.bidata, in its order, with its id.
M.Weapon.all = {
  { id = "rusty_sword", value = M.Weapon.rusty_sword },
  { id = "dagger", value = M.Weapon.dagger },
}

---@param id string
---@return Weapon?
function M.Weapon.find(id)
  for _, entry in ipairs(M.Weapon.all) do
    if entry.id == id then return entry.value end
  end
  return nil
end

return M
"#;

    #[test]
    fn the_fixture_data_as_lua() {
        let m = fixture::model(Lang::Lua, true, "");
        let lm = Default::default();
        let u = &m.units[0];
        assert_eq!(
            Lua.data(&u.data[0], u, &m, &cx(&lm, Some("gen.scriptableobjects"))),
            EXPECTED_DATA
        );
        // The unit is what it was: the data file only requires it.
        assert_eq!(Lua.unit(u, &m, &cx(&lm, Some("gen.scriptableobjects"))), EXPECTED);

        let text = Lua.data(&u.data[0], u, &m, &cx(&lm, None));
        assert!(text.starts_with(
            "-- generated by `bi gen struct` from level1.bidata — do not edit\n\nlocal weapons = require(\"weapons\")\nlocal bi = require(\"bi_types\")\nlocal M = {}\n"
        ));
    }

    #[test]
    fn a_set_field_overrides_the_default_in_data() {
        let data = fixture::DATA.replace(
            "\"$id\": \"dagger\", \"owner\": \"rusty_sword\"",
            "\"$id\": \"dagger\", \"damage\": 7, \"notes\": \"n\", \"tags\": []",
        );
        let m = fixture::model_of(Lang::Lua, fixture::SCHEMA, &[&data], "");
        let lm = Default::default();
        let u = &m.units[0];
        let text = Lua.data(&u.data[0], u, &m, &cx(&lm, None));
        assert!(text.contains(
            "\nM.Weapon.dagger = weapons.Weapon.new({ name = \"Sword \\\"x\\\"\", damage = 7, rarity = \"common\", offset = weapons.Vec2.new({ x = 0.5, y = 0.0 }), tags = {}, notes = \"n\", tint = bi.Rgb.new({ r = 200, g = 200, b = 200 }), owner = \"\", type = false })\n"
        ));
        assert!(text.contains("damage = 7,"));
        assert!(text.contains("notes = \"n\","));
        assert!(text.contains("tags = {},"));
        // The other instance keeps its defaults.
        assert!(text.contains(
            "\nM.Weapon.rusty_sword = weapons.Weapon.new({ name = \"Sword \\\"x\\\"\", damage = 10,"
        ));

        let data = r##"{ "$dialect": "bi/1", "$schema": "weapons.bischema",
          "instances": [ { "$type": "Node", "$id": "root" } ] }"##;
        let m = fixture::model_of(Lang::Lua, fixture::CYCLE, &[data], "");
        let u = &m.units[0];
        let text = Lua.data(&u.data[0], u, &m, &cx(&lm, None));
        assert!(text.contains("\nM.Node.root = weapons.Node.new({ next = nil, kids = {} })\n"));
        assert!(text.contains("  { id = \"root\", value = M.Node.root },\n"));
        assert!(text.contains("---@return Node?\nfunction M.Node.find(id)\n"));
        // No builtin but Ref: no support require.
        assert!(!text.contains("bi_types"));
        assert!(text.contains("\nlocal weapons = require(\"weapons\")\nlocal M = {}\n"));
    }

    #[test]
    fn every_builtin_in_the_support_file() {
        let m = fixture::model_of(Lang::Lua, fixture::EVERY_BUILTIN, &[], "");
        let lm = Default::default();
        let support = Lua.support(&m, &cx(&lm, None)).unwrap();
        for s in [
            "---@class Rgb\n",
            "---@class Rgba\n",
            "---@class CurvePoint\n",
            "---@class Curve\n",
            "---@class GradientStop\n",
            "---@class Gradient\n",
        ] {
            assert!(support.contains(s), "{s}");
        }
        assert_eq!(support.matches("---@class Rgba\n").count(), 1);
        assert!(support.contains("---@field [\"in\"] number\n"));
        assert!(support.ends_with("\nreturn M\n"));
        let text = Lua.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("---@field next string? # ref<Fx>\n"));
        assert!(text.contains("---@field rows number[][]\n"));
        assert!(text.contains(
            "---@field big integer # u64: exact on Lua 5.3+, a double on 5.1 and LuaJIT\n"
        ));
        assert!(text.contains("  v.next = t.next\n"));
        assert!(text.contains(
            "  if t.rows ~= nil then v.rows = t.rows else v.rows = { { 1.0, 2.0 }, {} } end\n"
        ));
        assert!(text.contains(
            "  if t.big ~= nil then v.big = t.big else v.big = 0xffffffffffffffff end\n"
        ));
        assert!(text.contains(
            "  if t.tint ~= nil then v.tint = bi.Rgb.new(t.tint) else v.tint = bi.Rgb.new({ r = 0, g = 0, b = 0 }) end\n"
        ));
        assert!(text.contains(
            "  if t.glow ~= nil then v.glow = bi.Rgba.new(t.glow) else v.glow = bi.Rgba.new({ r = 255, g = 0, b = 0, a = 128 }) end\n"
        ));
        assert!(text.contains(
            "  if t.falloff ~= nil then v.falloff = bi.Curve.new(t.falloff) else v.falloff = bi.Curve.new({ points = { bi.CurvePoint.new({ x = 0.0, y = 0.0, [\"in\"] = 1.0, out = 1.0 }), bi.CurvePoint.new({ x = 1.0, y = 1.0, [\"in\"] = 1.0, out = 1.0 }) } }) end\n"
        ));
        assert!(text.contains(
            "  if t.trail ~= nil then v.trail = bi.Gradient.new(t.trail) else v.trail = bi.Gradient.new({ stops = { bi.GradientStop.new({ t = 0.0, color = bi.Rgba.new({ r = 0, g = 0, b = 0, a = 255 }) }), bi.GradientStop.new({ t = 1.0, color = bi.Rgba.new({ r = 255, g = 255, b = 255, a = 255 }) }) } }) end\n"
        ));
        // Gradient alone still brings Rgba, once.
        let m = fixture::model_of(
            Lang::Lua,
            fixture::EVERY_BUILTIN,
            &[],
            "[lua.types]\nrgba = \"Color\"\n",
        );
        let support = Lua.support(&m, &cx(&lm, None)).unwrap();
        assert_eq!(support.matches("---@class Rgba\n").count(), 1);
        assert!(support.contains("---@class Gradient\n"));
    }

    #[test]
    fn a_reserved_field_is_reached_by_key() {
        let schema = r##"{ "$dialect": "bi/1", "types": {
          "Loop": { "kind": "struct", "fields": [
            { "name": "end", "type": "i32", "default": 3, "doc": "The last frame" },
            { "name": "at", "type": "Loop2" }
          ]},
          "Loop2": { "kind": "struct", "fields": [
            { "name": "end", "type": "bool" }
          ]}}}"##;
        let m = fixture::model_of(Lang::Lua, schema, &[], "");
        let lm = Default::default();
        let text = Lua.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("---@field [\"end\"] integer # The last frame\n"));
        assert!(text.contains(
            "  if t[\"end\"] ~= nil then v[\"end\"] = t[\"end\"] else v[\"end\"] = 3 end\n"
        ));
        assert!(text.contains(
            "  if t.at ~= nil then v.at = M.Loop2.new(t.at) else v.at = M.Loop2.new({ [\"end\"] = false }) end\n"
        ));
        assert!(!text.contains("end_"));
        assert!(!text.contains("bi_types"));
    }

    #[test]
    fn an_external_type_is_spelled_not_generated() {
        let mapping = "[lua.types]\nVec2 = { as = \"glm.vec2\", import = \"local glm = require(\\\"glm\\\")\" }\nrgb = { as = \"Color\", import = \"local Color = require(\\\"color\\\")\", default = \"Color.GRAY\" }\n[lua]\nheader = \"-- hand-written header\"\n";
        let m = fixture::model(Lang::Lua, false, mapping);
        let lm = super::super::super::Mapping::parse(mapping).unwrap().for_lang(Lang::Lua);
        let text = Lua.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.starts_with(
            "-- generated by `bi gen struct` from weapons.bischema — do not edit\n\nlocal Color = require(\"color\")\nlocal glm = require(\"glm\")\n-- hand-written header\nlocal M = {}\n"
        ));
        assert!(!text.contains("---@class Vec2"));
        assert!(text.contains("---@field offset glm.vec2\n"));
        assert!(text.contains("  v.offset = t.offset\n"));
        assert!(text.contains("---@field tint Color\n"));
        assert!(
            text.contains("  if t.tint ~= nil then v.tint = t.tint else v.tint = Color.GRAY end\n")
        );
        // Only Ref is left of the builtins: no support file, no require.
        assert!(Lua.support(&m, &cx(&lm, None)).is_none());
        assert!(!text.contains("bi_types"));
    }

    #[test]
    fn generics_templates_and_cycles() {
        let mapping = "[lua.generics]\nlist = \"List<{T}>\"\nref = \"Handle<{S}>\"\n";
        let m = fixture::model(Lang::Lua, false, mapping);
        let lm = super::super::super::Mapping::parse(mapping).unwrap().for_lang(Lang::Lua);
        let text = Lua.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("---@field tags List<string>\n"));
        assert!(text.contains("---@field owner Handle<Weapon> # ref<Weapon>, required\n"));
        assert!(
            text.contains("  if t.owner ~= nil then v.owner = t.owner else v.owner = \"\" end\n")
        );

        let m = fixture::model_of(Lang::Lua, fixture::CYCLE, &[], "");
        let lm = Default::default();
        let text = Lua.unit(&m.units[0], &m, &cx(&lm, None));
        assert!(text.contains("---@field next Node?\n"));
        assert!(text.contains("---@field kids Node[]\n"));
        assert!(text.contains("  if t.next ~= nil then v.next = M.Node.new(t.next) end\n"));
        assert!(text.contains("  if t.kids ~= nil then v.kids = t.kids else v.kids = {} end\n"));
        assert!(Lua.support(&m, &cx(&lm, None)).is_none());
        assert!(!text.contains("bi_types"));
    }
}
