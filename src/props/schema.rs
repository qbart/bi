//! A `.bischema`: types, their fields, and what every type expression
//! means — its default, whether a value encodes it, how the ex line
//! spells one — plus the layout a field asks for in the data view. See
//! `docs/specs/bi-format.md`.

use serde_json::{Map, Number, Value};

use super::{Diagnostic, check_dialect, is_identifier, json_eq};

pub const PRIMITIVES: [&str; 15] = [
    "bool", "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64", "string", "rgb",
    "rgba", "curve",
];
pub const GENERICS: [&str; 3] = ["list", "optional", "ref"];

/// The integer widths, signed and unsigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntKind {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl IntKind {
    pub fn text(self) -> &'static str {
        match self {
            IntKind::I8 => "i8",
            IntKind::I16 => "i16",
            IntKind::I32 => "i32",
            IntKind::I64 => "i64",
            IntKind::U8 => "u8",
            IntKind::U16 => "u16",
            IntKind::U32 => "u32",
            IntKind::U64 => "u64",
        }
    }

    pub fn parse(text: &str) -> Option<IntKind> {
        Some(match text {
            "i8" => IntKind::I8,
            "i16" => IntKind::I16,
            "i32" => IntKind::I32,
            "i64" => IntKind::I64,
            "u8" => IntKind::U8,
            "u16" => IntKind::U16,
            "u32" => IntKind::U32,
            "u64" => IntKind::U64,
            _ => return None,
        })
    }

    pub fn range(self) -> (i128, i128) {
        match self {
            IntKind::I8 => (i8::MIN as i128, i8::MAX as i128),
            IntKind::I16 => (i16::MIN as i128, i16::MAX as i128),
            IntKind::I32 => (i32::MIN as i128, i32::MAX as i128),
            IntKind::I64 => (i64::MIN as i128, i64::MAX as i128),
            IntKind::U8 => (0, u8::MAX as i128),
            IntKind::U16 => (0, u16::MAX as i128),
            IntKind::U32 => (0, u32::MAX as i128),
            IntKind::U64 => (0, u64::MAX as i128),
        }
    }

    pub fn fits(self, n: i128) -> bool {
        let (lo, hi) = self.range();
        (lo..=hi).contains(&n)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExpr {
    Bool,
    Int(IntKind),
    F32,
    F64,
    Str,
    /// `#rrggbb`, `#rrggbbaa`.
    Rgb,
    Rgba,
    /// The curve editor's list of points, in JSON.
    Curve,
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
        if let Some(kind) = IntKind::parse(text) {
            return Ok(TypeExpr::Int(kind));
        }
        Ok(match text {
            "bool" => TypeExpr::Bool,
            "f32" => TypeExpr::F32,
            "f64" => TypeExpr::F64,
            "string" => TypeExpr::Str,
            "rgb" => TypeExpr::Rgb,
            "rgba" => TypeExpr::Rgba,
            "curve" => TypeExpr::Curve,
            name if is_identifier(name) => TypeExpr::Named(name.into()),
            _ => return Err(format!("type {text:?} does not parse")),
        })
    }

    /// The expression as the schema spells it.
    pub fn text(&self) -> String {
        match self {
            TypeExpr::Bool => "bool".into(),
            TypeExpr::Int(kind) => kind.text().into(),
            TypeExpr::F32 => "f32".into(),
            TypeExpr::F64 => "f64".into(),
            TypeExpr::Str => "string".into(),
            TypeExpr::Rgb => "rgb".into(),
            TypeExpr::Rgba => "rgba".into(),
            TypeExpr::Curve => "curve".into(),
            TypeExpr::Named(n) => n.clone(),
            TypeExpr::List(t) => format!("list<{}>", t.text()),
            TypeExpr::Optional(t) => format!("optional<{}>", t.text()),
            TypeExpr::Ref(n) => format!("ref<{n}>"),
        }
    }

    pub fn is_integer(&self) -> bool {
        matches!(self, TypeExpr::Int(_))
    }

    pub fn is_float(&self) -> bool {
        matches!(self, TypeExpr::F32 | TypeExpr::F64)
    }

    pub fn is_numeric(&self) -> bool {
        self.is_integer() || self.is_float()
    }

    /// Every named type this expression mentions.
    pub fn names(&self, out: &mut Vec<String>) {
        match self {
            TypeExpr::Named(n) | TypeExpr::Ref(n) => out.push(n.clone()),
            TypeExpr::List(t) | TypeExpr::Optional(t) => t.names(out),
            _ => {}
        }
    }

    /// The same expression with type `old` called `new`.
    pub fn renamed(&self, old: &str, new: &str) -> TypeExpr {
        match self {
            TypeExpr::Named(n) if n == old => TypeExpr::Named(new.into()),
            TypeExpr::Ref(n) if n == old => TypeExpr::Ref(new.into()),
            TypeExpr::List(t) => TypeExpr::List(Box::new(t.renamed(old, new))),
            TypeExpr::Optional(t) => TypeExpr::Optional(Box::new(t.renamed(old, new))),
            other => other.clone(),
        }
    }
}

/// How a field asks to be drawn, beyond what its type implies. See
/// `docs/specs/props.md` §Layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Widget {
    /// An enum as every value in a row, the current one marked.
    Toggle,
    /// A struct always open, with no fold of its own.
    Inline,
}

impl Widget {
    pub const NAMES: [&str; 2] = ["toggle", "inline"];

    pub fn parse(text: &str) -> Option<Widget> {
        match text {
            "toggle" => Some(Widget::Toggle),
            "inline" => Some(Widget::Inline),
            _ => None,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Widget::Toggle => "toggle",
            Widget::Inline => "inline",
        }
    }
}

/// `show_if` / `hide_if`: a test on a sibling field's value.
#[derive(Debug, Clone, PartialEq)]
pub struct Cond {
    pub field: String,
    pub op: CondOp,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Cond {
    /// `field`, `!field`, `field == value`, `field != value`, `field < n`,
    /// `field <= n`, `field > n`, `field >= n`. A value is `true`, `false`,
    /// a number, or a string with or without quotes.
    pub fn parse(text: &str) -> Result<Cond, String> {
        let text = text.trim();
        if let Some(field) = text.strip_prefix('!') {
            let field = field.trim();
            if !is_identifier(field) {
                return Err(format!("{text:?}: not a field name"));
            }
            return Ok(Cond { field: field.into(), op: CondOp::Eq, value: Value::Bool(false) });
        }
        for (spelling, op) in [
            ("==", CondOp::Eq),
            ("!=", CondOp::Ne),
            ("<=", CondOp::Le),
            (">=", CondOp::Ge),
            ("<", CondOp::Lt),
            (">", CondOp::Gt),
        ] {
            if let Some((field, value)) = text.split_once(spelling) {
                let field = field.trim();
                if !is_identifier(field) {
                    return Err(format!("{text:?}: not a field name before {spelling}"));
                }
                let value = value.trim();
                let value = match value {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    v => match v.parse::<f64>() {
                        Ok(n) if n.is_finite() => number(n),
                        _ => Value::String(unquote(v)),
                    },
                };
                if matches!(op, CondOp::Lt | CondOp::Le | CondOp::Gt | CondOp::Ge)
                    && !value.is_number()
                {
                    return Err(format!("{text:?}: {spelling} wants a number"));
                }
                return Ok(Cond { field: field.into(), op, value });
            }
        }
        if !is_identifier(text) {
            return Err(format!("{text:?}: not a condition"));
        }
        Ok(Cond { field: text.into(), op: CondOp::Eq, value: Value::Bool(true) })
    }

    pub fn text(&self) -> String {
        let value = match &self.value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        match (self.op, &self.value) {
            (CondOp::Eq, Value::Bool(true)) => self.field.clone(),
            (CondOp::Eq, Value::Bool(false)) => format!("!{}", self.field),
            (op, _) => format!("{} {} {value}", self.field, op_text(op)),
        }
    }

    /// The test against the sibling values, `siblings` resolved.
    pub fn holds(&self, siblings: &Map<String, Value>) -> bool {
        let Some(actual) = siblings.get(&self.field) else { return false };
        match self.op {
            CondOp::Eq => json_eq(actual, &self.value),
            CondOp::Ne => !json_eq(actual, &self.value),
            op => match (actual.as_f64(), self.value.as_f64()) {
                (Some(a), Some(b)) => match op {
                    CondOp::Lt => a < b,
                    CondOp::Le => a <= b,
                    CondOp::Gt => a > b,
                    _ => a >= b,
                },
                _ => false,
            },
        }
    }
}

fn op_text(op: CondOp) -> &'static str {
    match op {
        CondOp::Eq => "==",
        CondOp::Ne => "!=",
        CondOp::Lt => "<",
        CondOp::Le => "<=",
        CondOp::Gt => ">",
        CondOp::Ge => ">=",
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
    // ---- layout ----
    /// `Stats`, or nested `Stats/Combat`: the section the field sits in.
    pub group: Option<String>,
    /// Display order among siblings; the array order breaks ties.
    pub order: Option<f64>,
    /// What the row says instead of the name.
    pub label: Option<String>,
    pub readonly: bool,
    pub show_if: Option<Cond>,
    pub hide_if: Option<Cond>,
    pub widget: Option<Widget>,
}

impl FieldDef {
    /// The row's label: `label`, else the name.
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }

    /// Whether the field shows beside `siblings`, resolved.
    pub fn shown(&self, siblings: &Map<String, Value>) -> bool {
        if let Some(cond) = &self.show_if
            && !cond.holds(siblings)
        {
            return false;
        }
        if let Some(cond) = &self.hide_if
            && cond.holds(siblings)
        {
            return false;
        }
        true
    }
}

/// The attributes a field may carry, in the order the schema view lists
/// them; `name` last, since it is the field's identity rather than a
/// setting.
pub const FIELD_ATTRS: [&str; 14] = [
    "type", "default", "min", "max", "step", "doc", "group", "order", "label", "readonly",
    "show_if", "hide_if", "widget", "name",
];

/// A group's options, from the struct's `groups` map.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroupDef {
    pub collapsed: bool,
    pub doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeDef {
    Struct { fields: Vec<FieldDef>, doc: Option<String>, groups: Vec<(String, GroupDef)> },
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

    pub fn group(&self, name: &str) -> Option<&GroupDef> {
        match self {
            TypeDef::Struct { groups, .. } => {
                groups.iter().find(|(n, _)| n == name).map(|(_, g)| g)
            }
            TypeDef::Enum { .. } => None,
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
    /// cycles, every default encodable, min/max/step where numbers are,
    /// conditions naming siblings, widgets on the types they fit.
    fn check_types(&self, errors: &mut Vec<Diagnostic>) {
        for (name, def) in &self.types {
            let TypeDef::Struct { fields, .. } = def else { continue };
            for field in fields {
                let at = format!("type:{name}/field:{}", field.name);
                let refuse = |errors: &mut Vec<Diagnostic>, what: String| {
                    errors.push(Diagnostic::error(
                        Some(&at),
                        format!("{name}.{}: {what}", field.name),
                    ));
                };
                let mut names = Vec::new();
                field.ty.names(&mut names);
                let mut known = true;
                for n in names {
                    if self.get(&n).is_none() {
                        refuse(errors, format!("unknown type {n}"));
                        known = false;
                    }
                }
                if !known {
                    continue;
                }
                if let Some(target) = ref_target(&field.ty)
                    && !matches!(self.get(target), Some(TypeDef::Struct { .. }))
                {
                    refuse(errors, format!("ref<{target}> wants a struct"));
                }
                if let TypeExpr::Named(embedded) = &field.ty
                    && self.embeds(embedded, name, &mut Vec::new())
                {
                    refuse(errors, format!("{embedded} embeds {name} back"));
                    continue;
                }
                if (field.min.is_some() || field.max.is_some() || field.step.is_some())
                    && !field.ty.is_numeric()
                {
                    refuse(errors, format!("min/max/step on a {}", field.ty.text()));
                }
                if let Some(default) = &field.default {
                    let mut out = Vec::new();
                    self.check_value(&field.ty, default, &at, &mut out, None);
                    if let Some(first) = out.into_iter().find(|d| d.level == super::Level::Error) {
                        refuse(errors, format!("default {}", first.message));
                    }
                }
                for (what, cond) in [("show_if", &field.show_if), ("hide_if", &field.hide_if)] {
                    if let Some(cond) = cond
                        && !fields.iter().any(|f| f.name == cond.field)
                    {
                        refuse(errors, format!("{what} names no field {}", cond.field));
                    }
                }
                match field.widget {
                    Some(Widget::Toggle) if self.enum_values(&field.ty.text()).is_none() => {
                        refuse(errors, "widget toggle wants an enum".into());
                    }
                    Some(Widget::Inline) if !self.is_struct(&field.ty.text()) => {
                        refuse(errors, "widget inline wants a struct".into());
                    }
                    _ => {}
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

    /// Every type expression a field could be given, for cycling: the
    /// primitives, every type, and a ref to every struct.
    pub fn type_choices(&self) -> Vec<String> {
        let mut out: Vec<String> = PRIMITIVES.iter().map(|p| p.to_string()).collect();
        out.extend(self.types.iter().map(|(n, _)| n.clone()));
        out.extend(self.structs().map(|s| format!("ref<{s}>")));
        out
    }

    /// The implicit default of a type expression — the format's table.
    pub fn default_of(&self, ty: &TypeExpr) -> Value {
        match ty {
            TypeExpr::Bool => Value::Bool(false),
            TypeExpr::Int(_) | TypeExpr::F32 | TypeExpr::F64 => Value::from(0),
            TypeExpr::Str => Value::String(String::new()),
            TypeExpr::Rgb => Value::String("#000000".into()),
            TypeExpr::Rgba => Value::String("#000000ff".into()),
            TypeExpr::Curve => curve_value(&crate::curve::linear()),
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
            (TypeExpr::Rgb, Value::String(s)) | (TypeExpr::Rgba, Value::String(s)) => {
                canonical_colour(ty, s).unwrap_or_else(|| value.clone())
            }
            _ => value.clone(),
        }
    }

    /// A resolved value with every struct key equal to the struct's own
    /// default taken out again — sparse storage. Lists are kept whole.
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
            (TypeExpr::Rgb, Value::String(s)) | (TypeExpr::Rgba, Value::String(s)) => {
                canonical_colour(ty, s).unwrap_or_else(|| value.clone())
            }
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
            TypeExpr::Int(kind) => {
                let whole =
                    value.as_i64().map(i128::from).or_else(|| value.as_u64().map(i128::from));
                match whole {
                    Some(n) if !kind.fits(n) => out.push(Diagnostic::error(
                        Some(at),
                        format!("{n} does not fit {}", kind.text()),
                    )),
                    Some(_) => {}
                    None => wrong(out),
                }
            }
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
            TypeExpr::Rgb | TypeExpr::Rgba => {
                let alpha = *ty == TypeExpr::Rgba;
                if value.as_str().is_none_or(|s| super::color::parse(s, alpha).is_none()) {
                    wrong(out);
                }
            }
            TypeExpr::Curve => {
                if let Err(e) = curve_of(value) {
                    out.push(Diagnostic::error(Some(at), e));
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
            TypeExpr::Int(kind) => {
                let (lo, hi) = kind.range();
                format!("a whole number {lo}..{hi}")
            }
            TypeExpr::F32 | TypeExpr::F64 => "a number".into(),
            TypeExpr::Str => "a string".into(),
            TypeExpr::Rgb => "a colour as #rrggbb".into(),
            TypeExpr::Rgba => "a colour as #rrggbbaa".into(),
            TypeExpr::Curve => "a curve as JSON: [[x, y, out, in, locked], …]".into(),
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
            TypeExpr::Int(kind) => match text.parse::<i128>() {
                Ok(n) if kind.fits(n) => int_value(n),
                _ => return refuse(),
            },
            TypeExpr::F32 | TypeExpr::F64 => match text.parse::<f64>() {
                Ok(n) if n.is_finite() => number(n),
                _ => return refuse(),
            },
            TypeExpr::Str => Value::String(unquote(text)),
            TypeExpr::Rgb | TypeExpr::Rgba => {
                let alpha = *ty == TypeExpr::Rgba;
                match super::color::parse(&unquote(text), alpha) {
                    Some(c) => Value::String(super::color::text(c, alpha)),
                    None => return refuse(),
                }
            }
            TypeExpr::Curve => match serde_json::from_str::<Value>(text) {
                Ok(v) if v.is_array() => v,
                _ => return refuse(),
            },
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

/// A colour as the editor writes it — lowercase, eight digits for an
/// `rgba` — when the text reads as one.
fn canonical_colour(ty: &TypeExpr, text: &str) -> Option<Value> {
    let alpha = *ty == TypeExpr::Rgba;
    super::color::parse(text, alpha).map(|c| Value::String(super::color::text(c, alpha)))
}

/// A `curve` value as the curve module holds it: every point an array of
/// `x`, `y`, `out`, `in`, `locked`, the tail optional; sorted by `x`
/// inside `0..1`. What is wrong with it, otherwise.
pub fn curve_of(value: &Value) -> Result<crate::curve::Curve, String> {
    let Some(items) = value.as_array() else {
        return Err("a curve is an array of points".into());
    };
    let mut points = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let Some(parts) = item.as_array() else {
            return Err(format!("point [{i}] is not an array"));
        };
        if parts.len() < 2 || parts.len() > 5 {
            return Err(format!("point [{i}] wants 2 to 5 entries: [x, y, out, in, locked]"));
        }
        let num = |n: usize| -> Result<f32, String> {
            match parts.get(n) {
                None => Ok(0.0),
                Some(v) => v
                    .as_f64()
                    .map(|f| f as f32)
                    .ok_or_else(|| format!("point [{i}] entry {n} is not a number")),
            }
        };
        let locked = match parts.get(4) {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(format!("point [{i}] locked is not true or false")),
        };
        let p = crate::curve::Point { x: num(0)?, y: num(1)?, out: num(2)?, in_: num(3)?, locked };
        if !(0.0..=1.0).contains(&p.x) {
            return Err(format!("point [{i}] x {} is outside 0..1", p.x));
        }
        if let Some(prev) = points.last().map(|q: &crate::curve::Point| q.x)
            && p.x < prev
        {
            return Err(format!("point [{i}] x {} is before the point ahead of it", p.x));
        }
        points.push(p);
    }
    Ok(crate::curve::Curve { points })
}

/// The curve as the format writes it.
pub fn curve_value(curve: &crate::curve::Curve) -> Value {
    Value::Array(
        curve
            .points
            .iter()
            .map(|p| {
                Value::Array(vec![
                    number(p.x as f64),
                    number(p.y as f64),
                    number(p.out as f64),
                    number(p.in_ as f64),
                    Value::Bool(p.locked),
                ])
            })
            .collect(),
    )
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
                let mut refuse = |what: String| {
                    errors.push(Diagnostic::error(Some(&fat), format!("{name}.{fname}: {what}")));
                };
                if fname.starts_with('$') {
                    refuse("$ names are reserved".into());
                } else if !is_identifier(fname) {
                    refuse("not an identifier".into());
                }
                if fields.iter().any(|x| x.name == fname) {
                    refuse("defined twice".into());
                }
                let ty = match f.get("type").and_then(Value::as_str) {
                    Some(t) => match TypeExpr::parse(t) {
                        Ok(ty) => ty,
                        Err(e) => {
                            refuse(e);
                            continue;
                        }
                    },
                    None => {
                        refuse("no type".into());
                        continue;
                    }
                };
                let mut num = |key: &str| -> Option<f64> {
                    match f.get(key) {
                        Some(v) => match v.as_f64() {
                            Some(n) => Some(n),
                            None => {
                                refuse(format!("{key} is not a number"));
                                None
                            }
                        },
                        None => None,
                    }
                };
                let (min, max, step, order) = (num("min"), num("max"), num("step"), num("order"));
                let mut text = |key: &str| -> Option<String> {
                    match f.get(key) {
                        Some(Value::String(s)) => Some(s.clone()),
                        Some(_) => {
                            refuse(format!("{key} is not a string"));
                            None
                        }
                        None => None,
                    }
                };
                let (doc, group, label) = (text("doc"), text("group"), text("label"));
                if group
                    .as_deref()
                    .is_some_and(|g| g.is_empty() || g.split('/').any(|p| p.trim().is_empty()))
                {
                    refuse("group is empty".into());
                }
                let mut cond = |key: &str| -> Option<Cond> {
                    match f.get(key) {
                        Some(Value::String(s)) => match Cond::parse(s) {
                            Ok(c) => Some(c),
                            Err(e) => {
                                refuse(format!("{key} {e}"));
                                None
                            }
                        },
                        Some(_) => {
                            refuse(format!("{key} is not a string"));
                            None
                        }
                        None => None,
                    }
                };
                let (show_if, hide_if) = (cond("show_if"), cond("hide_if"));
                let readonly = match f.get("readonly") {
                    Some(Value::Bool(b)) => *b,
                    Some(_) => {
                        refuse("readonly is not true or false".into());
                        false
                    }
                    None => false,
                };
                let widget = match f.get("widget") {
                    Some(Value::String(s)) => match Widget::parse(s) {
                        Some(w) => Some(w),
                        None => {
                            refuse(format!("widget {s:?} (want {})", Widget::NAMES.join(", ")));
                            None
                        }
                    },
                    Some(_) => {
                        refuse("widget is not a string".into());
                        None
                    }
                    None => None,
                };
                fields.push(FieldDef {
                    name: fname.into(),
                    ty,
                    default: f.get("default").cloned(),
                    min,
                    max,
                    step,
                    doc,
                    group,
                    order,
                    label,
                    readonly,
                    show_if,
                    hide_if,
                    widget,
                });
            }
            let mut groups = Vec::new();
            match map.get("groups") {
                None => {}
                Some(Value::Object(defs)) => {
                    for (gname, gdef) in defs {
                        let Some(g) = gdef.as_object() else {
                            errors.push(Diagnostic::error(
                                Some(at),
                                format!("{name}: group {gname} is not an object"),
                            ));
                            continue;
                        };
                        groups.push((
                            gname.clone(),
                            GroupDef {
                                collapsed: g
                                    .get("collapsed")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                                doc: g.get("doc").and_then(Value::as_str).map(str::to_string),
                            },
                        ));
                    }
                }
                Some(_) => errors
                    .push(Diagnostic::error(Some(at), format!("{name}: groups is not an object"))),
            }
            Some(TypeDef::Struct { fields, doc, groups })
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

/// A JSON number from an integer of any width the format has.
pub fn int_value(n: i128) -> Value {
    if let Ok(n) = i64::try_from(n) {
        Value::from(n)
    } else if let Ok(n) = u64::try_from(n) {
        Value::from(n)
    } else {
        Value::Null
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
        assert_eq!(TypeExpr::parse("i32"), Ok(TypeExpr::Int(IntKind::I32)));
        assert_eq!(TypeExpr::parse("u8"), Ok(TypeExpr::Int(IntKind::U8)));
        assert_eq!(TypeExpr::parse("u64"), Ok(TypeExpr::Int(IntKind::U64)));
        assert_eq!(TypeExpr::parse("string"), Ok(TypeExpr::Str));
        assert_eq!(TypeExpr::parse("Weapon"), Ok(TypeExpr::Named("Weapon".into())));
        assert_eq!(
            TypeExpr::parse("list<ref<Weapon>>"),
            Ok(TypeExpr::List(Box::new(TypeExpr::Ref("Weapon".into()))))
        );
        assert_eq!(
            TypeExpr::parse("optional<list<i16>>"),
            Ok(TypeExpr::Optional(Box::new(TypeExpr::List(Box::new(TypeExpr::Int(IntKind::I16))))))
        );
        assert!(TypeExpr::parse("list< i32 >").is_err());
        assert!(TypeExpr::parse("set<i32>").is_err());
        assert!(TypeExpr::parse("ref<i32>").is_err());
        assert!(TypeExpr::parse("optional<optional<i32>>").is_err());
        assert!(TypeExpr::parse("").is_err());
        assert!(TypeExpr::parse("list<").is_err());
        assert_eq!(TypeExpr::parse("list<list<f32>>").unwrap().text(), "list<list<f32>>");
        assert_eq!(
            TypeExpr::parse("list<ref<Weapon>>").unwrap().renamed("Weapon", "Arm").text(),
            "list<ref<Arm>>"
        );
    }

    #[test]
    fn the_example_schema_parses_to_four_types() {
        let s = schema();
        let names: Vec<&str> = s.types.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Rarity", "Vec2", "Weapon", "Enemy"]);
        assert_eq!(s.enum_values("Rarity").unwrap(), ["common", "rare", "epic"]);
        let damage = s.field("Weapon", "damage").unwrap();
        assert_eq!(damage.ty, TypeExpr::Int(IntKind::I32));
        assert_eq!(damage.default, Some(serde_json::json!(10)));
        assert_eq!((damage.min, damage.max), (Some(0.0), Some(999.0)));
        assert_eq!(
            s.get("Rarity").unwrap().doc(),
            Some("Drop tier, drives colour and loot tables")
        );
        assert!(s.type_choices().contains(&"ref<Weapon>".to_string()));
        assert!(s.type_choices().contains(&"u16".to_string()));
    }

    fn errors_of(text: &str) -> Vec<String> {
        Schema::parse(text).unwrap_err().into_iter().map(|d| d.message).collect()
    }

    fn one(types: &str) -> Vec<String> {
        errors_of(&format!(r#"{{"$dialect":"bi/1","types":{types}}}"#))
    }

    #[test]
    fn every_schema_error_is_named() {
        assert_eq!(
            errors_of("{"),
            ["invalid JSON: EOF while parsing an object at line 1 column 1"]
        );
        assert_eq!(errors_of(r#"{"types":{}}"#), ["no $dialect (want \"bi/1\")"]);
        assert_eq!(errors_of(r#"{"$dialect":"bi/1"}"#), ["no types object"]);
        assert_eq!(
            one(r#"{"1a":{"kind":"enum","values":["x"]}}"#),
            ["type name \"1a\" is not an identifier"]
        );
        assert_eq!(
            one(r#"{"list":{"kind":"enum","values":["x"]}}"#),
            ["type list shadows a built-in"]
        );
        assert_eq!(one(r#"{"u8":{"kind":"enum","values":["x"]}}"#), ["type u8 shadows a built-in"]);
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
            ["A.x: defined twice"]
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
            ["A.x: default \"ten\" is not a whole number -2147483648..2147483647"]
        );
        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"u8","default":300}]}}"#),
            ["A.x: default 300 does not fit u8"]
        );
        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"string","min":0}]}}"#),
            ["A.x: min/max/step on a string"]
        );
        assert!(
            Schema::parse(
                r#"{"$dialect":"bi/1","types":{"A":{"kind":"struct","fields":[{"name":"s","type":"optional<A>"},{"name":"l","type":"list<A>"}]}}}"#
            )
            .is_ok(),
            "recursion through optional and list is fine"
        );
    }

    #[test]
    fn layout_attributes_parse_and_are_checked() {
        let text = r#"{"$dialect":"bi/1","types":{
            "Rarity":{"kind":"enum","values":["common","rare"]},
            "Vec2":{"kind":"struct","fields":[{"name":"x","type":"f32"},{"name":"y","type":"f32"}]},
            "W":{"kind":"struct","groups":{"Stats":{"collapsed":true,"doc":"numbers"}},"fields":[
                {"name":"two_handed","type":"bool","group":"Stats","order":2,"label":"Two-handed","readonly":true},
                {"name":"hp","type":"i32","group":"Stats/Combat","order":1,"show_if":"two_handed"},
                {"name":"rarity","type":"Rarity","widget":"toggle","hide_if":"hp > 10"},
                {"name":"off","type":"Vec2","widget":"inline"}
            ]}}}"#;
        let (_, s) = Schema::parse(text).unwrap();
        let w = s.get("W").unwrap();
        assert_eq!(
            w.group("Stats"),
            Some(&GroupDef { collapsed: true, doc: Some("numbers".into()) })
        );
        let two = s.field("W", "two_handed").unwrap();
        assert_eq!(
            (two.group.as_deref(), two.order, two.label(), two.readonly),
            (Some("Stats"), Some(2.0), "Two-handed", true)
        );
        let hp = s.field("W", "hp").unwrap();
        assert_eq!(hp.show_if.as_ref().unwrap().text(), "two_handed");
        assert_eq!(s.field("W", "rarity").unwrap().hide_if.as_ref().unwrap().text(), "hp > 10");
        assert_eq!(s.field("W", "rarity").unwrap().widget, Some(Widget::Toggle));
        assert_eq!(s.field("W", "off").unwrap().widget, Some(Widget::Inline));
        let mut siblings = Map::new();
        siblings.insert("two_handed".into(), Value::Bool(false));
        siblings.insert("hp".into(), serde_json::json!(20));
        assert!(!hp.shown(&siblings));
        assert!(!s.field("W", "rarity").unwrap().shown(&siblings));
        siblings.insert("two_handed".into(), Value::Bool(true));
        siblings.insert("hp".into(), serde_json::json!(5));
        assert!(hp.shown(&siblings));
        assert!(s.field("W", "rarity").unwrap().shown(&siblings));

        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"i32","show_if":"y"}]}}"#),
            ["A.x: show_if names no field y"]
        );
        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"i32","hide_if":"x <"}]}}"#),
            ["A.x: hide_if \"x <\": < wants a number"]
        );
        assert_eq!(
            one(
                r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"i32","widget":"toggle"}]}}"#
            ),
            ["A.x: widget toggle wants an enum"]
        );
        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"i32","widget":"knob"}]}}"#),
            ["A.x: widget \"knob\" (want toggle, inline)"]
        );
        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"i32","group":""}]}}"#),
            ["A.x: group is empty"]
        );
        assert_eq!(
            one(r#"{"A":{"kind":"struct","fields":[{"name":"x","type":"i32","readonly":1}]}}"#),
            ["A.x: readonly is not true or false"]
        );
        assert_eq!(
            one(r#"{"A":{"kind":"struct","groups":[],"fields":[{"name":"x","type":"i32"}]}}"#),
            ["A: groups is not an object"]
        );
    }

    #[test]
    fn conditions_parse_every_form() {
        assert_eq!(Cond::parse("flag").unwrap().text(), "flag");
        assert_eq!(Cond::parse("!flag").unwrap().text(), "!flag");
        assert_eq!(Cond::parse("rarity == epic").unwrap().text(), "rarity == epic");
        assert_eq!(Cond::parse("rarity != \"epic\"").unwrap().value, serde_json::json!("epic"));
        assert_eq!(Cond::parse("hp >= 10").unwrap().text(), "hp >= 10");
        assert_eq!(Cond::parse("hp<=2.5").unwrap().text(), "hp <= 2.5");
        assert!(Cond::parse("1x").is_err());
        assert!(Cond::parse("hp > big").is_err());
        assert!(Cond::parse("").is_err());
    }

    #[test]
    fn colours_and_curves_parse_check_and_default() {
        use super::super::color;
        let s = schema();
        assert_eq!(TypeExpr::parse("rgb"), Ok(TypeExpr::Rgb));
        assert_eq!(TypeExpr::parse("rgba"), Ok(TypeExpr::Rgba));
        assert_eq!(TypeExpr::parse("curve"), Ok(TypeExpr::Curve));
        assert_eq!(TypeExpr::parse("list<rgba>").unwrap().text(), "list<rgba>");
        assert_eq!(s.default_of(&TypeExpr::Rgb), serde_json::json!("#000000"));
        assert_eq!(s.default_of(&TypeExpr::Rgba), serde_json::json!("#000000ff"));
        assert_eq!(
            s.default_of(&TypeExpr::Curve),
            serde_json::json!([[0, 0, 1, 0, true], [1, 1, 1, 1, true]]),
            "whole numbers written whole"
        );
        assert_eq!(s.parse_value(&TypeExpr::Rgb, "C83C1E"), Ok(serde_json::json!("#c83c1e")));
        assert_eq!(s.parse_value(&TypeExpr::Rgba, "#c83c1e"), Ok(serde_json::json!("#c83c1eff")));
        assert_eq!(s.parse_value(&TypeExpr::Rgb, "red"), Err("wants a colour as #rrggbb".into()));
        assert_eq!(
            s.parse_value(&TypeExpr::Rgb, "#c83c1e80"),
            Err("wants a colour as #rrggbb".into())
        );
        assert!(s.check(&TypeExpr::Rgb, &serde_json::json!("#C83C1E")).is_ok(), "upper case reads");
        assert!(s.check(&TypeExpr::Rgba, &serde_json::json!("#c83c1e")).is_ok());
        assert!(s.check(&TypeExpr::Rgb, &serde_json::json!(0xc83c1e)).is_err());
        assert_eq!(color::parse("#c83c1e80", true), Some([0xc8, 0x3c, 0x1e, 0x80]));

        let curve = |t: &str| s.check(&TypeExpr::Curve, &serde_json::from_str(t).unwrap());
        assert_eq!(curve("[[0, 0], [0.5, 0.8, 0, 0, true], [1, 1, 1, 1, true]]"), Ok(()));
        assert_eq!(curve("[]"), Ok(()));
        assert_eq!(curve("[[0, 0], [1.5, 1]]"), Err("point [1] x 1.5 is outside 0..1".into()));
        assert_eq!(
            curve("[[0.5, 0], [0.2, 1]]"),
            Err("point [1] x 0.2 is before the point ahead of it".into())
        );
        assert_eq!(curve("[[0, \"a\"]]"), Err("point [0] entry 1 is not a number".into()));
        assert_eq!(curve("[[0, 0, 0, 0, 1]]"), Err("point [0] locked is not true or false".into()));
        assert_eq!(
            curve("[[0]]"),
            Err("point [0] wants 2 to 5 entries: [x, y, out, in, locked]".into())
        );
        assert_eq!(curve("[0, 1]"), Err("point [0] is not an array".into()));
        assert_eq!(curve("{}"), Err("a curve is an array of points".into()));
        let read = curve_of(&serde_json::json!([[0, 0], [1, 1, 2, 3, true]])).unwrap();
        assert_eq!(
            read.points[0],
            crate::curve::Point { x: 0.0, y: 0.0, out: 0.0, in_: 0.0, locked: false }
        );
        assert_eq!(
            read.points[1],
            crate::curve::Point { x: 1.0, y: 1.0, out: 2.0, in_: 3.0, locked: true }
        );
        assert_eq!(
            s.parse_value(&TypeExpr::Curve, "[[0, 0], [1, 1]]"),
            Ok(serde_json::json!([[0, 0], [1, 1]]))
        );
        assert_eq!(
            s.parse_value(&TypeExpr::Curve, "[[2, 0]]"),
            Err("wants a curve as JSON: [[x, y, out, in, locked], …]".into())
        );
        assert_eq!(
            one(
                r##"{"A":{"kind":"struct","fields":[{"name":"c","type":"rgb","default":"red"},{"name":"k","type":"curve","min":0}]}}"##
            ),
            ["A.c: default \"red\" is not a colour as #rrggbb", "A.k: min/max/step on a curve"]
        );
    }

    #[test]
    fn defaults_follow_the_table_and_partials_fill_in() {
        let s = schema();
        assert_eq!(s.default_of(&TypeExpr::Bool), serde_json::json!(false));
        assert_eq!(s.default_of(&TypeExpr::F32), serde_json::json!(0));
        assert_eq!(s.default_of(&TypeExpr::Int(IntKind::U64)), serde_json::json!(0));
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
        assert!(ok("u8", serde_json::json!(255)));
        assert!(!ok("u8", serde_json::json!(256)));
        assert!(!ok("u8", serde_json::json!(-1)));
        assert!(!ok("i8", serde_json::json!(128)));
        assert!(ok("u64", serde_json::json!(u64::MAX)));
        assert!(!ok("i64", serde_json::json!(u64::MAX)));
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
            s.check(&TypeExpr::Int(IntKind::I32), &serde_json::json!("x")),
            Err("\"x\" is not a whole number -2147483648..2147483647".into())
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
        assert_eq!(p("i32", "4.2"), Err("wants a whole number -2147483648..2147483647".into()));
        assert_eq!(p("u8", "300"), Err("wants a whole number 0..255".into()));
        assert_eq!(p("u64", "18446744073709551615"), Ok(serde_json::json!(u64::MAX)));
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
