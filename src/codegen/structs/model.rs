//! The language-neutral model every backend reads: one `Unit` per
//! schema, its types with names already cased and checked for the
//! target, defaults resolved all the way down, structs in dependency
//! order with the edge that closes an `optional` cycle marked boxed, the
//! ids the data files supply, and which of the format's builtins the
//! support file has to carry. Built once, read by one backend, so a
//! collision or a cycle is found in one place and the same way for all
//! six languages. See `docs/specs/gen-struct.md`.

use serde_json::Value;

use super::mapping::{External, LangMapping};
use super::{Lang, lit, names};
use crate::props::Diagnostic;
use crate::props::data::DataFile;
use crate::props::schema::{FieldDef, IntKind, Schema, TypeDef, TypeExpr};

/// The format's builtins the support file provides, in its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Builtin {
    Rgb,
    Rgba,
    Curve,
    Gradient,
    Ref,
}

impl Builtin {
    pub const ALL: [Builtin; 5] =
        [Builtin::Rgb, Builtin::Rgba, Builtin::Curve, Builtin::Gradient, Builtin::Ref];

    /// The mapping key that replaces it.
    pub fn key(self) -> &'static str {
        match self {
            Builtin::Rgb => "rgb",
            Builtin::Rgba => "rgba",
            Builtin::Curve => "curve",
            Builtin::Gradient => "gradient",
            Builtin::Ref => "ref",
        }
    }
}

/// A field's type, with the schema's names kept as wire names and the
/// one thing the schema does not say: whether an optional is boxed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Bool,
    Int(IntKind),
    F32,
    F64,
    Str,
    Rgb,
    Rgba,
    Curve,
    Gradient,
    /// The wire name of a schema type — `Model::type_named` finds it.
    Named(String),
    List(Box<Ty>),
    /// `true` when this optional closes a cycle and the language needs
    /// a box to size the struct.
    Optional(Box<Ty>, bool),
    /// The wire name of the target struct.
    Ref(String),
}

impl Ty {
    fn of(ty: &TypeExpr) -> Ty {
        match ty {
            TypeExpr::Bool => Ty::Bool,
            TypeExpr::Int(k) => Ty::Int(*k),
            TypeExpr::F32 => Ty::F32,
            TypeExpr::F64 => Ty::F64,
            TypeExpr::Str => Ty::Str,
            TypeExpr::Rgb => Ty::Rgb,
            TypeExpr::Rgba => Ty::Rgba,
            TypeExpr::Curve => Ty::Curve,
            TypeExpr::Gradient => Ty::Gradient,
            TypeExpr::Named(n) => Ty::Named(n.clone()),
            TypeExpr::List(inner) => Ty::List(Box::new(Ty::of(inner))),
            TypeExpr::Optional(inner) => Ty::Optional(Box::new(Ty::of(inner)), false),
            TypeExpr::Ref(t) => Ty::Ref(t.clone()),
        }
    }

    /// The builtin this type is, if it is one; `Ref` for a ref.
    pub fn builtin(&self) -> Option<Builtin> {
        match self {
            Ty::Rgb => Some(Builtin::Rgb),
            Ty::Rgba => Some(Builtin::Rgba),
            Ty::Curve => Some(Builtin::Curve),
            Ty::Gradient => Some(Builtin::Gradient),
            Ty::Ref(_) => Some(Builtin::Ref),
            _ => None,
        }
    }

    /// Every builtin this type mentions, itself and through generics.
    pub fn builtins(&self, out: &mut Vec<Builtin>) {
        if let Some(b) = self.builtin()
            && !out.contains(&b)
        {
            out.push(b);
        }
        match self {
            Ty::List(inner) | Ty::Optional(inner, _) => inner.builtins(out),
            _ => {}
        }
    }

    /// The struct this type needs complete, when it is one: itself, or
    /// itself through optionals. A list is a pointer, a ref a string.
    fn needs_complete(&self) -> Option<&str> {
        match self {
            Ty::Named(n) => Some(n),
            Ty::Optional(inner, _) => inner.needs_complete(),
            _ => None,
        }
    }

    /// Marks the outermost optional on the way to `Named` boxed.
    fn box_it(&mut self) {
        if let Ty::Optional(_, boxed) = self {
            *boxed = true;
        }
    }

    /// The wire names of every schema type this type mentions.
    pub fn named(&self, out: &mut Vec<String>) {
        match self {
            Ty::Named(n) | Ty::Ref(n) => {
                if !out.contains(n) {
                    out.push(n.clone());
                }
            }
            Ty::List(inner) | Ty::Optional(inner, _) => inner.named(out),
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumValue {
    pub wire: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Id {
    pub wire: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub wire: String,
    pub name: String,
    pub ty: Ty,
    /// Fully resolved: a partial struct default filled from the struct's
    /// own, an optional's `null`, a ref's empty id.
    pub default: Value,
    pub doc: Option<String>,
    /// `0..999`, `0..1 step 0.01` — for the doc comment.
    pub range: Option<String>,
    /// A `ref` with no default: the empty id until set.
    pub required_ref: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    Struct { fields: Vec<Field>, ids: Vec<Id> },
    Enum { values: Vec<EnumValue> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    pub wire: String,
    /// The code name — or the external spelling, when `external` is set.
    pub name: String,
    pub doc: Option<String>,
    pub kind: TypeKind,
    /// Set when the mapping says the project supplies this type: it is
    /// not generated, only spelled.
    pub external: Option<External>,
}

impl Type {
    pub fn fields(&self) -> &[Field] {
        match &self.kind {
            TypeKind::Struct { fields, .. } => fields,
            TypeKind::Enum { .. } => &[],
        }
    }

    pub fn values(&self) -> &[EnumValue] {
        match &self.kind {
            TypeKind::Enum { values } => values,
            TypeKind::Struct { .. } => &[],
        }
    }

    pub fn ids(&self) -> &[Id] {
        match &self.kind {
            TypeKind::Struct { ids, .. } => ids,
            TypeKind::Enum { .. } => &[],
        }
    }

    pub fn is_struct(&self) -> bool {
        matches!(self.kind, TypeKind::Struct { .. })
    }

    pub fn is_enum(&self) -> bool {
        matches!(self.kind, TypeKind::Enum { .. })
    }
}

/// One instance of a data file, fully resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    /// The `$id` as stored.
    pub wire: String,
    /// The cased id (`names::instance_name`), escaped; the backend puts
    /// the type and the file's stem around it.
    pub name: String,
    /// The wire name of its struct type.
    pub ty: String,
    /// Every field, sparse keys filled from defaults, colours canonical
    /// (`Schema::resolve`); unknown keys dropped.
    pub value: Value,
}

/// One data file: one output file of instances.
#[derive(Debug, Clone, PartialEq)]
pub struct DataUnit {
    /// `level1` — the output file's stem.
    pub stem: String,
    /// `level1.bidata` — for the generated-by line.
    pub source: String,
    /// In file order.
    pub instances: Vec<Instance>,
}

impl DataUnit {
    /// The instances of one type, in file order.
    pub fn of<'a>(&'a self, type_wire: &'a str) -> impl Iterator<Item = &'a Instance> {
        self.instances.iter().filter(move |i| i.ty == type_wire)
    }

    /// The wire names of the types that have instances here, in order
    /// of first appearance.
    pub fn types(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for i in &self.instances {
            if !out.contains(&i.ty.as_str()) {
                out.push(&i.ty);
            }
        }
        out
    }
}

/// One schema: one output file.
#[derive(Debug, Clone, PartialEq)]
pub struct Unit {
    /// `game` — the output file's stem.
    pub stem: String,
    /// `game.bischema` — for the generated-by line.
    pub source: String,
    /// In schema order.
    pub types: Vec<Type>,
    /// Indices into `types`: enums first in schema order, then structs
    /// with every embedded struct before the struct that embeds it.
    pub order: Vec<usize>,
    /// The data files of this schema, one output file each; empty when
    /// the mapping says `instances = false`.
    pub data: Vec<DataUnit>,
}

impl Unit {
    /// The types in the order a language that wants definitions before
    /// use emits them.
    pub fn ordered(&self) -> impl Iterator<Item = &Type> {
        self.order.iter().map(|&i| &self.types[i])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub lang: Lang,
    pub units: Vec<Unit>,
    /// The builtins used by some generated type and replaced by no
    /// mapping, in `Builtin::ALL` order.
    pub builtins: Vec<Builtin>,
}

/// The names the support file takes, per language, that a schema type
/// may not have while that file is generated.
fn support_names(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::C => &[
            "bi_rgb",
            "bi_rgba",
            "bi_curve",
            "bi_curve_point",
            "bi_gradient",
            "bi_gradient_stop",
            "bi_ref",
        ],
        Lang::Cpp => &[],
        Lang::Go | Lang::Rust | Lang::C3 | Lang::Lua => {
            &["Rgb", "Rgba", "Curve", "CurvePoint", "Gradient", "GradientStop", "Ref"]
        }
    }
}

impl Model {
    /// The model for `lang` from every unit — stem, source file name,
    /// the schema and its data files, each as stem, source and file —
    /// under `mapping`; `ids` says whether the data's ids become
    /// constants, `instances` whether the instances do. Errors stop the
    /// run; notes ride along.
    pub fn build(
        lang: Lang,
        units: Vec<(String, String, Schema, Vec<(String, String, DataFile)>)>,
        mapping: &LangMapping,
        ids: bool,
        instances: bool,
    ) -> Result<(Model, Vec<Diagnostic>), Vec<Diagnostic>> {
        let mut errors = Vec::new();
        let mut notes = Vec::new();
        let mut out = Vec::new();
        for (stem, source, schema, data) in &units {
            let flags = Flags { ids, instances };
            match build_unit(lang, stem, source, schema, data, mapping, flags, &mut notes) {
                Ok(unit) => out.push(unit),
                Err(mut e) => errors.append(&mut e),
            }
        }
        // Type names across units: one namespace.
        let mut all: Vec<(String, String)> = Vec::new();
        for unit in &out {
            for t in &unit.types {
                all.push((format!("{}:{}", unit.source, t.wire), t.name.clone()));
            }
        }
        for c in names::collisions(&all) {
            errors.push(Diagnostic::error(
                None,
                format!("across schemas: {c} — one namespace holds both"),
            ));
        }
        let mut model = Model { lang, units: out, builtins: Vec::new() };
        // Builtins: used by a generated type, replaced by no mapping.
        let mut used = Vec::new();
        for unit in &model.units {
            for t in unit.types.iter().filter(|t| t.external.is_none()) {
                for f in t.fields() {
                    f.ty.builtins(&mut used);
                }
            }
        }
        for b in Builtin::ALL {
            let replaced = match b {
                Builtin::Ref => mapping.generics.reference.is_some(),
                other => mapping.types.contains_key(other.key()),
            };
            if used.contains(&b) && !replaced {
                model.builtins.push(b);
            }
        }
        if !model.builtins.is_empty() {
            for unit in &model.units {
                for t in unit.types.iter().filter(|t| t.external.is_none()) {
                    if support_names(lang).contains(&t.name.as_str()) {
                        errors.push(Diagnostic::error(
                            None,
                            format!(
                                "{}: {}: `{}` is the support file's — [names] \"{}\" = \"…\" picks another",
                                unit.source, t.wire, t.name, t.wire
                            ),
                        ));
                    }
                }
            }
        }
        // A mapping entry naming nothing: a warning, the file serves every schema.
        for key in mapping.types.keys() {
            let is_builtin = Builtin::ALL.iter().any(|b| b.key() == key);
            if !is_builtin && model.type_named(key).is_none() {
                notes.push(Diagnostic::warning(
                    None,
                    format!("mapping: no schema has a type named `{key}`"),
                ));
            }
        }
        if errors.is_empty() { Ok((model, notes)) } else { Err(errors) }
    }

    pub fn type_named(&self, wire: &str) -> Option<&Type> {
        self.units.iter().flat_map(|u| u.types.iter()).find(|t| t.wire == wire)
    }

    /// The code spelling of a schema type: its cased name, or the
    /// external one.
    pub fn spelling<'a>(&'a self, wire: &'a str) -> &'a str {
        self.type_named(wire).map_or(wire, |t| t.name.as_str())
    }

    /// The builtins among `self.builtins` that `unit`'s generated types use.
    pub fn builtins_in(&self, unit: &Unit) -> Vec<Builtin> {
        let mut used = Vec::new();
        for t in unit.types.iter().filter(|t| t.external.is_none()) {
            for f in t.fields() {
                f.ty.builtins(&mut used);
            }
        }
        self.builtins.iter().copied().filter(|b| used.contains(b)).collect()
    }

    /// Whether `unit` uses any generated builtin — whether it imports
    /// the support file.
    pub fn uses_support(&self, unit: &Unit) -> bool {
        !self.builtins_in(unit).is_empty()
    }
}

#[derive(Clone, Copy)]
struct Flags {
    ids: bool,
    instances: bool,
}

#[allow(clippy::too_many_arguments)]
fn build_unit(
    lang: Lang,
    stem: &str,
    source: &str,
    schema: &Schema,
    data: &[(String, String, DataFile)],
    mapping: &LangMapping,
    flags: Flags,
    notes: &mut Vec<Diagnostic>,
) -> Result<Unit, Vec<Diagnostic>> {
    let mut errors = Vec::new();
    let mut types = Vec::new();
    if schema.types.is_empty() {
        notes.push(Diagnostic::warning(None, format!("{source} has no types")));
    }
    let at = |what: &str, msg: &str| Diagnostic::error(None, format!("{source}: {what}: {msg}"));
    for (wire, def) in &schema.types {
        let external = mapping.types.get(wire).cloned();
        let name = match (&external, mapping.renamed(wire)) {
            (Some(ext), _) => ext.spelling.clone(),
            (None, Some(renamed)) => match names::escape(lang, renamed) {
                Ok(n) => n,
                Err(e) => {
                    errors.push(at(wire, &e));
                    renamed.to_string()
                }
            },
            (None, None) => match names::escape(lang, &names::type_name(lang, wire)) {
                Ok(n) => n,
                Err(e) => {
                    errors.push(at(wire, &e));
                    wire.clone()
                }
            },
        };
        let kind = match def {
            TypeDef::Enum { values, .. } => {
                let mut out = Vec::new();
                for v in values {
                    let key = format!("{wire}.{v}");
                    let code: String = match mapping.renamed(&key) {
                        Some(r) => r.to_string(),
                        None => names::enum_value_name(lang, wire, v),
                    };
                    let code = match names::escape(lang, &code) {
                        Ok(c) => c,
                        Err(e) => {
                            errors.push(at(&key, &e));
                            code
                        }
                    };
                    out.push(EnumValue { wire: v.clone(), name: code });
                }
                let pairs: Vec<(String, String)> =
                    out.iter().map(|v| (v.wire.clone(), v.name.clone())).collect();
                for c in names::collisions(&pairs) {
                    errors.push(at(
                        wire,
                        &format!("{c} — [names] \"{wire}.<value>\" = \"…\" picks another"),
                    ));
                }
                TypeKind::Enum { values: out }
            }
            TypeDef::Struct { fields, .. } => {
                let mut out = Vec::new();
                for f in fields {
                    out.push(build_field(
                        lang,
                        wire,
                        f,
                        schema,
                        mapping,
                        source,
                        &mut errors,
                        notes,
                    ));
                }
                let pairs: Vec<(String, String)> =
                    out.iter().map(|f| (f.wire.clone(), f.name.clone())).collect();
                for c in names::collisions(&pairs) {
                    errors.push(at(
                        wire,
                        &format!("{c} — [names] \"{wire}.<field>\" = \"…\" picks another"),
                    ));
                }
                let mut id_list = Vec::new();
                if flags.ids {
                    for (_, _, df) in data {
                        for inst in df.instances.iter().filter(|i| &i.ty == wire) {
                            let code = names::id_name(lang, wire, &inst.id);
                            let code = match names::escape(lang, &code) {
                                Ok(c) => c,
                                Err(e) => {
                                    errors.push(at(&format!("{wire} {}", inst.id), &e));
                                    code
                                }
                            };
                            id_list.push(Id { wire: inst.id.clone(), name: code });
                        }
                    }
                    let pairs: Vec<(String, String)> =
                        id_list.iter().map(|i| (i.wire.clone(), i.name.clone())).collect();
                    for c in names::collisions(&pairs) {
                        errors.push(at(wire, &format!("ids {c}")));
                    }
                }
                TypeKind::Struct { fields: out, ids: id_list }
            }
        };
        types.push(Type {
            wire: wire.clone(),
            name,
            doc: def.doc().map(str::to_string),
            kind,
            external,
        });
    }
    let pairs: Vec<(String, String)> =
        types.iter().map(|t| (t.wire.clone(), t.name.clone())).collect();
    for c in names::collisions(&pairs) {
        errors.push(at("types", &format!("{c} — [names] \"<Type>\" = \"…\" picks another")));
    }
    let order = order_and_box(&mut types);
    let mut data_units = Vec::new();
    if flags.instances {
        for (dstem, dsource, df) in data {
            let mut instances = Vec::new();
            for inst in &df.instances {
                let Some(t) = types.iter().find(|t| t.wire == inst.ty) else { continue };
                if t.external.is_some() {
                    notes.push(Diagnostic::warning(
                        None,
                        format!(
                            "{dsource}: {} {}: {} is external, its instances are not generated",
                            inst.ty, inst.id, inst.ty
                        ),
                    ));
                    continue;
                }
                let name = match names::escape(lang, &names::instance_name(lang, &inst.id)) {
                    Ok(n) => n,
                    Err(e) => {
                        errors.push(Diagnostic::error(
                            None,
                            format!("{dsource}: {} {}: {e}", inst.ty, inst.id),
                        ));
                        inst.id.clone()
                    }
                };
                // Only the schema's keys, resolved; unknown keys are
                // dropped, the validator has already named them.
                let ty = TypeExpr::Named(inst.ty.clone());
                let mut known = serde_json::Map::new();
                if let Some(fields) = schema.fields_of(&inst.ty) {
                    for f in fields {
                        if let Some(v) = inst.values.get(&f.name) {
                            known.insert(f.name.clone(), v.clone());
                        }
                    }
                }
                let value = schema.resolve(&ty, &Value::Object(known));
                instances.push(Instance {
                    wire: inst.id.clone(),
                    name,
                    ty: inst.ty.clone(),
                    value,
                });
            }
            for t in types.iter().filter(|t| t.is_struct()) {
                let pairs: Vec<(String, String)> = instances
                    .iter()
                    .filter(|i| i.ty == t.wire)
                    .map(|i| (i.wire.clone(), i.name.clone()))
                    .collect();
                for c in names::collisions(&pairs) {
                    errors.push(Diagnostic::error(
                        None,
                        format!("{dsource}: {}: {c} — rename one instance", t.wire),
                    ));
                }
            }
            data_units.push(DataUnit { stem: dstem.clone(), source: dsource.clone(), instances });
        }
    }
    if errors.is_empty() {
        Ok(Unit {
            stem: stem.to_string(),
            source: source.to_string(),
            types,
            order,
            data: data_units,
        })
    } else {
        Err(errors)
    }
}

#[allow(clippy::too_many_arguments)]
fn build_field(
    lang: Lang,
    type_wire: &str,
    f: &FieldDef,
    schema: &Schema,
    mapping: &LangMapping,
    source: &str,
    errors: &mut Vec<Diagnostic>,
    notes: &mut Vec<Diagnostic>,
) -> Field {
    let key = format!("{type_wire}.{}", f.name);
    if let Some(e) = names::owned(lang, &f.name) {
        errors.push(Diagnostic::error(None, format!("{source}: {key}: {e}")));
    }
    let code: String = match mapping.renamed(&key) {
        Some(r) => r.to_string(),
        None => names::field_name(lang, &f.name),
    };
    let code = match names::escape(lang, &code) {
        Ok(c) => c,
        Err(e) => {
            errors.push(Diagnostic::error(None, format!("{source}: {key}: {e}")));
            code
        }
    };
    let ty = Ty::of(&f.ty);
    // An external type cannot carry the schema's default.
    if f.default.is_some() {
        let mut mentioned = Vec::new();
        ty.named(&mut mentioned);
        let ext_named = mentioned.iter().find(|n| mapping.types.contains_key(n.as_str()));
        let mut builtins = Vec::new();
        ty.builtins(&mut builtins);
        let ext_builtin = builtins
            .iter()
            .find(|b| **b != Builtin::Ref && mapping.types.contains_key(b.key()))
            .map(|b| b.key().to_string());
        if let Some(ext) = ext_named.cloned().or(ext_builtin)
            && mapping.types.get(&ext).is_some_and(|e| e.default.is_none())
        {
            notes.push(Diagnostic::warning(
                None,
                format!(
                    "{source}: {key}: {ext} is external, its default is the language's — default = \"…\" on the mapping entry says otherwise"
                ),
            ));
        }
    }
    Field {
        wire: f.name.clone(),
        name: code,
        required_ref: matches!(ty, Ty::Ref(_)) && f.default.is_none(),
        ty,
        default: schema.field_default(f),
        doc: f.doc.clone(),
        range: lit::range(f.min, f.max, f.step),
    }
}

/// Enums first, then structs with every struct they need complete
/// before them; an `optional` edge that would close a cycle is boxed
/// instead of followed.
fn order_and_box(types: &mut [Type]) -> Vec<usize> {
    let index = |wire: &str, types: &[Type]| types.iter().position(|t| t.wire == wire);
    let mut order: Vec<usize> = (0..types.len()).filter(|&i| types[i].is_enum()).collect();
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        Open,
        Done,
    }
    let mut marks = vec![Mark::New; types.len()];
    fn visit(i: usize, types: &mut [Type], marks: &mut Vec<Mark>, order: &mut Vec<usize>) {
        marks[i] = Mark::Open;
        let deps: Vec<(usize, Option<usize>)> = types[i]
            .fields()
            .iter()
            .enumerate()
            .filter_map(|(fi, f)| {
                let target = f.ty.needs_complete()?;
                let ti = types.iter().position(|t| t.wire == target)?;
                Some((fi, Some(ti)))
            })
            .map(|(fi, ti)| (fi, ti))
            .collect();
        for (fi, ti) in deps {
            let Some(ti) = ti else { continue };
            if !types[ti].is_struct() {
                continue;
            }
            match marks[ti] {
                Mark::Done => {}
                Mark::Open => {
                    // The edge that closes a cycle: box it, do not follow it.
                    if let TypeKind::Struct { fields, .. } = &mut types[i].kind {
                        fields[fi].ty.box_it();
                    }
                }
                Mark::New => visit(ti, types, marks, order),
            }
        }
        marks[i] = Mark::Done;
        order.push(i);
    }
    for i in 0..types.len() {
        if types[i].is_struct() && marks[i] == Mark::New {
            visit(i, types, &mut marks, &mut order);
        }
    }
    let _ = index;
    order
}

#[cfg(test)]
mod tests {
    use super::super::fixture;
    use super::*;

    #[test]
    fn names_are_cased_for_the_language() {
        let m = fixture::model(Lang::Go, false, "");
        let w = m.type_named("Weapon").unwrap();
        let f: Vec<&str> = w.fields().iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            f,
            ["Name", "Damage", "Rarity", "Offset", "Tags", "Notes", "Tint", "Owner", "Type"]
        );
        let m = fixture::model(Lang::Rust, false, "");
        assert_eq!(m.type_named("Weapon").unwrap().fields()[8].name, "r#type");
        let r = m.type_named("Rarity").unwrap();
        assert_eq!(r.values()[1], EnumValue { wire: "very rare".into(), name: "VeryRare".into() });
        let m = fixture::model(Lang::C, false, "");
        assert_eq!(m.type_named("Weapon").unwrap().fields()[8].name, "type", "not a C keyword");
        assert_eq!(
            fixture::model(Lang::Go, false, "").type_named("Weapon").unwrap().fields()[8].name,
            "Type"
        );
        assert_eq!(m.type_named("Rarity").unwrap().values()[1].name, "RARITY_VERY_RARE");
    }

    #[test]
    fn renames_from_the_mapping_win() {
        let m = fixture::model(
            Lang::Rust,
            false,
            "[names]\n\"Weapon.type\" = \"kind\"\n\"Rarity.very rare\" = \"Epic\"\n[rust.names]\n\"Weapon\" = \"Arm\"\n",
        );
        let w = m.type_named("Weapon").unwrap();
        assert_eq!(w.name, "Arm");
        assert_eq!(w.fields()[8].name, "kind");
        assert_eq!(m.type_named("Rarity").unwrap().values()[1].name, "Epic");
    }

    #[test]
    fn defaults_are_resolved_and_ranges_spelled() {
        let m = fixture::model(Lang::Rust, false, "");
        let w = m.type_named("Weapon").unwrap();
        assert_eq!(w.fields()[3].default, serde_json::json!({"x": 0.5, "y": 0}));
        assert_eq!(w.fields()[1].range.as_deref(), Some("0..999"));
        assert!(w.fields()[7].required_ref);
        assert_eq!(w.fields()[7].default, serde_json::json!(""));
        assert_eq!(w.fields()[5].default, serde_json::Value::Null);
        assert_eq!(w.fields()[2].default, serde_json::json!("common"));
    }

    #[test]
    fn ids_come_from_the_data_in_order() {
        let m = fixture::model(Lang::C, true, "");
        let ids: Vec<&str> =
            m.type_named("Weapon").unwrap().ids().iter().map(|i| i.name.as_str()).collect();
        assert_eq!(ids, ["WEAPON_RUSTY_SWORD", "WEAPON_DAGGER"]);
        assert!(
            fixture::model(Lang::C, true, "ids = false")
                .type_named("Weapon")
                .unwrap()
                .ids()
                .is_empty()
        );
        assert!(fixture::model(Lang::C, false, "").type_named("Weapon").unwrap().ids().is_empty());
    }

    #[test]
    fn instances_are_resolved_and_named() {
        let m = fixture::model(Lang::Rust, true, "");
        let d = &m.units[0].data;
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].stem.as_str(), d[0].source.as_str()), ("level1", "level1.bidata"));
        let names: Vec<(&str, &str)> =
            d[0].instances.iter().map(|i| (i.ty.as_str(), i.name.as_str())).collect();
        assert_eq!(names, [("Weapon", "rusty_sword"), ("Weapon", "dagger")]);
        let v = &d[0].of("Weapon").next().unwrap().value;
        assert_eq!(v["owner"], serde_json::json!("rusty_sword"));
        assert_eq!(v["damage"], serde_json::json!(10), "sparse: filled from the default");
        assert_eq!(v["offset"], serde_json::json!({"x": 0.5, "y": 0}));
        assert_eq!(d[0].types(), ["Weapon"]);
        assert_eq!(
            fixture::model(Lang::Go, true, "").units[0].data[0].instances[0].name,
            "RustySword"
        );
        assert!(fixture::model(Lang::Rust, true, "instances = false").units[0].data.is_empty());
        assert!(fixture::model(Lang::Rust, false, "").units[0].data.is_empty());
        let (m, notes) = fixture::model_notes(
            Lang::Rust,
            fixture::SCHEMA,
            &[fixture::DATA],
            "[rust.types]\nWeapon = \"W\"\n",
        );
        assert!(m.units[0].data[0].instances.is_empty());
        assert!(
            notes
                .iter()
                .any(|n| n.message.contains("Weapon is external, its instances are not generated"))
        );
    }

    #[test]
    fn order_puts_vec2_before_weapon_and_boxes_the_optional_cycle() {
        let m = fixture::model(Lang::C, false, "");
        let u = &m.units[0];
        let names: Vec<&str> = u.ordered().map(|t| t.wire.as_str()).collect();
        assert_eq!(names, ["Rarity", "Vec2", "Weapon"]);
        let m = fixture::model_of(Lang::Rust, fixture::CYCLE, &[], "");
        let n = m.type_named("Node").unwrap();
        assert!(matches!(n.fields()[0].ty, Ty::Optional(_, true)));
        assert!(matches!(n.fields()[1].ty, Ty::List(_)));

        // Vec2 declared after Weapon still comes first.
        let s = r#"{"$dialect":"bi/1","types":{
          "W":{"kind":"struct","fields":[{"name":"o","type":"optional<V>"}]},
          "V":{"kind":"struct","fields":[{"name":"x","type":"f32"}]}}}"#;
        let m = fixture::model_of(Lang::Cpp, s, &[], "");
        let names: Vec<&str> = m.units[0].ordered().map(|t| t.wire.as_str()).collect();
        assert_eq!(names, ["V", "W"]);
        assert!(matches!(m.type_named("W").unwrap().fields()[0].ty, Ty::Optional(_, false)));
    }

    #[test]
    fn builtins_are_only_the_used_unmapped_ones() {
        assert_eq!(
            fixture::model(Lang::Rust, false, "").builtins,
            vec![Builtin::Rgb, Builtin::Ref]
        );
        assert_eq!(
            fixture::model(Lang::Rust, false, "[rust.types]\nrgb = \"X\"\n").builtins,
            vec![Builtin::Ref]
        );
        assert_eq!(
            fixture::model(Lang::Rust, false, "[rust.generics]\nref = \"H<{S}>\"\n").builtins,
            vec![Builtin::Rgb]
        );
        let m = fixture::model(Lang::Rust, false, "[rust.types]\nVec2 = \"glam::Vec2\"\n");
        assert_eq!(m.type_named("Vec2").unwrap().external.as_ref().unwrap().spelling, "glam::Vec2");
        assert_eq!(m.spelling("Vec2"), "glam::Vec2");
        assert_eq!(m.spelling("Weapon"), "Weapon");
        assert!(m.uses_support(&m.units[0]));
    }

    #[test]
    fn collisions_are_refused() {
        let s = r#"{"$dialect":"bi/1","types":{"S":{"kind":"struct","fields":[{"name":"foo_bar","type":"bool"},{"name":"fooBar","type":"bool"}]}}}"#;
        let e = fixture::try_model(Lang::Go, s, &[], "").unwrap_err();
        assert_eq!(
            e[0].message,
            "weapons.bischema: S: `foo_bar` and `fooBar` both become `FooBar` — [names] \"S.<field>\" = \"…\" picks another"
        );
        assert!(fixture::try_model(Lang::C, s, &[], "").is_ok());
        let s = r#"{"$dialect":"bi/1","types":{"E":{"kind":"enum","values":["rare","Rare"]}}}"#;
        assert!(fixture::try_model(Lang::Rust, s, &[], "").is_err());
        let s = r#"{"$dialect":"bi/1","types":{"Rgb":{"kind":"struct","fields":[{"name":"c","type":"rgb"}]}}}"#;
        let e = fixture::try_model(Lang::Rust, s, &[], "").unwrap_err();
        assert!(e[0].message.contains("`Rgb` is the support file's"), "{}", e[0].message);
        assert!(
            fixture::try_model(Lang::Rust, s, &[], "[rust.types]\nrgb = \"X\"\n").is_ok(),
            "rgb mapped away: no support file, no clash"
        );
    }

    #[test]
    fn reserved_names_are_escaped_or_refused() {
        let s = r#"{"$dialect":"bi/1","types":{"S":{"kind":"struct","fields":[{"name":"_","type":"bool"}]}}}"#;
        let e = fixture::try_model(Lang::Go, s, &[], "").unwrap_err();
        assert!(e[0].message.contains("`_` is reserved in go"), "{}", e[0].message);
        assert!(fixture::try_model(Lang::Rust, s, &[], "").is_ok());
    }

    #[test]
    fn external_defaults_are_noted() {
        let (_, notes) = fixture::model_notes(
            Lang::Rust,
            fixture::SCHEMA,
            &[],
            "[rust.types]\nVec2 = \"glam::Vec2\"\n",
        );
        assert_eq!(
            notes[0].message,
            "weapons.bischema: Weapon.offset: Vec2 is external, its default is the language's — default = \"…\" on the mapping entry says otherwise"
        );
        let (_, notes) = fixture::model_notes(
            Lang::Rust,
            fixture::SCHEMA,
            &[],
            "[rust.types]\nVec2 = { as = \"glam::Vec2\", default = \"glam::Vec2::ZERO\" }\nNope = \"X\"\n",
        );
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].message, "mapping: no schema has a type named `Nope`");
    }
}
