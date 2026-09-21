//! A `.bischema`: types, their fields, and what every type expression
//! means — its default, whether a value encodes it, how the ex line
//! spells one. See `docs/specs/bi-format.md`.

use serde_json::{Map, Number, Value};

use super::{Diagnostic, check_dialect, is_identifier, json_eq};

pub const PRIMITIVES: [&str; 6] = ["bool", "i32", "i64", "f32", "f64", "string"];
pub const GENERICS: [&str; 3] = ["list", "optional", "ref"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExpr {
    Bool,
    I32,
    I64,
    F32,
    F64,
    Str,
    /// A key of `types`: a struct or an enum.
    Named(String),
    List(Box<TypeExpr>),
    Optional(Box<TypeExpr>),
    Ref(String),
}

impl TypeExpr {
    /// The grammar of the format, whitespace refused. Names are not
    /// checked against the schema here — `Schema::parse` does that once
    /// every type is known.
    pub fn parse(text: &str) -> Result<TypeExpr, String> {
        if text.is_empty() {
            return Err("empty type".into());
        }
        if text.chars().any(char::is_whitespace) {
            return Err(format!("type {text:?} has whitespace in it"));
        }
        if let Some(inner) = text.strip_suffix('>') {
            let Some((generic, arg)) = inner.split_once('<') else {
                return Err(format!("type {text:?} does not parse"));
            };
            return match generic {
                "list" => Ok(TypeExpr::List(Box::new(TypeExpr::parse(arg)?))),
                "optional" => match TypeExpr::parse(arg)? {
                    TypeExpr::Optional(_) => Err(format!("{text}: optional<optional<T>>")),
                    inner => Ok(TypeExpr::Optional(Box::new(inner))),
                },
                "ref" => match TypeExpr::parse(arg)? {
                    TypeExpr::Named(name) => Ok(TypeExpr::Ref(name)),
                    _ => Err(format!("{text}: ref<T> wants a struct type")),
                },
                other => Err(format!("type {text:?}: not a generic: {other}")),
            };
        }
        Ok(match text {
            "bool" => TypeExpr::Bool,
            "i32" => TypeExpr::I32,
            "i64" => TypeExpr::I64,
            "f32" => TypeExpr::F32,
            "f64" => TypeExpr::F64,
            "string" => TypeExpr::Str,
            name if is_identifier(name) => TypeExpr::Named(name.into()),
            _ => return Err(format!("type {text:?} does not parse")),
        })
    }

    /// The expression as the schema spells it.
    pub fn text(&self) -> String {
        match self {
            TypeExpr::Bool => "bool".into(),
            TypeExpr::I32 => "i32".into(),
            TypeExpr::I64 => "i64".into(),
            TypeExpr::F32 => "f32".into(),
            TypeExpr::F64 => "f64".into(),
            TypeExpr::Str => "string".into(),
            TypeExpr::Named(n) => n.clone(),
            TypeExpr::List(t) => format!("list<{}>", t.text()),
            TypeExpr::Optional(t) => format!("optional<{}>", t.text()),
            TypeExpr::Ref(n) => format!("ref<{n}>"),
        }
    }

    pub fn is_integer(&self) -> bool {
        matches!(self, TypeExpr::I32 | TypeExpr::I64)
    }

    pub fn is_float(&self) -> bool {
        matches!(self, TypeExpr::F32 | TypeExpr::F64)
    }

    pub fn is_numeric(&self) -> bool {
        self.is_integer() || self.is_float()
    }

    /// Every named type this expression mentions.
    fn names(&self, out: &mut Vec<String>) {
        match self {
            TypeExpr::Named(n) | TypeExpr::Ref(n) => out.push(n.clone()),
            TypeExpr::List(t) | TypeExpr::Optional(t) => t.names(out),
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    pub name: String,
    pub ty: TypeExpr,
    pub default: Option<Value>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeDef {
    Struct { fields: Vec<FieldDef>, doc: Option<String> },
    Enum { values: Vec<String>, doc: Option<String> },
}

impl TypeDef {
    pub fn kind(&self) -> &'static str {
        match self {
            TypeDef::Struct { .. } => "struct",
            TypeDef::Enum { .. } => "enum",
        }
    }

    pub fn doc(&self) -> Option<&str> {
        match self {
            TypeDef::Struct { doc, .. } | TypeDef::Enum { doc, .. } => doc.as_deref(),
        }
    }

    pub fn fields(&self) -> &[FieldDef] {
        match self {
            TypeDef::Struct { fields, .. } => fields,
            TypeDef::Enum { .. } => &[],
        }
    }
}

/// The types of one schema, in the file's order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Schema {
    pub types: Vec<(String, TypeDef)>,
}

impl Schema {
    /// The file's text read into its raw value and the schema it defines,
    /// or every error found. The raw value is what edits change and the
    /// writer writes, so unknown keys survive without being listed.
    pub fn parse(text: &str) -> Result<(Value, Schema), Vec<Diagnostic>> {
        let doc: Value = match serde_json::from_str(text) {
            Ok(doc) => doc,
            Err(e) => return Err(vec![Diagnostic::error(None, format!("invalid JSON: {e}"))]),
        };
        let schema = Schema::from_value(&doc)?;
        Ok((doc, schema))
    }

    /// The schema a raw value defines, or every error in it.
    pub fn from_value(doc: &Value) -> Result<Schema, Vec<Diagnostic>> {
        let mut errors = Vec::new();
        if let Err(d) = check_dialect(doc) {
            errors.push(Diagnostic::error(None, d.message()));
        }
        let Some(types) = doc.get("types").and_then(Value::as_object) else {
            errors.push(Diagnostic::error(None, "no types object"));
            return Err(errors);
        };
        let mut schema = Schema::default();
        for (name, def) in types {
            let at = format!("type:{name}");
            if !is_identifier(name) {
                errors.push(Diagnostic::error(
                    Some(&at),
                    format!("type name {name:?} is not an identifier"),
                ));
            } else if PRIMITIVES.contains(&name.as_str()) || GENERICS.contains(&name.as_str()) {
                errors
                    .push(Diagnostic::error(Some(&at), format!("type {name} shadows a built-in")));
            }
            match parse_typedef(name, def, &at, &mut errors) {
                Some(def) => schema.types.push((name.clone(), def)),
                None => continue,
            }
        }
        schema.check_types(&mut errors);
        if errors.is_empty() { Ok(schema) } else { Err(errors) }
    }

    /// The second pass: every name known, refs to structs, no embedding
    /// cycles, every default encodable, min/max/step where numbers are.
    fn check_types(&self, errors: &mut Vec<Diagnostic>) {
        for (name, def) in &self.types {
            let TypeDef::Struct { fields, .. } = def else { continue };
            for field in fields {
                let at = format!("type:{name}/field:{}", field.name);
                let mut names = Vec::new();
                field.ty.names(&mut names);
                let mut known = true;
                for n in names {
                    if self.get(&n).is_none() {
                        errors.push(Diagnostic::error(
                            Some(&at),
                            format!("{name}.{}: unknown type {n}", field.name),
                        ));
                        known = false;
                    }
                }
                if !known {
                    continue;
                }
                if let Some(target) = ref_target(&field.ty)
                    && !matches!(self.get(target), Some(TypeDef::Struct { .. }))
                {
                    errors.push(Diagnostic::error(
                        Some(&at),
                        format!("{name}.{}: ref<{target}> wants a struct", field.name),
                    ));
                }
                if let TypeExpr::Named(embedded) = &field.ty
                    && self.embeds(embedded, name, &mut Vec::new())
                {
                    errors.push(Diagnostic::error(
                        Some(&at),
                        format!("{name}.{}: {embedded} embeds {name} back", field.name),
                    ));
                    continue;
                }
                if (field.min.is_some() || field.max.is_some() || field.step.is_some())
                    && !field.ty.is_numeric()
                {
                    errors.push(Diagnostic::error(
                        Some(&at),
                        format!("{name}.{}: min/max/step on a {}", field.name, field.ty.text()),
                    ));
                }
                if let Some(default) = &field.default {
                    let mut out = Vec::new();
                    self.check_value(&field.ty, default, &at, &mut out, None);
                    if let Some(first) = out.into_iter().find(|d| d.level == super::Level::Error) {
                        errors.push(Diagnostic::error(
                            Some(&at),
                            format!("{name}.{}: default {}", field.name, first.message),
                        ));
                    }
                }
            }
        }
    }

    /// Whether struct `from` reaches `target` through plain embedded
    /// fields — the recursion the format refuses.
    fn embeds(&self, from: &str, target: &str, seen: &mut Vec<String>) -> bool {
        if from == target {
            return true;
        }
        if seen.iter().any(|s| s == from) {
            return false;
        }
        seen.push(from.into());
        let Some(TypeDef::Struct { fields, .. }) = self.get(from) else { return false };
        fields.iter().any(|f| match &f.ty {
            TypeExpr::Named(n) => self.embeds(n, target, seen),
            _ => false,
        })
    }

    pub fn get(&self, name: &str) -> Option<&TypeDef> {
        self.types.iter().find(|(n, _)| n == name).map(|(_, d)| d)
    }

    pub fn fields_of(&self, name: &str) -> Option<&[FieldDef]> {
        match self.get(name)? {
            TypeDef::Struct { fields, .. } => Some(fields),
            TypeDef::Enum { .. } => None,
        }
    }

    pub fn field(&self, ty: &str, field: &str) -> Option<&FieldDef> {
        self.fields_of(ty)?.iter().find(|f| f.name == field)
    }

    pub fn enum_values(&self, name: &str) -> Option<&[String]> {
        match self.get(name)? {
            TypeDef::Enum { values, .. } => Some(values),
            TypeDef::Struct { .. } => None,
        }
    }

    pub fn is_struct(&self, name: &str) -> bool {
        matches!(self.get(name), Some(TypeDef::Struct { .. }))
    }

    /// Every struct type name, in order.
    pub fn structs(&self) -> impl Iterator<Item = &str> {
        self.types
            .iter()
            .filter(|(_, d)| matches!(d, TypeDef::Struct { .. }))
            .map(|(n, _)| n.as_str())
    }

    /// The implicit default of a type expression — the format's table.
    pub fn default_of(&self, ty: &TypeExpr) -> Value {
        match ty {
            TypeExpr::Bool => Value::Bool(false),
            TypeExpr::I32 | TypeExpr::I64 | TypeExpr::F32 | TypeExpr::F64 => Value::from(0),
            TypeExpr::Str => Value::String(String::new()),
            TypeExpr::Named(name) => match self.get(name) {
                Some(TypeDef::Enum { values, .. }) => {
                    Value::String(values.first().cloned().unwrap_or_default())
                }
                Some(TypeDef::Struct { fields, .. }) => {
                    let mut map = Map::new();
                    for f in fields {
                        map.insert(f.name.clone(), self.field_default(f));
                    }
                    Value::Object(map)
                }
                None => Value::Null,
            },
            TypeExpr::List(_) => Value::Array(Vec::new()),
            TypeExpr::Optional(_) => Value::Null,
            // Required: an empty id, flagged until set.
            TypeExpr::Ref(_) => Value::String(String::new()),
        }
    }

    /// A field's default: its own, filled from the struct's where it is
    /// a partial object, else the type's implicit one.
    pub fn field_default(&self, field: &FieldDef) -> Value {
        match &field.default {
            Some(d) => self.resolve(&field.ty, d),
            None => self.default_of(&field.ty),
        }
    }

    /// A stored value with every omitted struct key filled from its
    /// default, all the way down — what the value means.
    pub fn resolve(&self, ty: &TypeExpr, value: &Value) -> Value {
        match (ty, value) {
            (TypeExpr::Named(name), Value::Object(map)) => match self.get(name) {
                Some(TypeDef::Struct { fields, .. }) => {
                    let mut out = Map::new();
                    for f in fields {
                        let v = match map.get(&f.name) {
                            Some(v) => self.resolve(&f.ty, v),
                            None => self.field_default(f),
                        };
                        out.insert(f.name.clone(), v);
                    }
                    for (k, v) in map {
                        if !out.contains_key(k) {
                            out.insert(k.clone(), v.clone());
                        }
                    }
                    Value::Object(out)
                }
                _ => value.clone(),
            },
            (TypeExpr::List(inner), Value::Array(items)) => {
                Value::Array(items.iter().map(|v| self.resolve(inner, v)).collect())
            }
            (TypeExpr::Optional(inner), v) if !v.is_null() => self.resolve(inner, v),
            _ => value.clone(),
        }
    }

    /// A resolved value with every struct key equal to its default taken
    /// out again — sparse storage. An object that empties goes with it.
    /// Lists are kept whole.
    pub fn sparse(&self, ty: &TypeExpr, value: &Value) -> Value {
        match (ty, value) {
            (TypeExpr::Named(name), Value::Object(map)) => match self.get(name) {
                Some(TypeDef::Struct { fields, .. }) => {
                    let mut out = Map::new();
                    for (k, v) in map {
                        match fields.iter().find(|f| &f.name == k) {
                            Some(f) => {
                                if !json_eq(&self.resolve(&f.ty, v), &self.field_default(f)) {
                                    out.insert(k.clone(), self.sparse(&f.ty, v));
                                }
                            }
                            None => {
                                out.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    Value::Object(out)
                }
                _ => value.clone(),
            },
            (TypeExpr::Optional(inner), v) if !v.is_null() => self.sparse(inner, v),
            _ => value.clone(),
        }
    }

    /// Whether `value` encodes `ty`, and what is off about it when it
    /// does not: errors for a value of the wrong shape, warnings for a
    /// number out of range, an unknown nested key, a dangling ref (when
    /// `refs` is given to ask). `at` is the row key of the value.
    pub fn check_value(
        &self,
        ty: &TypeExpr,
        value: &Value,
        at: &str,
        out: &mut Vec<Diagnostic>,
        refs: Option<&dyn Fn(&str, &str) -> bool>,
    ) {
        let wrong = |out: &mut Vec<Diagnostic>| {
            out.push(Diagnostic::error(
                Some(at),
                format!("{} is not {}", short(value), self.wants(ty)),
            ));
        };
        match ty {
            TypeExpr::Bool => {
                if !value.is_boolean() {
                    wrong(out);
                }
            }
            TypeExpr::I32 | TypeExpr::I64 => match value.as_i64() {
                Some(n) if *ty == TypeExpr::I32 && i32::try_from(n).is_err() => {
                    out.push(Diagnostic::error(Some(at), format!("{n} does not fit an i32")));
                }
                Some(_) => {}
                None => wrong(out),
            },
            TypeExpr::F32 | TypeExpr::F64 => {
                if !value.is_number() {
                    wrong(out);
                }
            }
            TypeExpr::Str => {
                if !value.is_string() {
                    wrong(out);
                }
            }
            TypeExpr::Named(name) => match self.get(name) {
                Some(TypeDef::Enum { values, .. }) => match value.as_str() {
                    Some(v) if values.iter().any(|x| x == v) => {}
                    _ => wrong(out),
                },
                Some(TypeDef::Struct { fields, .. }) => {
                    let Some(map) = value.as_object() else { return wrong(out) };
                    for (k, v) in map {
                        let key = format!("{at}/{k}");
                        match fields.iter().find(|f| &f.name == k) {
                            Some(f) => self.check_field(f, v, &key, out, refs),
                            None => out
                                .push(Diagnostic::warning(Some(&key), format!("unknown key {k}"))),
                        }
                    }
                }
                None => out.push(Diagnostic::error(Some(at), format!("unknown type {name}"))),
            },
            TypeExpr::List(inner) => {
                let Some(items) = value.as_array() else { return wrong(out) };
                for (i, v) in items.iter().enumerate() {
                    self.check_value(inner, v, &format!("{at}/[{i}]"), out, refs);
                }
            }
            TypeExpr::Optional(inner) => {
                if !value.is_null() {
                    self.check_value(inner, value, at, out, refs);
                }
            }
            TypeExpr::Ref(target) => match value.as_str() {
                Some(id) if id.is_empty() => {
                    out.push(Diagnostic::warning(Some(at), format!("ref<{target}> not set")));
                }
                Some(id) => {
                    if let Some(refs) = refs
                        && !refs(target, id)
                    {
                        out.push(Diagnostic::warning(Some(at), format!("no {target} {id}")));
                    }
                }
                None => wrong(out),
            },
        }
    }

    /// `check_value` for a field: the same, then the range.
    pub fn check_field(
        &self,
        field: &FieldDef,
        value: &Value,
        at: &str,
        out: &mut Vec<Diagnostic>,
        refs: Option<&dyn Fn(&str, &str) -> bool>,
    ) {
        let before = out.len();
        self.check_value(&field.ty, value, at, out, refs);
        if out.len() > before {
            return;
        }
        if let Some(n) = value.as_f64() {
            let low = field.min.is_some_and(|min| n < min);
            let high = field.max.is_some_and(|max| n > max);
            if low || high {
                out.push(Diagnostic::warning(
                    Some(at),
                    format!("{} outside {}", short(value), range_text(field)),
                ));
            }
        }
    }

    /// The first error of `check_value`, for a yes-or-no caller.
    pub fn check(&self, ty: &TypeExpr, value: &Value) -> Result<(), String> {
        let mut out = Vec::new();
        self.check_value(ty, value, "", &mut out, None);
        match out.into_iter().find(|d| d.level == super::Level::Error) {
            Some(d) => Err(d.message),
            None => Ok(()),
        }
    }

    /// What a type wants, for a refusal.
    pub fn wants(&self, ty: &TypeExpr) -> String {
        match ty {
            TypeExpr::Bool => "true or false".into(),
            TypeExpr::I32 | TypeExpr::I64 => "a whole number".into(),
            TypeExpr::F32 | TypeExpr::F64 => "a number".into(),
            TypeExpr::Str => "a string".into(),
            TypeExpr::Named(name) => match self.get(name) {
                Some(TypeDef::Enum { values, .. }) => format!("one of {}", values.join(", ")),
                _ => format!("a {name} as JSON"),
            },
            TypeExpr::List(inner) => format!("a list<{}> as JSON", inner.text()),
            TypeExpr::Optional(inner) => format!("null or {}", self.wants(inner)),
            TypeExpr::Ref(target) => format!("the id of a {target}"),
        }
    }

    /// A value typed on the ex line, read by the type: `true`, a number,
    /// a string with or without quotes, an enum value or an id by name,
    /// `null`/`none`/`-` for an absent optional, JSON for a list or a
    /// struct.
    pub fn parse_value(&self, ty: &TypeExpr, text: &str) -> Result<Value, String> {
        let text = text.trim();
        let refuse = || Err(format!("wants {}", self.wants(ty)));
        let value = match ty {
            TypeExpr::Bool => match text {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => return refuse(),
            },
            TypeExpr::I32 | TypeExpr::I64 => match text.parse::<i64>() {
                Ok(n) => Value::from(n),
                Err(_) => return refuse(),
            },
            TypeExpr::F32 | TypeExpr::F64 => match text.parse::<f64>() {
                Ok(n) if n.is_finite() => number(n),
                _ => return refuse(),
            },
            TypeExpr::Str => Value::String(unquote(text)),
            TypeExpr::Named(name) => match self.get(name) {
                Some(TypeDef::Enum { .. }) => Value::String(unquote(text)),
                _ => match serde_json::from_str::<Value>(text) {
                    Ok(v) if v.is_object() => v,
                    _ => return refuse(),
                },
            },
            TypeExpr::List(_) => match serde_json::from_str::<Value>(text) {
                Ok(v) if v.is_array() => v,
                _ => return refuse(),
            },
            TypeExpr::Optional(inner) => match text {
                "null" | "none" | "-" | "—" => Value::Null,
                _ => self.parse_value(inner, text)?,
            },
            TypeExpr::Ref(_) => {
                let id = unquote(text);
                if !id.is_empty() && !is_identifier(&id) {
                    return Err(format!("{id:?} is not an identifier"));
                }
                Value::String(id)
            }
        };
        self.check(ty, &value).map_err(|_| format!("wants {}", self.wants(ty)))?;
        Ok(value)
    }
}

/// `ref<T>`'s `T`, through an optional or a list.
fn ref_target(ty: &TypeExpr) -> Option<&str> {
    match ty {
        TypeExpr::Ref(t) => Some(t),
        TypeExpr::List(inner) | TypeExpr::Optional(inner) => ref_target(inner),
        _ => None,
    }
}

fn parse_typedef(
    name: &str,
    def: &Value,
    at: &str,
    errors: &mut Vec<Diagnostic>,
) -> Option<TypeDef> {
    let Some(map) = def.as_object() else {
        errors.push(Diagnostic::error(Some(at), format!("type {name} is not an object")));
        return None;
    };
    let doc = map.get("doc").and_then(Value::as_str).map(str::to_string);
    match map.get("kind").and_then(Value::as_str) {
        Some("struct") => {
            let Some(items) = map.get("fields").and_then(Value::as_array) else {
                errors.push(Diagnostic::error(
                    Some(at),
                    format!("struct {name} has no fields array"),
                ));
                return None;
            };
            let mut fields: Vec<FieldDef> = Vec::new();
            for (i, item) in items.iter().enumerate() {
                let Some(f) = item.as_object() else {
                    errors.push(Diagnostic::error(
                        Some(at),
                        format!("{name}.fields[{i}] is not an object"),
                    ));
                    continue;
                };
                let Some(fname) = f.get("name").and_then(Value::as_str) else {
                    errors.push(Diagnostic::error(
                        Some(at),
                        format!("{name}.fields[{i}] has no name"),
                    ));
                    continue;
                };
                let fat = format!("{at}/field:{fname}");
                if fname.starts_with('$') {
                    errors.push(Diagnostic::error(
                        Some(&fat),
                        format!("{name}.{fname}: $ names are reserved"),
                    ));
                } else if !is_identifier(fname) {
                    errors.push(Diagnostic::error(
                        Some(&fat),
                        format!("{name}.{fname:?} is not an identifier"),
                    ));
                }
                if fields.iter().any(|x| x.name == fname) {
                    errors.push(Diagnostic::error(
                        Some(&fat),
                        format!("{name}.{fname} is defined twice"),
                    ));
                }
                let ty = match f.get("type").and_then(Value::as_str) {
                    Some(t) => match TypeExpr::parse(t) {
                        Ok(ty) => ty,
                        Err(e) => {
                            errors.push(Diagnostic::error(
                                Some(&fat),
                                format!("{name}.{fname}: {e}"),
                            ));
                            continue;
                        }
                    },
                    None => {
                        errors.push(Diagnostic::error(
                            Some(&fat),
                            format!("{name}.{fname} has no type"),
                        ));
                        continue;
                    }
                };
                let mut num = |key: &str| -> Option<f64> {
                    match f.get(key) {
                        Some(v) => match v.as_f64() {
                            Some(n) => Some(n),
                            None => {
                                errors.push(Diagnostic::error(
                                    Some(&fat),
                                    format!("{name}.{fname}: {key} is not a number"),
                                ));
                                None
                            }
                        },
                        None => None,
                    }
                };
                let (min, max, step) = (num("min"), num("max"), num("step"));
                fields.push(FieldDef {
                    name: fname.into(),
                    ty,
                    default: f.get("default").cloned(),
                    min,
                    max,
                    step,
                    doc: f.get("doc").and_then(Value::as_str).map(str::to_string),
                });
            }
            Some(TypeDef::Struct { fields, doc })
        }
        Some("enum") => {
            let Some(items) = map.get("values").and_then(Value::as_array) else {
                errors
                    .push(Diagnostic::error(Some(at), format!("enum {name} has no values array")));
                return None;
            };
            let mut values: Vec<String> = Vec::new();
            for item in items {
                let Some(v) = item.as_str() else {
                    errors.push(Diagnostic::error(
                        Some(at),
                        format!("enum {name}: {item} is not a string"),
                    ));
                    continue;
                };
                if values.iter().any(|x| x == v) {
                    errors.push(Diagnostic::error(Some(at), format!("enum {name}: {v} twice")));
                }
                values.push(v.into());
            }
            if values.is_empty() {
                errors.push(Diagnostic::error(Some(at), format!("enum {name} has no values")));
            }
            Some(TypeDef::Enum { values, doc })
        }
        Some(other) => {
            errors
                .push(Diagnostic::error(Some(at), format!("type {name}: unknown kind {other:?}")));
            None
        }
        None => {
            errors.push(Diagnostic::error(Some(at), format!("type {name} has no kind")));
            None
        }
    }
}

/// `min..max` as a field's row shows it: `0..999`, `1..`, `..10`.
pub fn range_text(field: &FieldDef) -> String {
    match (field.min, field.max) {
        (None, None) => String::new(),
        (min, max) => format!(
            "{}..{}",
            min.map(compact).unwrap_or_default(),
            max.map(compact).unwrap_or_default()
        ),
    }
}

/// `20` for `20.0`, `0.1` for `0.1`.
pub fn compact(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { n.to_string() }
}

/// A JSON number from a float, whole when it is whole so `3` stays `3`.
pub fn number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        Value::from(n as i64)
    } else {
        Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null)
    }
}

/// A quoted JSON string unescaped, or the text as typed.
pub fn unquote(text: &str) -> String {
    if text.len() >= 2
        && text.starts_with('"')
        && text.ends_with('"')
        && let Ok(Value::String(s)) = serde_json::from_str::<Value>(text)
    {
        return s;
    }
    text.to_string()
}

/// A value for a message: whole when short, elided when not.
pub fn short(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 30 {
        let head: String = text.chars().take(27).collect();
        format!("{head}…")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::SCHEMA;
    use super::*;

    fn schema() -> Schema {
        Schema::parse(SCHEMA).unwrap().1
    }

    #[test]
    fn type_expressions_parse_and_refuse() {
        assert_eq!(TypeExpr::parse("i32"), Ok(TypeExpr::I32));
        assert_eq!(TypeExpr::parse("string"), Ok(TypeExpr::Str));
        assert_eq!(TypeExpr::parse("Weapon"), Ok(TypeExpr::Named("Weapon".into())));
        assert_eq!(
            TypeExpr::parse("list<ref<Weapon>>"),
            Ok(TypeExpr::List(Box::new(TypeExpr::Ref("Weapon".into()))))
        );
        assert_eq!(
            TypeExpr::parse("optional<list<i32>>"),
            Ok(TypeExpr::Optional(Box::new(TypeExpr::List(Box::new(TypeExpr::I32)))))
        );
        assert!(TypeExpr::parse("list< i32 >").is_err());
        assert!(TypeExpr::parse("set<i32>").is_err());
        assert!(TypeExpr::parse("ref<i32>").is_err());
        assert!(TypeExpr::parse("optional<optional<i32>>").is_err());
        assert!(TypeExpr::parse("").is_err());
        assert!(TypeExpr::parse("list<").is_err());
        assert_eq!(TypeExpr::parse("list<list<f32>>").unwrap().text(), "list<list<f32>>");
    }

    #[test]
    fn the_example_schema_parses_to_four_types() {
        let s = schema();
        let names: Vec<&str> = s.types.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Rarity", "Vec2", "Weapon", "Enemy"]);
        assert_eq!(s.enum_values("Rarity").unwrap(), ["common", "rare", "epic"]);
        let damage = s.field("Weapon", "damage").unwrap();
        assert_eq!(damage.ty, TypeExpr::I32);
        assert_eq!(damage.default, Some(serde_json::json!(10)));
        assert_eq!((damage.min, damage.max), (Some(0.0), Some(999.0)));
        assert_eq!(
            s.get("Rarity").unwrap().doc(),
            Some("Drop tier, drives colour and loot tables")
        );
    }

    fn errors_of(text: &str) -> Vec<String> {
        Schema::parse(text).unwrap_err().into_iter().map(|d| d.message).collect()
    }

    #[test]
    fn every_schema_error_is_named() {
        assert_eq!(
            errors_of("{"),
            ["invalid JSON: EOF while parsing an object at line 1 column 1"]
        );
        assert_eq!(errors_of(r#"{"types":{}}"#), ["no $dialect (want \"bi/1\")"]);
        assert_eq!(errors_of(r#"{"$dialect":"bi/1"}"#), ["no types object"]);
        let one = |types: &str| errors_of(&format!(r#"{{"$dialect":"bi/1","types":{types}}}"#));
        assert_eq!(
            one(r#"{"1a":{"kind":"enum","values":["x"]}}"#),
            ["type name \"1a\" is not an identifier"]
        );
        assert_eq!(
            one(r#"{"list":{"kind":"enum","values":["x"]}}"#),
            ["type list shadows a built-in"]
        );
        assert_eq!(one(r#"{"A":{"kind":"union"}}"#), ["type A: unknown kind \"union\""]);
        assert_eq!(one(r#"{"A":{"kind":"struct"}}"#), ["struct A has no fields array"]);
        assert_eq!(one(r#"{"A":{"kind":"enum","values":[]}}"#), ["enum A has no values"]);
        assert_eq!(one(r#"{"A":{"kind":"enum","values":["x","x"]}}"#), ["enum A: x twice"]);
        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"$x","type":"i32"}]}}"#),
            ["A.$x: $ names are reserved"]
        );
        assert_eq!(
            one(
                r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"i32"},{"name":"x","type":"i32"}]}}"#
            ),
            ["A.x is defined twice"]
        );
        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"i32 "}]}}"#),
            ["A.x: type \"i32 \" has whitespace in it"]
        );
        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"B"}]}}"#),
            ["A.x: unknown type B"]
        );
        assert_eq!(
            one(
                r#"{"E":{"kind":"enum","values":["a"]},"A":{"kind":"struct","fields":[{"name":"x","type":"ref<E>"}]}}"#
            ),
            ["A.x: ref<E> wants a struct"]
        );
        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"A"}]}}"#),
            ["A.x: A embeds A back"]
        );
        assert_eq!(
            one(
                r#"{"A":{"kind":"struct","fields":[{"name":"b","type":"B"}]},"B":{"kind":"struct","fields":[{"name":"a","type":"A"}]}}"#
            ),
            ["A.b: B embeds A back", "B.a: A embeds B back"]
        );
        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"i32","default":"ten"}]}}"#),
            ["A.x: default \"ten\" is not a whole number"]
        );
        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"string","min":0}]}}"#),
            ["A.x: min/max/step on a string"]
        );
        assert!(
            Schema::parse(r#"{"$dialect":"bi/1","types":{"A":{"kind":"struct","fields":[{"name":"s","type":"optional<A>"},{"name":"l","type":"list<A>"}]}}}"#).is_ok(),
            "recursion through optional and list is fine"
        );
    }

    #[test]
    fn defaults_follow_the_table_and_partials_fill_in() {
        let s = schema();
        assert_eq!(s.default_of(&TypeExpr::Bool), serde_json::json!(false));
        assert_eq!(s.default_of(&TypeExpr::F32), serde_json::json!(0));
        assert_eq!(s.default_of(&TypeExpr::Str), serde_json::json!(""));
        assert_eq!(s.default_of(&TypeExpr::Named("Rarity".into())), serde_json::json!("common"));
        assert_eq!(
            s.default_of(&TypeExpr::Named("Vec2".into())),
            serde_json::json!({"x": 0, "y": 0})
        );
        assert_eq!(s.default_of(&TypeExpr::parse("list<i32>").unwrap()), serde_json::json!([]));
        assert_eq!(s.default_of(&TypeExpr::parse("optional<i32>").unwrap()), Value::Null);
        assert_eq!(s.default_of(&TypeExpr::Ref("Weapon".into())), serde_json::json!(""));
        let offset = s.field("Weapon", "offset").unwrap();
        assert_eq!(s.field_default(offset), serde_json::json!({"x": 0.5, "y": 0}));
        let whole = s.default_of(&TypeExpr::Named("Weapon".into()));
        assert_eq!(whole["offset"], serde_json::json!({"x": 0.5, "y": 0}));
        assert_eq!(whole["tags"], serde_json::json!([]));
        assert_eq!(
            s.resolve(&TypeExpr::Named("Vec2".into()), &serde_json::json!({"y": 3})),
            serde_json::json!({"x": 0, "y": 3})
        );
        assert_eq!(
            s.sparse(&TypeExpr::Named("Vec2".into()), &serde_json::json!({"x": 0, "y": 3})),
            serde_json::json!({"y": 3})
        );
    }

    #[test]
    fn values_are_checked_by_type() {
        let s = schema();
        let ok = |t: &str, v: Value| s.check(&TypeExpr::parse(t).unwrap(), &v).is_ok();
        assert!(ok("i32", serde_json::json!(3)));
        assert!(!ok("i32", serde_json::json!(3.5)));
        assert!(!ok("i32", serde_json::json!("3")));
        assert!(!ok("i32", serde_json::json!(5_000_000_000i64)));
        assert!(ok("i64", serde_json::json!(5_000_000_000i64)));
        assert!(ok("f32", serde_json::json!(3)));
        assert!(ok("Rarity", serde_json::json!("epic")));
        assert!(!ok("Rarity", serde_json::json!("mythic")));
        assert!(ok("Vec2", serde_json::json!({"x": 1})));
        assert!(!ok("Vec2", serde_json::json!({"x": "one"})));
        assert!(ok("list<ref<Weapon>>", serde_json::json!(["a", "b"])));
        assert!(!ok("list<ref<Weapon>>", serde_json::json!(["a", 2])));
        assert!(ok("optional<i32>", Value::Null));
        assert!(!ok("optional<i32>", serde_json::json!(true)));
        assert_eq!(
            s.check(&TypeExpr::I32, &serde_json::json!("x")),
            Err("\"x\" is not a whole number".into())
        );
        let mut out = Vec::new();
        let damage = s.field("Weapon", "damage").unwrap();
        s.check_field(damage, &serde_json::json!(1000), "k", &mut out, None);
        assert_eq!(out[0].message, "1000 outside 0..999");
        assert_eq!(out[0].level, super::super::Level::Warning);
    }

    #[test]
    fn ex_line_values_parse_per_type() {
        let s = schema();
        let p = |t: &str, text: &str| s.parse_value(&TypeExpr::parse(t).unwrap(), text);
        assert_eq!(p("bool", "true"), Ok(serde_json::json!(true)));
        assert_eq!(p("bool", "yes"), Err("wants true or false".into()));
        assert_eq!(p("i32", "42"), Ok(serde_json::json!(42)));
        assert_eq!(p("i32", "4.2"), Err("wants a whole number".into()));
        assert_eq!(p("f32", "4.2"), Ok(serde_json::json!(4.2)));
        assert_eq!(p("f32", "4"), Ok(serde_json::json!(4)));
        assert_eq!(p("string", "Rusty Sword"), Ok(serde_json::json!("Rusty Sword")));
        assert_eq!(p("string", "\"a \\\"b\\\"\""), Ok(serde_json::json!("a \"b\"")));
        assert_eq!(p("Rarity", "rare"), Ok(serde_json::json!("rare")));
        assert_eq!(p("Rarity", "mythic"), Err("wants one of common, rare, epic".into()));
        assert_eq!(p("Vec2", "{\"x\": 1}"), Ok(serde_json::json!({"x": 1})));
        assert_eq!(p("Vec2", "1"), Err("wants a Vec2 as JSON".into()));
        assert_eq!(p("list<string>", "[\"a\"]"), Ok(serde_json::json!(["a"])));
        assert_eq!(p("optional<ref<Enemy>>", "none"), Ok(Value::Null));
        assert_eq!(p("optional<ref<Enemy>>", "goblin"), Ok(serde_json::json!("goblin")));
        assert_eq!(p("ref<Weapon>", "not an id"), Err("\"not an id\" is not an identifier".into()));
        assert_eq!(p("ref<Weapon>", ""), Ok(serde_json::json!("")));
    }
}
