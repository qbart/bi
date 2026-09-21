//! The property view: rows over a schema or a data file, and every edit
//! the keys and `:bi` make, each returning the buffer's new text. See
//! `docs/specs/props.md`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::{Map, Value};

use super::data::{self, DataFile, Index};
use super::schema::{FieldDef, Schema, TypeDef, TypeExpr, compact, number, range_text, unquote};
use super::{Diagnostic, Kind, Level, check_dialect, is_identifier, summary, write_kind};
use crate::buffer::BufferId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// `Weapon rusty_sword`.
    Instance,
    /// A field of an instance, or of an embedded struct.
    Field,
    /// One item of a list.
    Item,
    /// A key the struct does not name — preserved, warned about.
    Unknown,
    /// `Weapon  struct`.
    Type,
    /// A field's definition: `damage  i32 = 10  0..999`.
    FieldDef,
    /// One attribute of a definition: `default 10`.
    Attr,
    /// One value of an enum.
    EnumValue,
    /// The one row a broken file shows.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Stable across rebuilds: `inst:3/drops/[1]`, `type:Weapon/field:damage/default`.
    pub key: String,
    pub depth: usize,
    pub label: String,
    pub value: String,
    pub kind: RowKind,
    /// The value is the default, not stored: drawn dim.
    pub inherited: bool,
    pub warning: Option<String>,
    pub doc: Option<String>,
    pub expandable: bool,
    pub expanded: bool,
}

/// What an edit asks the editor to do beyond the buffer's new text.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// The buffer's new text.
    Text(String),
    /// A line for the ex line — the edits that need a name.
    Prompt(String),
    /// This buffer's new text and the rewrite every other data file of
    /// the schema gets.
    Refactor { text: String, refactor: Refactor },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refactor {
    RenameId { ty: String, old: String, new: String },
    RenameField { ty: String, old: String, new: String },
    RenameEnumValue { en: String, old: String, new: String },
}

impl Refactor {
    /// The rewrite applied to one data document.
    pub fn apply(&self, doc: &mut Value, schema: &Schema) -> bool {
        match self {
            Refactor::RenameId { ty, old, new } => data::rename_id(doc, schema, ty, old, new),
            Refactor::RenameField { ty, old, new } => data::rename_field(doc, schema, ty, old, new),
            Refactor::RenameEnumValue { en, old, new } => {
                data::rename_enum_value(doc, schema, en, old, new)
            }
        }
    }
}

/// One path segment below an instance or a type.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    Key(String),
    Index(usize),
}

#[derive(Debug, Clone)]
pub struct Props {
    pub kind: Kind,
    pub buffer: BufferId,
    pub path: Option<PathBuf>,
    /// The schema a data file resolved, for the index and the refactorings.
    pub schema_path: Option<PathBuf>,
    pub raw: Value,
    pub schema: Schema,
    pub data: Option<DataFile>,
    pub diagnostics: Vec<Diagnostic>,
    pub index: Index,
    pub rows: Vec<Row>,
    pub selected: usize,
    expanded: BTreeSet<String>,
    /// The file is broken: the one row says how.
    pub error: Option<String>,
    /// The buffer's edit counter the rows reflect, and the schema
    /// buffer's when it is open.
    pub seen: Option<u64>,
    pub schema_seen: Option<u64>,
}

impl Props {
    pub fn new(kind: Kind, buffer: BufferId, path: Option<PathBuf>) -> Self {
        Self {
            kind,
            buffer,
            path,
            schema_path: None,
            raw: Value::Null,
            schema: Schema::default(),
            data: None,
            diagnostics: Vec::new(),
            index: Index::default(),
            rows: Vec::new(),
            selected: 0,
            expanded: BTreeSet::new(),
            error: None,
            seen: None,
            schema_seen: None,
        }
    }

    /// The `$schema` a data text names, before the schema can be loaded;
    /// a schema file needs none.
    pub fn schema_needed(kind: Kind, text: &str) -> Result<Option<String>, String> {
        match kind {
            Kind::Schema => Ok(None),
            Kind::Data => match data::parse(text) {
                Ok((_, d)) => Ok(Some(d.schema)),
                Err(errors) => Err(summary(&errors).unwrap_or_default()),
            },
        }
    }

    /// The text read, checked and turned into rows. `schema` is the
    /// schema file's text for a data file, or why it could not be read;
    /// `index` the other data files' instances. The error, if any, is
    /// kept on the view as well as returned.
    pub fn load(
        &mut self,
        text: &str,
        schema: Option<Result<String, String>>,
        index: Index,
    ) -> Result<(), String> {
        self.index = index;
        let result = match self.kind {
            Kind::Schema => Schema::parse(text).map(|(raw, schema)| {
                self.raw = raw;
                self.schema = schema;
                self.data = None;
                Vec::new()
            }),
            Kind::Data => data::parse(text).and_then(|(raw, d)| {
                let schema = match schema {
                    Some(Ok(text)) => Schema::parse(&text).map(|(_, s)| s).map_err(|e| {
                        vec![Diagnostic::error(
                            None,
                            format!("schema: {}", summary(&e).unwrap_or_default()),
                        )]
                    })?,
                    Some(Err(why)) => return Err(vec![Diagnostic::error(None, why)]),
                    None => return Err(vec![Diagnostic::error(None, "no schema")]),
                };
                let diagnostics = data::validate(&d, &schema, &self.index);
                if diagnostics.iter().any(|d| d.level == Level::Error) {
                    return Err(diagnostics);
                }
                self.raw = raw;
                self.schema = schema;
                self.data = Some(d);
                Ok(diagnostics)
            }),
        };
        match result {
            Ok(diagnostics) => {
                self.diagnostics = diagnostics;
                self.error = None;
                self.rebuild();
                Ok(())
            }
            Err(errors) => {
                let message = summary(&errors).unwrap_or_else(|| "broken".into());
                self.error = Some(message.clone());
                self.rows = vec![Row {
                    key: "error".into(),
                    depth: 0,
                    label: message.clone(),
                    value: String::new(),
                    kind: RowKind::Error,
                    inherited: false,
                    warning: None,
                    doc: None,
                    expandable: false,
                    expanded: false,
                }];
                self.selected = 0;
                Err(message)
            }
        }
    }

    /// The document as the writer spells it.
    pub fn text(&self) -> String {
        write_kind(self.kind, &self.raw)
    }

    /// The document normalised, as `:w` writes it.
    pub fn normalised_text(&self) -> String {
        match self.kind {
            Kind::Data => write_kind(self.kind, &data::normalise(&self.raw, &self.schema)),
            Kind::Schema => self.text(),
        }
    }

    pub fn warnings(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.level == Level::Warning).count()
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    // ---- rows ----

    /// Rows from the document and the expanded set; the selection follows
    /// its key.
    pub fn rebuild(&mut self) {
        let key = self.rows.get(self.selected).map(|r| r.key.clone());
        let mut rows = Vec::new();
        match self.kind {
            Kind::Data => self.data_rows(&mut rows),
            Kind::Schema => self.schema_rows(&mut rows),
        }
        for row in &mut rows {
            if row.warning.is_none() {
                row.warning = self
                    .diagnostics
                    .iter()
                    .find(|d| d.at.as_deref() == Some(&row.key))
                    .map(|d| d.message.clone());
            }
        }
        self.rows = rows;
        self.selected = key
            .and_then(|k| self.rows.iter().position(|r| r.key == k))
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
    }

    fn data_rows(&self, rows: &mut Vec<Row>) {
        let Some(data) = &self.data else { return };
        for (i, inst) in data.instances.iter().enumerate() {
            let key = format!("inst:{i}");
            let expanded = self.expanded.contains(&key);
            rows.push(Row {
                key: key.clone(),
                depth: 0,
                label: format!("{} {}", inst.ty, inst.id),
                value: String::new(),
                kind: RowKind::Instance,
                inherited: false,
                warning: None,
                doc: self.schema.get(&inst.ty).and_then(TypeDef::doc).map(str::to_string),
                expandable: true,
                expanded,
            });
            if !expanded {
                continue;
            }
            let Some(fields) = self.schema.fields_of(&inst.ty) else { continue };
            for field in fields {
                let stored = inst.values.get(&field.name);
                let resolved = match stored {
                    Some(v) => self.schema.resolve(&field.ty, v),
                    None => self.schema.field_default(field),
                };
                self.value_rows(
                    rows,
                    &format!("{key}/{}", field.name),
                    1,
                    &field.name,
                    &field.ty,
                    &resolved,
                    stored,
                    stored.is_none(),
                    Some(field),
                    RowKind::Field,
                );
            }
            for (k, v) in &inst.values {
                if !fields.iter().any(|f| &f.name == k) {
                    rows.push(Row {
                        key: format!("{key}/?{k}"),
                        depth: 1,
                        label: k.clone(),
                        value: v.to_string(),
                        kind: RowKind::Unknown,
                        inherited: false,
                        warning: None,
                        doc: None,
                        expandable: false,
                        expanded: false,
                    });
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn value_rows(
        &self,
        rows: &mut Vec<Row>,
        key: &str,
        depth: usize,
        label: &str,
        ty: &TypeExpr,
        resolved: &Value,
        stored: Option<&Value>,
        inherited: bool,
        field: Option<&FieldDef>,
        kind: RowKind,
    ) {
        let inner = match ty {
            TypeExpr::Optional(inner) if !resolved.is_null() => inner.as_ref(),
            other => other,
        };
        let expandable = match (inner, resolved) {
            (TypeExpr::Named(n), Value::Object(_)) => {
                self.schema.fields_of(n).is_some_and(|f| !f.is_empty())
            }
            (TypeExpr::List(_), Value::Array(items)) => !items.is_empty(),
            _ => false,
        };
        let expanded = expandable && self.expanded.contains(key);
        rows.push(Row {
            key: key.into(),
            depth,
            label: label.into(),
            value: self.value_text(ty, resolved),
            kind,
            inherited,
            warning: None,
            doc: field.and_then(|f| f.doc.clone()),
            expandable,
            expanded,
        });
        if !expanded {
            return;
        }
        match (inner, resolved) {
            (TypeExpr::Named(n), Value::Object(map)) => {
                let Some(fields) = self.schema.fields_of(n) else { return };
                let stored_map = stored.and_then(Value::as_object);
                for f in fields {
                    let sub_stored = stored_map.and_then(|m| m.get(&f.name));
                    let Some(sub) = map.get(&f.name) else { continue };
                    self.value_rows(
                        rows,
                        &format!("{key}/{}", f.name),
                        depth + 1,
                        &f.name,
                        &f.ty,
                        sub,
                        sub_stored,
                        inherited || sub_stored.is_none(),
                        Some(f),
                        RowKind::Field,
                    );
                }
                for (k, v) in map {
                    if !fields.iter().any(|f| &f.name == k) {
                        rows.push(Row {
                            key: format!("{key}/{k}"),
                            depth: depth + 1,
                            label: k.clone(),
                            value: v.to_string(),
                            kind: RowKind::Unknown,
                            inherited,
                            warning: None,
                            doc: None,
                            expandable: false,
                            expanded: false,
                        });
                    }
                }
            }
            (TypeExpr::List(item_ty), Value::Array(items)) => {
                let stored_items = stored.and_then(Value::as_array);
                for (i, item) in items.iter().enumerate() {
                    let sub_stored = stored_items.and_then(|s| s.get(i));
                    self.value_rows(
                        rows,
                        &format!("{key}/[{i}]"),
                        depth + 1,
                        &format!("[{i}]"),
                        item_ty,
                        item,
                        sub_stored,
                        inherited,
                        None,
                        RowKind::Item,
                    );
                }
            }
            _ => {}
        }
    }

    fn schema_rows(&self, rows: &mut Vec<Row>) {
        for (name, def) in &self.schema.types {
            let key = format!("type:{name}");
            let expanded = self.expanded.contains(&key);
            rows.push(Row {
                key: key.clone(),
                depth: 0,
                label: name.clone(),
                value: def.kind().into(),
                kind: RowKind::Type,
                inherited: false,
                warning: None,
                doc: def.doc().map(str::to_string),
                expandable: true,
                expanded,
            });
            if !expanded {
                continue;
            }
            rows.push(attr_row(
                &format!("{key}/doc"),
                1,
                "doc",
                def.doc().map(|d| format!("{d:?}")),
            ));
            match def {
                TypeDef::Struct { fields, .. } => {
                    for f in fields {
                        let fkey = format!("{key}/field:{}", f.name);
                        let fexpanded = self.expanded.contains(&fkey);
                        rows.push(Row {
                            key: fkey.clone(),
                            depth: 1,
                            label: f.name.clone(),
                            value: self.field_def_text(f),
                            kind: RowKind::FieldDef,
                            inherited: false,
                            warning: None,
                            doc: f.doc.clone(),
                            expandable: true,
                            expanded: fexpanded,
                        });
                        if !fexpanded {
                            continue;
                        }
                        rows.push(attr_row(&format!("{fkey}/type"), 2, "type", Some(f.ty.text())));
                        rows.push(attr_row(
                            &format!("{fkey}/default"),
                            2,
                            "default",
                            f.default
                                .as_ref()
                                .map(|d| self.value_text(&f.ty, &self.schema.resolve(&f.ty, d))),
                        ));
                        rows.push(attr_row(&format!("{fkey}/min"), 2, "min", f.min.map(compact)));
                        rows.push(attr_row(&format!("{fkey}/max"), 2, "max", f.max.map(compact)));
                        rows.push(attr_row(
                            &format!("{fkey}/step"),
                            2,
                            "step",
                            f.step.map(compact),
                        ));
                        rows.push(attr_row(
                            &format!("{fkey}/doc"),
                            2,
                            "doc",
                            f.doc.as_ref().map(|d| format!("{d:?}")),
                        ));
                    }
                }
                TypeDef::Enum { values, .. } => {
                    for (i, v) in values.iter().enumerate() {
                        rows.push(Row {
                            key: format!("{key}/value:{i}"),
                            depth: 1,
                            label: v.clone(),
                            value: String::new(),
                            kind: RowKind::EnumValue,
                            inherited: false,
                            warning: None,
                            doc: None,
                            expandable: false,
                            expanded: false,
                        });
                    }
                }
            }
        }
    }

    /// `i32 = 10  0..999  step 0.1`.
    fn field_def_text(&self, f: &FieldDef) -> String {
        let mut out = f.ty.text();
        if let Some(d) = &f.default {
            out.push_str(&format!(" = {}", self.value_text(&f.ty, &self.schema.resolve(&f.ty, d))));
        }
        let range = range_text(f);
        if !range.is_empty() {
            out.push_str(&format!("  {range}"));
        }
        if let Some(step) = f.step {
            out.push_str(&format!("  step {}", compact(step)));
        }
        out
    }

    /// A resolved value as its row shows it.
    pub fn value_text(&self, ty: &TypeExpr, value: &Value) -> String {
        match (ty, value) {
            (_, Value::Null) => "—".into(),
            (TypeExpr::Ref(target), Value::String(id)) => {
                if id.is_empty() {
                    format!("(unset) → {target}")
                } else {
                    format!("{id} → {target}")
                }
            }
            (TypeExpr::Optional(inner), v) => self.value_text(inner, v),
            (TypeExpr::Str, Value::String(s)) => format!("{s:?}"),
            (TypeExpr::Named(_), Value::String(s)) => s.clone(),
            (TypeExpr::List(inner), Value::Array(items)) => {
                if items.is_empty() {
                    return "[]".into();
                }
                let parts: Vec<String> = items.iter().map(|v| self.bare_text(inner, v)).collect();
                let joined = format!("[{}]", parts.join(", "));
                if joined.chars().count() <= 48
                    && items.iter().all(|v| !v.is_object() && !v.is_array())
                {
                    joined
                } else {
                    format!("{} × {}", items.len(), inner.text())
                }
            }
            (TypeExpr::Named(name), Value::Object(map)) => {
                let parts: Vec<String> = map
                    .iter()
                    .map(|(k, v)| {
                        let ty = self
                            .schema
                            .field(name, k)
                            .map(|f| f.ty.clone())
                            .unwrap_or(TypeExpr::Str);
                        format!("{k} {}", self.bare_text(&ty, v))
                    })
                    .collect();
                let joined = format!("{{ {} }}", parts.join(", "));
                if joined.chars().count() <= 48
                    && map.values().all(|v| !v.is_object() && !v.is_array())
                {
                    joined
                } else {
                    name.clone()
                }
            }
            (_, v) => v.to_string(),
        }
    }

    /// Inside a compact list or struct: strings and ids bare.
    fn bare_text(&self, ty: &TypeExpr, value: &Value) -> String {
        match (ty, value) {
            (_, Value::String(s)) => s.clone(),
            (TypeExpr::Optional(inner), v) => self.bare_text(inner, v),
            _ => self.value_text(ty, value),
        }
    }

    /// A value as the ex line takes it back — `Enter`'s prefill.
    fn edit_text(&self, ty: &TypeExpr, value: &Value) -> String {
        match (ty, value) {
            (_, Value::Null) => "none".into(),
            (TypeExpr::Optional(inner), v) => self.edit_text(inner, v),
            (TypeExpr::Str, Value::String(s))
            | (TypeExpr::Ref(_), Value::String(s))
            | (TypeExpr::Named(_), Value::String(s)) => {
                if s.is_empty() || s != s.trim() || s.contains('"') {
                    format!("{s:?}")
                } else {
                    s.clone()
                }
            }
            (_, v) => v.to_string(),
        }
    }

    // ---- selection and expansion ----

    pub fn select(&mut self, index: usize) {
        self.selected = index.min(self.rows.len().saturating_sub(1));
    }

    pub fn select_by(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    /// `l`: open the row. False when it has nothing to open.
    pub fn expand(&mut self) -> bool {
        let Some(row) = self.rows.get(self.selected) else { return false };
        if !row.expandable {
            return false;
        }
        if !row.expanded {
            self.expanded.insert(row.key.clone());
            self.rebuild();
        }
        true
    }

    /// `h`: close the row, or go to its parent.
    pub fn collapse(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        if row.expanded {
            self.expanded.remove(&row.key);
            self.rebuild();
            return;
        }
        let depth = row.depth;
        if let Some(parent) = self.rows[..self.selected].iter().rposition(|r| r.depth < depth) {
            self.selected = parent;
        }
    }

    /// `Enter`: open or close a row with children.
    pub fn toggle_expand(&mut self) -> bool {
        let Some(row) = self.rows.get(self.selected) else { return false };
        if !row.expandable {
            return false;
        }
        if row.expanded {
            self.expanded.remove(&row.key);
        } else {
            self.expanded.insert(row.key.clone());
        }
        self.rebuild();
        true
    }

    /// The instance `ty` `id` opened and selected, when this file has it.
    pub fn select_instance(&mut self, ty: &str, id: &str) -> bool {
        let Some(data) = &self.data else { return false };
        let Some(i) = data.instances.iter().position(|x| x.ty == ty && x.id == id) else {
            return false;
        };
        let key = format!("inst:{i}");
        self.expanded.insert(key.clone());
        self.rebuild();
        if let Some(at) = self.rows.iter().position(|r| r.key == key) {
            self.selected = at;
        }
        true
    }

    // ---- paths ----

    /// The `:bi set` path of a row, as `Enter` prefills it.
    pub fn path_of(&self, key: &str) -> Option<String> {
        let (head, rest) = match key.split_once('/') {
            Some((h, r)) => (h, Some(r)),
            None => (key, None),
        };
        if let Some(i) = head.strip_prefix("inst:") {
            let i: usize = i.parse().ok()?;
            let inst = self.data.as_ref()?.instances.get(i)?;
            let mut out = self.instance_ref(inst.ty.as_str(), inst.id.as_str());
            for seg in rest.into_iter().flat_map(|r| r.split('/')) {
                if let Some(k) = seg.strip_prefix('?') {
                    out.push_str(&format!(".{k}"));
                } else if seg.starts_with('[') {
                    out.push_str(seg);
                } else {
                    out.push_str(&format!(".{seg}"));
                }
            }
            return Some(out);
        }
        let name = head.strip_prefix("type:")?;
        let mut out = name.to_string();
        for seg in rest.into_iter().flat_map(|r| r.split('/')) {
            if let Some(f) = seg.strip_prefix("field:") {
                out.push_str(&format!(".{f}"));
            } else if let Some(i) = seg.strip_prefix("value:") {
                out.push_str(&format!(".values[{i}]"));
            } else {
                out.push_str(&format!(".{seg}"));
            }
        }
        Some(out)
    }

    /// `goblin`, or `Enemy:goblin` when a Weapon is called goblin too.
    fn instance_ref(&self, ty: &str, id: &str) -> String {
        let shared = self
            .data
            .as_ref()
            .is_some_and(|d| d.instances.iter().filter(|i| i.id == id).count() > 1);
        if shared { format!("{ty}:{id}") } else { id.to_string() }
    }

    /// A data path parsed: the instance's index and the segments under it.
    fn parse_data_path(&self, path: &str) -> Result<(usize, Vec<Seg>), String> {
        let data = self.data.as_ref().ok_or("not a data file")?;
        let end = path.find(['.', '[']).unwrap_or(path.len());
        let (head, rest) = path.split_at(end);
        let index = match head.split_once(':') {
            Some((ty, id)) => data
                .instances
                .iter()
                .position(|i| i.ty == ty && i.id == id)
                .ok_or_else(|| format!("no {ty} {id}"))?,
            None => {
                let hits: Vec<usize> = data
                    .instances
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| i.id == head)
                    .map(|(n, _)| n)
                    .collect();
                match hits.as_slice() {
                    [one] => *one,
                    [] => return Err(format!("no instance {head}")),
                    _ => {
                        let types: Vec<String> = hits
                            .iter()
                            .map(|&n| format!("{}:{head}", data.instances[n].ty))
                            .collect();
                        return Err(format!("{head} is ambiguous (want {})", types.join(" or ")));
                    }
                }
            }
        };
        Ok((index, parse_segs(rest)?))
    }

    /// The type at the end of a segment chain from a struct, and the
    /// field definition it belongs to when it is a field.
    fn walk<'a>(
        &'a self,
        ty: &str,
        segs: &[Seg],
    ) -> Result<(TypeExpr, Option<&'a FieldDef>), String> {
        let mut current = TypeExpr::Named(ty.into());
        let mut field = None;
        for seg in segs {
            let inner = match &current {
                TypeExpr::Optional(inner) => inner.as_ref().clone(),
                other => other.clone(),
            };
            match (inner, seg) {
                (TypeExpr::Named(name), Seg::Key(k)) => {
                    let f = self
                        .schema
                        .field(&name, k)
                        .ok_or_else(|| format!("no field {k} on {name}"))?;
                    current = f.ty.clone();
                    field = Some(f);
                }
                (TypeExpr::List(item), Seg::Index(_)) => {
                    current = *item;
                    field = None;
                }
                (t, Seg::Key(k)) => return Err(format!("{} has no field {k}", t.text())),
                (t, Seg::Index(n)) => return Err(format!("{} has no item [{n}]", t.text())),
            }
        }
        Ok((current, field))
    }

    // ---- data edits ----

    /// `:bi set <path> <value>`, or `Enter`'s target. `value` empty reports.
    pub fn set(&self, path: &str, value: &str) -> Result<Edit, String> {
        match self.kind {
            Kind::Data => self.set_data(path, value),
            Kind::Schema => self.set_schema(path, value),
        }
    }

    fn set_data(&self, path: &str, text: &str) -> Result<Edit, String> {
        let (index, segs) = self.parse_data_path(path)?;
        let inst = &self.data.as_ref().ok_or("not a data file")?.instances[index];
        if segs.as_slice() == [Seg::Key("$id".into())] {
            if text.is_empty() {
                return Err(format!("{path} = {}", inst.id));
            }
            let new = unquote(text);
            return self.rename_id(&inst.ty.clone(), &inst.id.clone(), &new);
        }
        if segs.is_empty() {
            return Err(format!("{path} is an instance; name a field"));
        }
        let (ty, _) = self.walk(&inst.ty, &segs)?;
        if text.is_empty() {
            let (resolved, _) = self.resolved_at(index, &segs)?;
            return Err(format!("{path} = {}", self.edit_text(&ty, &resolved)));
        }
        let value = self.schema.parse_value(&ty, text).map_err(|e| format!("{path} {e}"))?;
        let mut raw = self.raw.clone();
        self.assign_at(&mut raw, index, &segs, value)?;
        Ok(Edit::Text(write_kind(self.kind, &raw)))
    }

    /// The resolved value at a path and whether it is stored.
    fn resolved_at(&self, index: usize, segs: &[Seg]) -> Result<(Value, bool), String> {
        let inst = &self.data.as_ref().ok_or("not a data file")?.instances[index];
        let Some(Seg::Key(first)) = segs.first() else { return Err("no field".into()) };
        let field = self
            .schema
            .field(&inst.ty, first)
            .ok_or_else(|| format!("no field {first} on {}", inst.ty))?;
        let stored = inst.values.get(first);
        let mut value = match stored {
            Some(v) => self.schema.resolve(&field.ty, v),
            None => self.schema.field_default(field),
        };
        let mut is_stored = stored.is_some();
        let mut stored_here = stored.cloned();
        for seg in &segs[1..] {
            match seg {
                Seg::Key(k) => {
                    value = value.get(k).cloned().ok_or_else(|| format!("no {k}"))?;
                    stored_here = stored_here.and_then(|s| s.get(k).cloned());
                }
                Seg::Index(n) => {
                    value = value.get(n).cloned().ok_or_else(|| format!("no item [{n}]"))?;
                    stored_here = stored_here.and_then(|s| s.get(n).cloned());
                }
            }
            is_stored = stored_here.is_some();
        }
        Ok((value, is_stored))
    }

    /// `value` written at `segs` of instance `index` in `raw`, the
    /// structs on the way created sparse, the lists materialised, the
    /// instance normalised after.
    fn assign_at(
        &self,
        raw: &mut Value,
        index: usize,
        segs: &[Seg],
        value: Value,
    ) -> Result<(), String> {
        let inst = &self.data.as_ref().ok_or("not a data file")?.instances[index];
        let Some(Seg::Key(first)) = segs.first() else { return Err("no field".into()) };
        let field = self
            .schema
            .field(&inst.ty, first)
            .ok_or_else(|| format!("no field {first} on {}", inst.ty))?;
        let stored = inst.values.get(first);
        let resolved = match stored {
            Some(v) => self.schema.resolve(&field.ty, v),
            None => self.schema.field_default(field),
        };
        let new = assign(&self.schema, &field.ty, stored, &resolved, &segs[1..], value)?;
        let item = raw
            .get_mut("instances")
            .and_then(Value::as_array_mut)
            .and_then(|a| a.get_mut(index))
            .ok_or("no instance")?;
        let map = item.as_object_mut().ok_or("no instance")?;
        map.insert(first.clone(), new);
        *item = normalise_one(item, &self.schema);
        Ok(())
    }

    /// `dd` on a row: what it removes, or the prompt it needs.
    pub fn delete(&self, key: &str) -> Result<Edit, String> {
        match self.kind {
            Kind::Data => self.delete_data(key),
            Kind::Schema => self.delete_schema(key),
        }
    }

    fn delete_data(&self, key: &str) -> Result<Edit, String> {
        let (index, segs) = parse_data_key(key)?;
        let data = self.data.as_ref().ok_or("not a data file")?;
        let inst = data.instances.get(index).ok_or("no instance")?;
        let mut raw = self.raw.clone();
        if segs.is_empty() {
            let referrers = data::referrers(data, &self.schema, &inst.ty, &inst.id);
            if !referrers.is_empty() {
                let who: Vec<String> = referrers.iter().map(|(t, i)| format!("{t} {i}")).collect();
                return Ok(Edit::Prompt(format!(
                    "bi delete {} {}  # referenced by {}",
                    inst.ty,
                    inst.id,
                    who.join(", ")
                )));
            }
            raw["instances"].as_array_mut().ok_or("no instances")?.remove(index);
            return Ok(Edit::Text(write_kind(self.kind, &raw)));
        }
        let item =
            raw["instances"].get_mut(index).and_then(Value::as_object_mut).ok_or("no instance")?;
        match segs.as_slice() {
            [Seg::Key(k)] => {
                if item.shift_remove(k).is_none() {
                    return Err(format!("{k} is already the default"));
                }
            }
            [Seg::Key(first), rest @ ..] => {
                let field = self
                    .schema
                    .field(&inst.ty, first)
                    .ok_or_else(|| format!("no field {first}"))?;
                let stored = inst.values.get(first);
                let resolved = match stored {
                    Some(v) => self.schema.resolve(&field.ty, v),
                    None => self.schema.field_default(field),
                };
                let new = remove_in(&self.schema, &field.ty, stored, &resolved, rest)?;
                item.insert(first.clone(), new);
            }
            _ => return Err("nothing to remove".into()),
        }
        let normalised = normalise_one(&Value::Object(std::mem::take(item)), &self.schema);
        raw["instances"][index] = normalised;
        Ok(Edit::Text(write_kind(self.kind, &raw)))
    }

    /// `Space` on a row: a bool flipped, an enum, a ref or an optional
    /// turned. `steps` is the direction and count for `Ctrl-A`/`Ctrl-X`.
    pub fn turn(&self, key: &str, steps: i64, space: bool) -> Result<Edit, String> {
        match self.kind {
            Kind::Data => self.turn_data(key, steps, space),
            Kind::Schema => self.turn_schema(key, steps, space),
        }
    }

    fn turn_data(&self, key: &str, steps: i64, space: bool) -> Result<Edit, String> {
        let (index, segs) = parse_data_key(key)?;
        if segs.is_empty() {
            return Err("an instance; pick a field".into());
        }
        let inst = &self.data.as_ref().ok_or("not a data file")?.instances[index];
        let (ty, field) = self.walk(&inst.ty, &segs)?;
        let (current, _) = self.resolved_at(index, &segs)?;
        let value = self.turned(&ty, field, &current, steps, space)?;
        let mut raw = self.raw.clone();
        self.assign_at(&mut raw, index, &segs, value)?;
        Ok(Edit::Text(write_kind(self.kind, &raw)))
    }

    /// The next value of a type: what `Space` and the nudges produce.
    fn turned(
        &self,
        ty: &TypeExpr,
        field: Option<&FieldDef>,
        current: &Value,
        steps: i64,
        space: bool,
    ) -> Result<Value, String> {
        match ty {
            TypeExpr::Bool => Ok(Value::Bool(!current.as_bool().unwrap_or(false))),
            TypeExpr::I32 | TypeExpr::I64 | TypeExpr::F32 | TypeExpr::F64 => {
                if space {
                    return Err("a number: Ctrl-A and Ctrl-X turn it, Enter edits it".into());
                }
                let step =
                    field.and_then(|f| f.step).unwrap_or(if ty.is_integer() { 1.0 } else { 0.1 });
                let now = current.as_f64().unwrap_or(0.0);
                let mut next = now + steps as f64 * step;
                let scale = 10f64.powi(decimals(step) as i32);
                next = (next * scale).round() / scale;
                if let Some(min) = field.and_then(|f| f.min) {
                    next = next.max(min);
                }
                if let Some(max) = field.and_then(|f| f.max) {
                    next = next.min(max);
                }
                if ty.is_integer() {
                    next = next.round();
                }
                Ok(number(next))
            }
            TypeExpr::Named(name) => match self.schema.get(name) {
                Some(TypeDef::Enum { values, .. }) => {
                    let at = current
                        .as_str()
                        .and_then(|c| values.iter().position(|v| v == c))
                        .unwrap_or(0) as i64;
                    let next = (at + steps).rem_euclid(values.len().max(1) as i64) as usize;
                    Ok(Value::String(values[next].clone()))
                }
                _ => Err("a struct: open it and turn a field".into()),
            },
            TypeExpr::Ref(target) => {
                let ids = self.ids_of(target);
                if ids.is_empty() {
                    return Err(format!("no {target} to point at"));
                }
                let at = current.as_str().and_then(|c| ids.iter().position(|v| v == c));
                let next = match at {
                    Some(at) => (at as i64 + steps).rem_euclid(ids.len() as i64) as usize,
                    None => {
                        if steps >= 0 {
                            0
                        } else {
                            ids.len() - 1
                        }
                    }
                };
                Ok(Value::String(ids[next].clone()))
            }
            TypeExpr::Optional(inner) => {
                if current.is_null() {
                    let seed = match inner.as_ref() {
                        TypeExpr::Ref(target) => {
                            Value::String(self.ids_of(target).first().cloned().unwrap_or_default())
                        }
                        other => self.schema.default_of(other),
                    };
                    Ok(seed)
                } else if space {
                    Ok(Value::Null)
                } else {
                    self.turned(inner, field, current, steps, space)
                }
            }
            TypeExpr::Str => Err("a string: Enter edits it".into()),
            TypeExpr::List(_) => Err("a list: open it, or `a` adds an item".into()),
        }
    }

    /// Every id of struct `ty` — this file's and the index's, sorted.
    pub fn ids_of(&self, ty: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .data
            .iter()
            .flat_map(|d| d.instances.iter())
            .filter(|i| i.ty == ty)
            .map(|i| i.id.clone())
            .collect();
        ids.extend(self.index.ids_of(ty).map(str::to_string));
        ids.sort();
        ids.dedup();
        ids
    }

    /// `a` on a row: an item on a list, or a prompt for what needs a name.
    pub fn add(&self, key: &str) -> Result<Edit, String> {
        match self.kind {
            Kind::Data => self.add_data(key),
            Kind::Schema => self.add_schema(key),
        }
    }

    fn add_data(&self, key: &str) -> Result<Edit, String> {
        let Ok((index, mut segs)) = parse_data_key(key) else {
            return Ok(Edit::Prompt("bi new ".into()));
        };
        if segs.is_empty() {
            return Ok(Edit::Prompt("bi new ".into()));
        }
        let inst = &self.data.as_ref().ok_or("not a data file")?.instances[index];
        // On an item, the list it is in.
        if matches!(segs.last(), Some(Seg::Index(_))) {
            segs.pop();
        }
        let (ty, _) = self.walk(&inst.ty, &segs)?;
        let item_ty = match &ty {
            TypeExpr::List(inner) => inner.as_ref().clone(),
            TypeExpr::Optional(inner) => match inner.as_ref() {
                TypeExpr::List(item) => item.as_ref().clone(),
                _ => return Err("not a list".into()),
            },
            _ => return Err("not a list (`:bi new` adds an instance)".into()),
        };
        let (current, _) = self.resolved_at(index, &segs)?;
        let mut items = current.as_array().cloned().unwrap_or_default();
        items.push(self.schema.default_of(&item_ty));
        let mut raw = self.raw.clone();
        self.assign_at(&mut raw, index, &segs, Value::Array(items))?;
        Ok(Edit::Text(write_kind(self.kind, &raw)))
    }

    /// `:bi new <Type> <id>`: an instance at the end, required refs blank.
    pub fn new_instance(&self, ty: &str, id: &str) -> Result<Edit, String> {
        let data = self.data.as_ref().ok_or("not a data file")?;
        let fields = self.schema.fields_of(ty).ok_or_else(|| match self.schema.get(ty) {
            Some(_) => format!("{ty} is an enum"),
            None => format!(
                "no type {ty} (want {})",
                self.schema.structs().collect::<Vec<_>>().join(", ")
            ),
        })?;
        if !is_identifier(id) {
            return Err(format!("{id:?} is not an identifier"));
        }
        if data.instances.iter().any(|i| i.ty == ty && i.id == id) || self.index.has(ty, id) {
            return Err(format!("{ty} {id} exists"));
        }
        let mut map = Map::new();
        map.insert("$type".into(), Value::String(ty.into()));
        map.insert("$id".into(), Value::String(id.into()));
        for f in fields {
            if matches!(f.ty, TypeExpr::Ref(_)) && f.default.is_none() {
                map.insert(f.name.clone(), Value::String(String::new()));
            }
        }
        let mut raw = self.raw.clone();
        raw["instances"].as_array_mut().ok_or("no instances")?.push(Value::Object(map));
        Ok(Edit::Text(write_kind(self.kind, &raw)))
    }

    /// `:bi delete <Type> <id>`: the instance, refs to it left dangling.
    pub fn delete_instance(&self, ty: &str, id: &str) -> Result<Edit, String> {
        let data = self.data.as_ref().ok_or("not a data file")?;
        let index = data
            .instances
            .iter()
            .position(|i| i.ty == ty && i.id == id)
            .ok_or_else(|| format!("no {ty} {id}"))?;
        let mut raw = self.raw.clone();
        raw["instances"].as_array_mut().ok_or("no instances")?.remove(index);
        Ok(Edit::Text(write_kind(self.kind, &raw)))
    }

    /// The row `inst:i` of instance `ty` `id`, for the prompt-confirmed
    /// delete to land the selection sensibly after.
    pub fn instance_row(&self, ty: &str, id: &str) -> Option<usize> {
        let i = self.data.as_ref()?.instances.iter().position(|x| x.ty == ty && x.id == id)?;
        self.rows.iter().position(|r| r.key == format!("inst:{i}"))
    }

    fn rename_id(&self, ty: &str, old: &str, new: &str) -> Result<Edit, String> {
        if !is_identifier(new) {
            return Err(format!("{new:?} is not an identifier"));
        }
        if new == old {
            return Err(format!("{ty} {old} is already called that"));
        }
        let data = self.data.as_ref().ok_or("not a data file")?;
        if data.instances.iter().any(|i| i.ty == ty && i.id == new) || self.index.has(ty, new) {
            return Err(format!("{ty} {new} exists"));
        }
        let mut raw = self.raw.clone();
        data::rename_id(&mut raw, &self.schema, ty, old, new);
        Ok(Edit::Refactor {
            text: write_kind(self.kind, &raw),
            refactor: Refactor::RenameId { ty: ty.into(), old: old.into(), new: new.into() },
        })
    }

    /// `:bi rename <Type>.<old> <new>`: an id in a data file, a field or
    /// an enum value in a schema.
    pub fn rename(&self, spec: &str, new: &str) -> Result<Edit, String> {
        let (ty, old) = spec.split_once('.').ok_or("want <Type>.<name>")?;
        match self.kind {
            Kind::Data => self.rename_id(ty, old, new),
            Kind::Schema => self.rename_in_schema(ty, old, new),
        }
    }

    /// `:bi prune`: every unknown key gone.
    pub fn prune(&self) -> Result<(Edit, usize), String> {
        if self.kind != Kind::Data {
            return Err("not a data file".into());
        }
        let mut raw = self.raw.clone();
        let n = data::prune(&mut raw, &self.schema);
        Ok((Edit::Text(write_kind(self.kind, &raw)), n))
    }

    /// `gd` on a ref row: the target.
    pub fn target(&self, key: &str) -> Result<(String, String), String> {
        let (index, segs) = parse_data_key(key)?;
        let inst = &self.data.as_ref().ok_or("not a data file")?.instances[index];
        let (ty, _) = self.walk(&inst.ty, &segs)?;
        let target = match &ty {
            TypeExpr::Ref(t) => t.clone(),
            TypeExpr::Optional(inner) => match inner.as_ref() {
                TypeExpr::Ref(t) => t.clone(),
                _ => return Err("not a ref".into()),
            },
            _ => return Err("not a ref".into()),
        };
        let (value, _) = self.resolved_at(index, &segs)?;
        match value.as_str() {
            Some(id) if !id.is_empty() => Ok((target, id.into())),
            _ => Err(format!("ref<{target}> not set")),
        }
    }

    /// `Enter` on a leaf: the ex line to prefill; `None` on a row that
    /// opens instead.
    pub fn edit_line(&self, key: &str) -> Option<String> {
        let row = self.rows.iter().find(|r| r.key == key)?;
        if row.expandable
            || matches!(
                row.kind,
                RowKind::Instance | RowKind::Type | RowKind::FieldDef | RowKind::Error
            )
        {
            return None;
        }
        let path = self.path_of(key)?;
        let value = match self.kind {
            Kind::Data => {
                if row.kind == RowKind::Unknown {
                    return None;
                }
                let (index, segs) = parse_data_key(key).ok()?;
                let inst = self.data.as_ref()?.instances.get(index)?;
                let (ty, _) = self.walk(&inst.ty, &segs).ok()?;
                let (value, _) = self.resolved_at(index, &segs).ok()?;
                self.edit_text(&ty, &value)
            }
            Kind::Schema => self.schema_edit_text(key).unwrap_or_default(),
        };
        Some(format!("bi set {path} {value}"))
    }

    // ---- schema edits ----

    fn parse_schema_path(&self, path: &str) -> Result<(String, Vec<Seg>), String> {
        let end = path.find(['.', '[']).unwrap_or(path.len());
        let (head, rest) = path.split_at(end);
        if self.schema.get(head).is_none() {
            return Err(format!("no type {head}"));
        }
        Ok((head.into(), parse_segs(rest)?))
    }

    fn schema_edit_text(&self, key: &str) -> Option<String> {
        let mut parts = key.split('/');
        let name = parts.next()?.strip_prefix("type:")?;
        let def = self.schema.get(name)?;
        let second = parts.next()?;
        if second == "doc" {
            return Some(def.doc().map(|d| format!("{d:?}")).unwrap_or_else(|| "-".into()));
        }
        if let Some(i) = second.strip_prefix("value:") {
            let i: usize = i.parse().ok()?;
            return self.schema.enum_values(name)?.get(i).cloned();
        }
        let fname = second.strip_prefix("field:")?;
        let f = self.schema.field(name, fname)?;
        let attr = parts.next()?;
        Some(match attr {
            "type" => f.ty.text(),
            "default" => {
                f.default.as_ref().map(|d| self.edit_text(&f.ty, d)).unwrap_or_else(|| "-".into())
            }
            "min" => f.min.map(compact).unwrap_or_else(|| "-".into()),
            "max" => f.max.map(compact).unwrap_or_else(|| "-".into()),
            "step" => f.step.map(compact).unwrap_or_else(|| "-".into()),
            "doc" => f.doc.as_ref().map(|d| format!("{d:?}")).unwrap_or_else(|| "-".into()),
            _ => return None,
        })
    }

    fn set_schema(&self, path: &str, text: &str) -> Result<Edit, String> {
        let (name, segs) = self.parse_schema_path(path)?;
        let none = matches!(text, "-" | "none" | "null");
        match segs.as_slice() {
            [Seg::Key(k)] if k == "doc" => {
                if text.is_empty() {
                    return Err(format!(
                        "{path} = {}",
                        self.schema.get(&name).and_then(TypeDef::doc).unwrap_or("-")
                    ));
                }
                let mut raw = self.raw.clone();
                let def = raw["types"][&name].as_object_mut().ok_or("no type")?;
                if none {
                    def.shift_remove("doc");
                } else {
                    def.insert("doc".into(), Value::String(unquote(text)));
                }
                self.checked(raw)
            }
            [Seg::Key(k), Seg::Index(i)] if k == "values" => {
                let values = self
                    .schema
                    .enum_values(&name)
                    .ok_or_else(|| format!("{name} is not an enum"))?;
                let old = values.get(*i).ok_or_else(|| format!("{name} has no values[{i}]"))?;
                if text.is_empty() {
                    return Err(format!("{path} = {old}"));
                }
                self.rename_in_schema(&name, old, &unquote(text))
            }
            [Seg::Key(fname), Seg::Key(attr)] => {
                let f = self
                    .schema
                    .field(&name, fname)
                    .ok_or_else(|| format!("no field {fname} on {name}"))?;
                if attr == "name" {
                    if text.is_empty() {
                        return Err(format!("{path} = {fname}"));
                    }
                    return self.rename_in_schema(&name, fname, &unquote(text));
                }
                if text.is_empty() {
                    let key = format!("type:{name}/field:{fname}/{attr}");
                    return Err(format!(
                        "{path} = {}",
                        self.schema_edit_text(&key).unwrap_or_else(|| "-".into())
                    ));
                }
                let value = match attr.as_str() {
                    "type" => {
                        TypeExpr::parse(text)?;
                        Some(Value::String(text.into()))
                    }
                    "default" => {
                        if none {
                            None
                        } else {
                            Some(
                                self.schema
                                    .parse_value(&f.ty, text)
                                    .map_err(|e| format!("{path} {e}"))?,
                            )
                        }
                    }
                    "min" | "max" | "step" => {
                        if none {
                            None
                        } else {
                            let n: f64 =
                                text.parse().map_err(|_| format!("{path} wants a number"))?;
                            Some(number(n))
                        }
                    }
                    "doc" => {
                        if none {
                            None
                        } else {
                            Some(Value::String(unquote(text)))
                        }
                    }
                    other => {
                        return Err(format!(
                            "not a field attribute: {other} (want type, default, min, max, step, doc, name)"
                        ));
                    }
                };
                if attr == "type" && value.is_none() {
                    return Err("a field needs a type".into());
                }
                let mut raw = self.raw.clone();
                let field = field_mut(&mut raw, &name, fname).ok_or("no field")?;
                match value {
                    Some(v) => {
                        field.insert(attr.clone(), v);
                    }
                    None => {
                        field.shift_remove(attr);
                    }
                }
                self.checked(raw)
            }
            [] => Err(format!("{path} is a type; name an attribute")),
            _ => Err(format!("not a schema path: {path}")),
        }
    }

    /// A changed schema document, refused with its first error when the
    /// change broke it.
    fn checked(&self, raw: Value) -> Result<Edit, String> {
        Schema::from_value(&raw).map_err(|e| summary(&e).unwrap_or_default())?;
        Ok(Edit::Text(write_kind(self.kind, &raw)))
    }

    fn rename_in_schema(&self, ty: &str, old: &str, new: &str) -> Result<Edit, String> {
        if new == old {
            return Err(format!("{ty}.{old} is already called that"));
        }
        let mut raw = self.raw.clone();
        let refactor = match self.schema.get(ty) {
            Some(TypeDef::Struct { fields, .. }) => {
                if !fields.iter().any(|f| f.name == old) {
                    return Err(format!("no field {old} on {ty}"));
                }
                if !is_identifier(new) || new.starts_with('$') {
                    return Err(format!("{new:?} is not an identifier"));
                }
                if fields.iter().any(|f| f.name == new) {
                    return Err(format!("{ty}.{new} exists"));
                }
                field_mut(&mut raw, ty, old)
                    .ok_or("no field")?
                    .insert("name".into(), Value::String(new.into()));
                Refactor::RenameField { ty: ty.into(), old: old.into(), new: new.into() }
            }
            Some(TypeDef::Enum { values, .. }) => {
                let at = values
                    .iter()
                    .position(|v| v == old)
                    .ok_or_else(|| format!("no value {old} on {ty}"))?;
                if new.is_empty() {
                    return Err("an enum value cannot be empty".into());
                }
                if values.iter().any(|v| v == new) {
                    return Err(format!("{ty}.{new} exists"));
                }
                raw["types"][ty]["values"][at] = Value::String(new.into());
                Refactor::RenameEnumValue { en: ty.into(), old: old.into(), new: new.into() }
            }
            None => return Err(format!("no type {ty}")),
        };
        Schema::from_value(&raw).map_err(|e| summary(&e).unwrap_or_default())?;
        Ok(Edit::Refactor { text: write_kind(self.kind, &raw), refactor })
    }

    /// `:bi add <Type> <field> <type>`, `:bi add <Enum> <value>`,
    /// `:bi add <Name> struct|enum [first value]`.
    pub fn add_to_schema(&self, args: &[&str]) -> Result<Edit, String> {
        if self.kind != Kind::Schema {
            return Err("not a schema (`:bi new` adds an instance)".into());
        }
        let mut raw = self.raw.clone();
        match args {
            [name, kind @ ("struct" | "enum"), rest @ ..] if self.schema.get(name).is_none() => {
                if !is_identifier(name) {
                    return Err(format!("{name:?} is not an identifier"));
                }
                let mut def = Map::new();
                def.insert("kind".into(), Value::String((*kind).into()));
                if *kind == "struct" {
                    def.insert("fields".into(), Value::Array(Vec::new()));
                } else {
                    let first = rest.first().copied().unwrap_or("none");
                    def.insert("values".into(), Value::Array(vec![Value::String(first.into())]));
                }
                raw["types"].as_object_mut().ok_or("no types")?.insert((*name).into(), Value::Object(def));
            }
            [ty, field, type_text] if self.schema.is_struct(ty) => {
                if !is_identifier(field) || field.starts_with('$') {
                    return Err(format!("{field:?} is not an identifier"));
                }
                if self.schema.field(ty, field).is_some() {
                    return Err(format!("{ty}.{field} exists"));
                }
                TypeExpr::parse(type_text)?;
                let mut def = Map::new();
                def.insert("name".into(), Value::String((*field).into()));
                def.insert("type".into(), Value::String((*type_text).into()));
                raw["types"][*ty]["fields"].as_array_mut().ok_or("no fields")?.push(Value::Object(def));
            }
            [ty, value] if self.schema.enum_values(ty).is_some() => {
                if self.schema.enum_values(ty).is_some_and(|v| v.iter().any(|x| x == value)) {
                    return Err(format!("{ty}.{value} exists"));
                }
                raw["types"][*ty]["values"].as_array_mut().ok_or("no values")?.push(Value::String((*value).into()));
            }
            [ty, ..] if self.schema.is_struct(ty) => return Err(format!("add what to {ty}? (`:bi add {ty} <field> <type>`)")),
            [ty, ..] if self.schema.get(ty).is_some() => return Err(format!("add what to {ty}? (`:bi add {ty} <value>`)")),
            [name, ..] if self.schema.get(name).is_none() => return Err(format!("no type {name} (`:bi add {name} struct|enum` makes one)")),
            _ => return Err("add what? (`:bi add <Type> <field> <type>`, `:bi add <Enum> <value>`, `:bi add <Name> struct|enum`)".into()),
        }
        self.checked(raw)
    }

    fn add_schema(&self, key: &str) -> Result<Edit, String> {
        let name = key.strip_prefix("type:").and_then(|k| k.split('/').next());
        Ok(match name {
            Some(name) if self.schema.is_struct(name) => Edit::Prompt(format!("bi add {name} ")),
            Some(name) if self.schema.get(name).is_some() => {
                Edit::Prompt(format!("bi add {name} "))
            }
            _ => Edit::Prompt("bi add ".into()),
        })
    }

    fn delete_schema(&self, key: &str) -> Result<Edit, String> {
        let mut parts = key.split('/');
        let name = parts.next().and_then(|k| k.strip_prefix("type:")).ok_or("nothing to remove")?;
        let mut raw = self.raw.clone();
        match (parts.next(), parts.next()) {
            (None, _) => {
                if let Some(user) = data::type_used(&self.schema, name) {
                    return Err(format!("{name} is used by {user}"));
                }
                raw["types"].as_object_mut().ok_or("no types")?.shift_remove(name);
            }
            (Some("doc"), None) => {
                raw["types"][name].as_object_mut().ok_or("no type")?.shift_remove("doc");
            }
            (Some(v), None) if v.starts_with("value:") => {
                let i: usize = v["value:".len()..].parse().map_err(|_| "bad row")?;
                let values = raw["types"][name]["values"].as_array_mut().ok_or("no values")?;
                if values.len() <= 1 {
                    return Err("an enum keeps one value".into());
                }
                if i >= values.len() {
                    return Err("no such value".into());
                }
                values.remove(i);
            }
            (Some(f), attr) if f.starts_with("field:") => {
                let fname = &f["field:".len()..];
                match attr {
                    None => {
                        let fields =
                            raw["types"][name]["fields"].as_array_mut().ok_or("no fields")?;
                        let at = fields
                            .iter()
                            .position(|x| x.get("name").and_then(Value::as_str) == Some(fname))
                            .ok_or("no field")?;
                        fields.remove(at);
                    }
                    Some("type" | "name") => return Err("a field keeps its name and type".into()),
                    Some(attr) => {
                        let field = field_mut(&mut raw, name, fname).ok_or("no field")?;
                        if field.shift_remove(attr).is_none() {
                            return Err(format!("{attr} is not set"));
                        }
                    }
                }
            }
            _ => return Err("nothing to remove".into()),
        }
        self.checked(raw)
    }

    fn turn_schema(&self, key: &str, steps: i64, space: bool) -> Result<Edit, String> {
        let mut parts = key.split('/');
        let name = parts.next().and_then(|k| k.strip_prefix("type:")).ok_or("nothing to turn")?;
        let fname = parts
            .next()
            .and_then(|f| f.strip_prefix("field:"))
            .ok_or("open a field and turn an attribute")?;
        let attr = parts.next().ok_or("open the field and turn an attribute")?;
        let f = self.schema.field(name, fname).ok_or("no field")?;
        let mut raw = self.raw.clone();
        let value = match attr {
            "default" => {
                let current = f
                    .default
                    .as_ref()
                    .map(|d| self.schema.resolve(&f.ty, d))
                    .unwrap_or_else(|| self.schema.default_of(&f.ty));
                self.turned(&f.ty, Some(f), &current, steps, space)?
            }
            "min" | "max" | "step" => {
                if space {
                    return Err("a number: Ctrl-A and Ctrl-X turn it, Enter edits it".into());
                }
                let current = match attr {
                    "min" => f.min,
                    "max" => f.max,
                    _ => f.step,
                }
                .unwrap_or(0.0);
                let step = if f.ty.is_integer() { 1.0 } else { f.step.unwrap_or(0.1) };
                let scale = 10f64.powi(decimals(step) as i32);
                number(((current + steps as f64 * step) * scale).round() / scale)
            }
            _ => return Err(format!("{attr}: Enter edits it")),
        };
        field_mut(&mut raw, name, fname).ok_or("no field")?.insert(attr.into(), value);
        self.checked(raw)
    }

    /// `:bi migrate`: an older `$dialect` brought up. bi/1 is the first
    /// version, so there is nothing else to rewrite yet.
    pub fn migrate(text: &str, kind: Kind) -> Result<String, String> {
        let mut doc: Value =
            serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
        match check_dialect(&doc) {
            Ok(()) => Err(format!("already {}", super::DIALECT)),
            Err(super::Dialect::Older(_)) => {
                doc["$dialect"] = Value::String(super::DIALECT.into());
                Ok(write_kind(kind, &doc))
            }
            Err(d) => Err(d.message()),
        }
    }
}

fn attr_row(key: &str, depth: usize, label: &str, value: Option<String>) -> Row {
    Row {
        key: key.into(),
        depth,
        label: label.into(),
        value: value.clone().unwrap_or_else(|| "—".into()),
        kind: RowKind::Attr,
        inherited: value.is_none(),
        warning: None,
        doc: None,
        expandable: false,
        expanded: false,
    }
}

/// `.a.b[2].c` → segments.
fn parse_segs(rest: &str) -> Result<Vec<Seg>, String> {
    let mut segs = Vec::new();
    let mut rest = rest;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.') {
            let end = after.find(['.', '[']).unwrap_or(after.len());
            let (name, tail) = after.split_at(end);
            if name.is_empty() {
                return Err("empty field name in path".into());
            }
            segs.push(Seg::Key(name.into()));
            rest = tail;
        } else if let Some(after) = rest.strip_prefix('[') {
            let (n, tail) = after.split_once(']').ok_or("unclosed [ in path")?;
            segs.push(Seg::Index(n.parse().map_err(|_| format!("bad index [{n}]"))?));
            rest = tail;
        } else {
            return Err(format!("bad path at {rest:?}"));
        }
    }
    Ok(segs)
}

/// `inst:3/drops/[1]` → (3, [drops, [1]]); `inst:0/?dmg` → (0, [dmg]).
fn parse_data_key(key: &str) -> Result<(usize, Vec<Seg>), String> {
    let mut parts = key.split('/');
    let index: usize = parts
        .next()
        .and_then(|h| h.strip_prefix("inst:"))
        .and_then(|i| i.parse().ok())
        .ok_or("not a data row")?;
    let mut segs = Vec::new();
    for part in parts {
        if let Some(n) = part.strip_prefix('[').and_then(|p| p.strip_suffix(']')) {
            segs.push(Seg::Index(n.parse().map_err(|_| "bad row")?));
        } else {
            segs.push(Seg::Key(part.trim_start_matches('?').into()));
        }
    }
    Ok((index, segs))
}

/// `new` written at `segs` below a value of type `ty` whose stored form
/// is `stored` and whose meaning is `resolved`: structs on the way are
/// created sparse, lists materialised whole.
fn assign(
    schema: &Schema,
    ty: &TypeExpr,
    stored: Option<&Value>,
    resolved: &Value,
    segs: &[Seg],
    new: Value,
) -> Result<Value, String> {
    let Some(first) = segs.first() else {
        return Ok(schema.sparse(ty, &new));
    };
    let inner = match ty {
        TypeExpr::Optional(inner) => inner.as_ref(),
        other => other,
    };
    match (inner, first) {
        (TypeExpr::Named(name), Seg::Key(k)) => {
            let field = schema.field(name, k).ok_or_else(|| format!("no field {k} on {name}"))?;
            // An absent struct starts from what it means, so setting one
            // key keeps the others at the field's default rather than
            // dropping them to the struct's own.
            let mut map = match stored {
                Some(Value::Object(m)) => m.clone(),
                _ => resolved.as_object().cloned().unwrap_or_default(),
            };
            let child_resolved = resolved.get(k).cloned().unwrap_or(Value::Null);
            let child = assign(schema, &field.ty, map.get(k), &child_resolved, &segs[1..], new)?;
            map.insert(k.clone(), child);
            Ok(Value::Object(map))
        }
        (TypeExpr::List(item), Seg::Index(n)) => {
            let mut items = resolved.as_array().cloned().unwrap_or_default();
            if *n >= items.len() {
                return Err(format!("no item [{n}] (the list has {})", items.len()));
            }
            let stored_item = stored.and_then(Value::as_array).and_then(|s| s.get(*n));
            let child = assign(schema, item, stored_item, &items[*n].clone(), &segs[1..], new)?;
            items[*n] = child;
            Ok(Value::Array(items))
        }
        (t, Seg::Key(k)) => Err(format!("{} has no field {k}", t.text())),
        (t, Seg::Index(n)) => Err(format!("{} has no item [{n}]", t.text())),
    }
}

/// The key or item at the end of `segs` removed; the containers on the
/// way kept.
fn remove_in(
    schema: &Schema,
    ty: &TypeExpr,
    stored: Option<&Value>,
    resolved: &Value,
    segs: &[Seg],
) -> Result<Value, String> {
    let inner = match ty {
        TypeExpr::Optional(inner) => inner.as_ref(),
        other => other,
    };
    match (inner, segs) {
        (TypeExpr::Named(name), [Seg::Key(k)]) => {
            let mut map = match stored {
                Some(Value::Object(m)) => m.clone(),
                _ => return Err(format!("{k} is already the default")),
            };
            if map.shift_remove(k).is_none() {
                return Err(format!("{k} is already the default"));
            }
            let _ = name;
            Ok(Value::Object(map))
        }
        (TypeExpr::List(_), [Seg::Index(n)]) => {
            let mut items = resolved.as_array().cloned().unwrap_or_default();
            if *n >= items.len() {
                return Err(format!("no item [{n}]"));
            }
            items.remove(*n);
            Ok(Value::Array(items))
        }
        (TypeExpr::Named(name), [Seg::Key(k), rest @ ..]) => {
            let field = schema.field(name, k).ok_or_else(|| format!("no field {k} on {name}"))?;
            let mut map = match stored {
                Some(Value::Object(m)) => m.clone(),
                _ => return Err("already the default".into()),
            };
            let child_resolved = resolved.get(k).cloned().unwrap_or(Value::Null);
            let child = remove_in(schema, &field.ty, map.get(k), &child_resolved, rest)?;
            map.insert(k.clone(), child);
            Ok(Value::Object(map))
        }
        (TypeExpr::List(item), [Seg::Index(n), rest @ ..]) => {
            let mut items = resolved.as_array().cloned().unwrap_or_default();
            if *n >= items.len() {
                return Err(format!("no item [{n}]"));
            }
            let stored_item = stored.and_then(Value::as_array).and_then(|s| s.get(*n));
            let child = remove_in(schema, item, stored_item, &items[*n].clone(), rest)?;
            items[*n] = child;
            Ok(Value::Array(items))
        }
        _ => Err("nothing to remove".into()),
    }
}

/// One instance normalised: the sparse form, keys in schema order.
fn normalise_one(item: &Value, schema: &Schema) -> Value {
    let mut doc = Value::Object(Map::new());
    doc["instances"] = Value::Array(vec![item.clone()]);
    data::normalise(&doc, schema)["instances"][0].clone()
}

fn field_mut<'a>(raw: &'a mut Value, ty: &str, field: &str) -> Option<&'a mut Map<String, Value>> {
    raw["types"][ty]["fields"]
        .as_array_mut()?
        .iter_mut()
        .find(|f| f.get("name").and_then(Value::as_str) == Some(field))
        .and_then(Value::as_object_mut)
}

/// How many decimals a step needs, at most six.
fn decimals(step: f64) -> usize {
    let mut d = 0;
    let mut s = step;
    while (s - s.round()).abs() > 1e-9 && d < 6 {
        s *= 10.0;
        d += 1;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{DATA, SCHEMA};
    use super::*;

    fn data_view() -> Props {
        let mut p = Props::new(Kind::Data, BufferId(1), Some(PathBuf::from("/p/level1.bidata")));
        p.load(DATA, Some(Ok(SCHEMA.into())), Index::default()).unwrap();
        p
    }

    fn schema_view() -> Props {
        let mut p = Props::new(Kind::Schema, BufferId(1), Some(PathBuf::from("/p/game.bischema")));
        p.load(SCHEMA, None, Index::default()).unwrap();
        p
    }

    fn text_of(edit: Result<Edit, String>) -> String {
        match edit.unwrap() {
            Edit::Text(t) => t,
            other => panic!("wanted text, got {other:?}"),
        }
    }

    /// The view after an edit: what the editor's sync would show.
    fn after(edit: Result<Edit, String>, p: &mut Props) -> String {
        let text = text_of(edit);
        p.load(&text, Some(Ok(SCHEMA.into())), Index::default()).unwrap();
        text
    }

    fn lines(p: &Props) -> Vec<String> {
        p.rows
            .iter()
            .map(|r| {
                format!(
                    "{}{}{} {}",
                    "  ".repeat(r.depth),
                    if r.inherited { "~" } else { "" },
                    r.label,
                    r.value
                )
                .trim_end()
                .to_string()
            })
            .collect()
    }

    fn open(p: &mut Props, key: &str) {
        p.expanded.insert(key.into());
        p.rebuild();
    }

    #[test]
    fn a_data_file_is_a_tree_of_instances() {
        let mut p = data_view();
        assert_eq!(
            lines(&p),
            [
                "Weapon rusty_sword",
                "Weapon dagger",
                "Weapon warhammer",
                "Enemy goblin",
                "Enemy goblin_chief"
            ]
        );
        open(&mut p, "inst:0");
        assert_eq!(
            lines(&p)[..7],
            [
                "Weapon rusty_sword",
                "  name \"Rusty Sword\"",
                "  damage 12",
                "  ~rarity common",
                "  ~offset { x 0.5, y 0 }",
                "  tags [melee, starter]",
                "  ~two_handed false",
            ]
        );
        open(&mut p, "inst:0/offset");
        assert_eq!(lines(&p)[5..7], ["    ~x 0.5", "    ~y 0"]);
        open(&mut p, "inst:3");
        open(&mut p, "inst:3/drops");
        let goblin = p.rows.iter().position(|r| r.key == "inst:3").unwrap();
        assert_eq!(
            lines(&p)[goblin..goblin + 11],
            [
                "Enemy goblin",
                "  name \"Goblin\"",
                "  hp 40",
                "  speed 1.4",
                "  weapon rusty_sword → Weapon",
                "  drops [rusty_sword, dagger]",
                "    [0] rusty_sword → Weapon",
                "    [1] dagger → Weapon",
                "  spawn { x 12, y 3 }",
                "  leader goblin_chief → Enemy",
                "Enemy goblin_chief",
            ]
        );
        let chief = p.rows.iter().position(|r| r.key == "inst:4").unwrap();
        open(&mut p, "inst:4");
        assert_eq!(lines(&p)[chief + 7], "  ~leader —");
    }

    #[test]
    fn warnings_ride_on_their_rows_and_expansion_survives_a_rebuild() {
        let mut p = data_view();
        let text = DATA
            .replace("\"leader\": \"goblin_chief\"", "\"leader\": \"goblin_king\"")
            .replace("\"hp\": 40,", "\"hp\": 40,\n      \"dmg\": 1,");
        p.load(&text, Some(Ok(SCHEMA.into())), Index::default()).unwrap();
        open(&mut p, "inst:3");
        let leader = p.rows.iter().find(|r| r.key == "inst:3/leader").unwrap();
        assert_eq!(leader.warning.as_deref(), Some("no Enemy goblin_king"));
        let dmg = p.rows.iter().find(|r| r.key == "inst:3/?dmg").unwrap();
        assert_eq!(
            (dmg.kind, dmg.warning.as_deref(), dmg.value.as_str()),
            (RowKind::Unknown, Some("unknown key dmg"), "1")
        );
        assert_eq!(p.warnings(), 2);
        p.select(p.rows.iter().position(|r| r.key == "inst:3/hp").unwrap());
        p.load(&text, Some(Ok(SCHEMA.into())), Index::default()).unwrap();
        assert_eq!(p.selected_row().unwrap().key, "inst:3/hp", "selection by key, expansion kept");
    }

    #[test]
    fn a_broken_file_shows_its_error_as_the_one_row() {
        let mut p = data_view();
        assert_eq!(
            p.load("{", Some(Ok(SCHEMA.into())), Index::default()),
            Err("invalid JSON: EOF while parsing an object at line 1 column 1".into())
        );
        assert_eq!(p.rows.len(), 1);
        assert_eq!(p.rows[0].kind, RowKind::Error);
        assert_eq!(
            p.error.as_deref(),
            Some("invalid JSON: EOF while parsing an object at line 1 column 1")
        );
        assert_eq!(
            p.load(DATA, Some(Err("no schema at game.bischema".into())), Index::default()),
            Err("no schema at game.bischema".into())
        );
        assert_eq!(
            p.load(DATA, Some(Ok("{".into())), Index::default()),
            Err("schema: invalid JSON: EOF while parsing an object at line 1 column 1".into())
        );
        assert!(p.load(DATA, Some(Ok(SCHEMA.into())), Index::default()).is_ok());
        assert_eq!(p.error, None);
    }

    #[test]
    fn set_rewrites_one_value_and_a_default_drops_the_key() {
        let mut p = data_view();
        let text = after(p.set("goblin.hp", "41"), &mut p);
        assert_eq!(text, DATA.replace("\"hp\": 40", "\"hp\": 41"));
        let text = after(p.set("dagger.rarity", "common"), &mut p);
        assert!(!text.contains("\"rarity\": \"rare\""));
        assert_eq!(
            p.set("dagger.rarity", "mythic"),
            Err("dagger.rarity wants one of common, rare, epic".into())
        );
        assert_eq!(p.set("goblin.hp", ""), Err("goblin.hp = 41".into()));
        assert_eq!(p.set("nobody.hp", "1"), Err("no instance nobody".into()));
        assert_eq!(p.set("goblin.mana", "1"), Err("no field mana on Enemy".into()));
        assert_eq!(p.set("goblin", "1"), Err("goblin is an instance; name a field".into()));
        let text = after(p.set("rusty_sword.name", "Iron Sword"), &mut p);
        assert!(text.contains("\"name\": \"Iron Sword\""));
    }

    #[test]
    fn nested_and_list_edits_materialise_what_they_touch() {
        let mut p = data_view();
        let text = after(p.set("rusty_sword.offset.y", "2"), &mut p);
        assert!(
            text.contains("\"offset\": { \"x\": 0.5, \"y\": 2 }"),
            "x keeps the field's default: {text}"
        );
        after(p.set("rusty_sword.offset.y", "0"), &mut p);
        assert!(
            p.data.as_ref().unwrap().instances[0].values.get("offset").is_none(),
            "back to the default, the object goes"
        );
        let text = after(p.set("goblin.drops[1]", "warhammer"), &mut p);
        assert!(text.contains("\"drops\": [\"rusty_sword\", \"warhammer\"]"));
        assert_eq!(
            p.set("goblin.drops[5]", "warhammer"),
            Err("no item [5] (the list has 2)".into())
        );
        let text = after(p.set("goblin.spawn.x", "1"), &mut p);
        assert!(text.contains("\"spawn\": { \"x\": 1, \"y\": 3 }"), "{text}");
        let text = after(p.set("goblin_chief.leader", "goblin"), &mut p);
        assert!(text.contains("\"leader\": \"goblin\""));
        let text = after(p.set("goblin_chief.leader", "none"), &mut p);
        assert!(!text.contains("\"leader\": \"goblin\""));
        let text = after(p.set("dagger.tags", "[\"a\", \"b\"]"), &mut p);
        assert!(text.contains("\"tags\": [\"a\", \"b\"]"));
        after(p.set("dagger.offset", "{\"x\": 0.5}"), &mut p);
        assert!(
            p.data.as_ref().unwrap().instances[1].values.get("offset").is_none(),
            "equal to the default once resolved"
        );
    }

    #[test]
    fn renaming_an_id_is_a_refactoring() {
        let p = data_view();
        let Edit::Refactor { text, refactor } = p.set("rusty_sword.$id", "iron_sword").unwrap()
        else {
            panic!()
        };
        assert_eq!(
            refactor,
            Refactor::RenameId {
                ty: "Weapon".into(),
                old: "rusty_sword".into(),
                new: "iron_sword".into()
            }
        );
        assert_eq!(text.matches("iron_sword").count(), 3);
        assert_eq!(p.set("rusty_sword.$id", "dagger"), Err("Weapon dagger exists".into()));
        assert_eq!(p.set("rusty_sword.$id", "a b"), Err("\"a b\" is not an identifier".into()));
        assert_eq!(p.set("rusty_sword.$id", ""), Err("rusty_sword.$id = rusty_sword".into()));
        let Edit::Refactor { refactor, .. } = p.rename("Enemy.goblin", "gob").unwrap() else {
            panic!()
        };
        assert_eq!(
            refactor,
            Refactor::RenameId { ty: "Enemy".into(), old: "goblin".into(), new: "gob".into() }
        );
    }

    #[test]
    fn space_and_the_nudges_turn_values_by_type() {
        let mut p = data_view();
        let text = after(p.turn("inst:2/two_handed", 1, true), &mut p);
        assert!(!text.contains("two_handed"), "true → false is the default: {text}");
        let text = after(p.turn("inst:0/rarity", 1, true), &mut p);
        assert!(text.contains("\"rarity\": \"rare\""));
        after(p.turn("inst:0/rarity", -1, false), &mut p);
        assert!(
            p.data.as_ref().unwrap().instances[0].values.get("rarity").is_none(),
            "back to the default"
        );
        let text = after(p.turn("inst:3/weapon", 1, true), &mut p);
        assert!(
            text.contains("\"weapon\": \"warhammer\""),
            "ids sorted: dagger, rusty_sword, warhammer: {text}"
        );
        let text = after(p.turn("inst:3/weapon", 1, true), &mut p);
        assert!(text.contains("\"weapon\": \"dagger\""), "wraps: {text}");
        let text = after(p.turn("inst:3/leader", 1, true), &mut p);
        assert!(!text.contains("\"leader\""), "an optional turns off: {text}");
        let text = after(p.turn("inst:3/leader", 1, true), &mut p);
        assert!(text.contains("\"leader\": \"goblin\""), "and on, to the first id: {text}");
        assert_eq!(
            p.turn("inst:3/hp", 1, true),
            Err("a number: Ctrl-A and Ctrl-X turn it, Enter edits it".into())
        );
        let text = after(p.turn("inst:3/hp", 5, false), &mut p);
        assert!(text.contains("\"hp\": 45"));
        let text = after(p.turn("inst:3/speed", 1, false), &mut p);
        assert!(text.contains("\"speed\": 1.5"), "by its step: {text}");
        let text = after(p.turn("inst:3/speed", 100, false), &mut p);
        assert!(text.contains("\"speed\": 10"), "clamped at max: {text}");
        let text = after(p.turn("inst:3/hp", -100, false), &mut p);
        assert!(text.contains("\"hp\": 1"), "clamped at min: {text}");
        let text = after(p.turn("inst:0/offset/x", 1, false), &mut p);
        assert!(text.contains("\"offset\": { \"x\": 0.6 }"), "{text}");
        let text = after(p.turn("inst:3/drops/[0]", 1, false), &mut p);
        assert!(text.contains("\"drops\": [\"warhammer\", \"dagger\"]"), "{text}");
        assert_eq!(p.turn("inst:3", 1, true), Err("an instance; pick a field".into()));
    }

    #[test]
    fn dd_resets_removes_and_deletes() {
        let mut p = data_view();
        let text = after(p.delete("inst:1/rarity"), &mut p);
        assert!(!text.contains("\"rarity\": \"rare\""));
        assert_eq!(p.delete("inst:1/rarity"), Err("rarity is already the default".into()));
        let text = after(p.delete("inst:1/offset/y"), &mut p);
        assert!(text.contains("\"offset\": { \"x\": 0.25 }"), "{text}");
        let text = after(p.delete("inst:3/drops/[0]"), &mut p);
        assert!(text.contains("\"drops\": [\"dagger\"]"));
        let with_unknown = text.replace("\"hp\": 40,", "\"hp\": 40,\n      \"dmg\": 1,");
        p.load(&with_unknown, Some(Ok(SCHEMA.into())), Index::default()).unwrap();
        let text = after(p.delete("inst:3/?dmg"), &mut p);
        assert!(!text.contains("dmg"));
        assert_eq!(
            p.delete("inst:0"),
            Ok(Edit::Prompt("bi delete Weapon rusty_sword  # referenced by Enemy goblin".into()))
        );
        assert_eq!(
            p.delete("inst:2"),
            Ok(Edit::Prompt(
                "bi delete Weapon warhammer  # referenced by Enemy goblin_chief".into()
            ))
        );
        let text = after(p.delete_instance("Weapon", "warhammer"), &mut p);
        assert!(!text.contains("\"$id\": \"warhammer\""));
        let text = after(p.delete_instance("Enemy", "goblin_chief"), &mut p);
        assert!(!text.contains("goblin_chief\","));
        assert_eq!(p.delete_instance("Enemy", "nobody"), Err("no Enemy nobody".into()));
    }

    #[test]
    fn a_adds_items_and_new_adds_instances() {
        let mut p = data_view();
        let text = after(p.add("inst:0/tags"), &mut p);
        assert!(text.contains("\"tags\": [\"melee\", \"starter\", \"\"]"));
        let text = after(p.add("inst:3/drops/[0]"), &mut p);
        assert!(text.contains("\"drops\": [\"rusty_sword\", \"dagger\", \"\"]"));
        assert_eq!(p.add("inst:3"), Ok(Edit::Prompt("bi new ".into())));
        assert_eq!(p.add("inst:3/hp"), Err("not a list (`:bi new` adds an instance)".into()));
        let text = after(p.new_instance("Enemy", "orc"), &mut p);
        assert!(text.ends_with("    {\n      \"$type\": \"Enemy\",\n      \"$id\": \"orc\",\n      \"weapon\": \"\"\n    }\n  ]\n}\n"), "{text}");
        assert_eq!(p.warnings(), 2, "the blank ref and the blank drop");
        assert_eq!(p.new_instance("Enemy", "orc"), Err("Enemy orc exists".into()));
        assert_eq!(p.new_instance("Rarity", "x"), Err("Rarity is an enum".into()));
        assert_eq!(
            p.new_instance("Boss", "x"),
            Err("no type Boss (want Vec2, Weapon, Enemy)".into())
        );
        assert_eq!(p.new_instance("Enemy", "1x"), Err("\"1x\" is not an identifier".into()));
    }

    #[test]
    fn paths_edit_lines_and_targets() {
        let mut p = data_view();
        open(&mut p, "inst:3");
        open(&mut p, "inst:3/drops");
        assert_eq!(p.path_of("inst:3/drops/[1]"), Some("goblin.drops[1]".into()));
        assert_eq!(p.path_of("inst:0"), Some("rusty_sword".into()));
        assert_eq!(p.edit_line("inst:3/hp"), Some("bi set goblin.hp 40".into()));
        assert_eq!(p.edit_line("inst:3/name"), Some("bi set goblin.name Goblin".into()));
        assert_eq!(p.edit_line("inst:3/drops/[1]"), Some("bi set goblin.drops[1] dagger".into()));
        assert_eq!(p.edit_line("inst:3/drops"), None, "a list opens");
        assert_eq!(p.edit_line("inst:3"), None);
        assert_eq!(p.target("inst:3/weapon"), Ok(("Weapon".into(), "rusty_sword".into())));
        assert_eq!(p.target("inst:3/drops/[1]"), Ok(("Weapon".into(), "dagger".into())));
        assert_eq!(p.target("inst:3/leader"), Ok(("Enemy".into(), "goblin_chief".into())));
        assert_eq!(p.target("inst:3/hp"), Err("not a ref".into()));
        assert!(p.select_instance("Enemy", "goblin_chief"));
        assert_eq!(p.selected_row().unwrap().key, "inst:4");
        assert!(p.selected_row().unwrap().expanded);
        // Two types sharing an id spell the type.
        let shared = DATA
            .replace("\"$id\": \"goblin\"", "\"$id\": \"dagger\"")
            .replace("\"leader\": \"goblin_chief\"", "\"leader\": \"goblin_chief\"");
        p.load(&shared, Some(Ok(SCHEMA.into())), Index::default()).unwrap();
        assert_eq!(p.path_of("inst:3/hp"), Some("Enemy:dagger.hp".into()));
        assert_eq!(
            p.set("dagger.hp", "1"),
            Err("dagger is ambiguous (want Weapon:dagger or Enemy:dagger)".into())
        );
        assert!(matches!(p.set("Enemy:dagger.hp", "1"), Ok(Edit::Text(_))));
    }

    #[test]
    fn a_schema_is_a_tree_of_types() {
        let mut p = schema_view();
        assert_eq!(lines(&p), ["Rarity enum", "Vec2 struct", "Weapon struct", "Enemy struct"]);
        open(&mut p, "type:Rarity");
        assert_eq!(
            lines(&p)[1..5],
            ["  doc \"Drop tier, drives colour and loot tables\"", "  common", "  rare", "  epic"]
        );
        open(&mut p, "type:Weapon");
        open(&mut p, "type:Weapon/field:damage");
        let weapon = p.rows.iter().position(|r| r.key == "type:Weapon").unwrap();
        assert_eq!(
            lines(&p)[weapon..weapon + 10],
            [
                "Weapon struct",
                "  ~doc —",
                "  name string",
                "  damage i32 = 10  0..999",
                "    type i32",
                "    default 10",
                "    min 0",
                "    max 999",
                "    ~step —",
                "    ~doc —",
            ]
        );
        let enemy = p.rows.iter().position(|r| r.key == "type:Enemy").unwrap();
        open(&mut p, "type:Enemy");
        assert_eq!(lines(&p)[enemy + 4], "  speed f32 = 1.0  0..10  step 0.1");
        assert_eq!(lines(&p)[enemy + 5], "  weapon ref<Weapon>");
        assert_eq!(
            p.path_of("type:Weapon/field:damage/default"),
            Some("Weapon.damage.default".into())
        );
        assert_eq!(p.path_of("type:Rarity/value:1"), Some("Rarity.values[1]".into()));
        assert_eq!(
            p.edit_line("type:Weapon/field:damage/default"),
            Some("bi set Weapon.damage.default 10".into())
        );
        assert_eq!(
            p.edit_line("type:Weapon/field:damage/step"),
            Some("bi set Weapon.damage.step -".into())
        );
        assert_eq!(p.edit_line("type:Rarity/value:1"), Some("bi set Rarity.values[1] rare".into()));
        assert_eq!(p.edit_line("type:Weapon/field:damage"), None);
    }

    #[test]
    fn schema_edits_are_checked_and_renames_refactor() {
        let mut p = schema_view();
        let text = text_of(p.set("Weapon.damage.default", "20"));
        assert!(text.contains("\"default\": 20, \"min\": 0"));
        p.load(&text, None, Index::default()).unwrap();
        assert_eq!(
            p.set("Weapon.damage.default", "lots"),
            Err("Weapon.damage.default wants a whole number".into())
        );
        let text = text_of(p.set("Weapon.damage.step", "5"));
        assert!(text.contains("\"max\": 999, \"step\": 5 }"));
        let text = text_of(p.set("Weapon.damage.max", "-"));
        assert!(!text.contains("\"max\": 999"));
        assert_eq!(
            p.set("Weapon.name.min", "3"),
            Err("Weapon.name: min/max/step on a string".into())
        );
        assert_eq!(
            p.set("Weapon.damage.type", "Boss"),
            Err("Weapon.damage: unknown type Boss".into())
        );
        let text = text_of(p.set("Weapon.damage.type", "i64"));
        assert!(text.contains("\"type\": \"i64\""));
        let text = text_of(p.set("Weapon.doc", "A thing to hit with"));
        assert!(text.contains("\"doc\": \"A thing to hit with\"\n"));
        let text = text_of(p.set("Rarity.doc", "-"));
        assert!(!text.contains("Drop tier"));
        assert_eq!(p.set("Weapon.damage.default", ""), Err("Weapon.damage.default = 20".into()));
        let Edit::Refactor { text, refactor } = p.set("Weapon.damage.name", "dmg").unwrap() else {
            panic!()
        };
        assert_eq!(
            refactor,
            Refactor::RenameField { ty: "Weapon".into(), old: "damage".into(), new: "dmg".into() }
        );
        assert!(text.contains("{ \"name\": \"dmg\", \"type\": \"i32\""));
        let Edit::Refactor { refactor, .. } = p.rename("Rarity.rare", "uncommon").unwrap() else {
            panic!()
        };
        assert_eq!(
            refactor,
            Refactor::RenameEnumValue {
                en: "Rarity".into(),
                old: "rare".into(),
                new: "uncommon".into()
            }
        );
        assert_eq!(p.rename("Rarity.rare", "epic"), Err("Rarity.epic exists".into()));
        assert_eq!(p.rename("Weapon.damage", "name"), Err("Weapon.name exists".into()));
        assert_eq!(p.rename("Weapon.nothing", "x"), Err("no field nothing on Weapon".into()));
        assert_eq!(p.rename("Boss.x", "y"), Err("no type Boss".into()));
        let text = text_of(p.turn("type:Weapon/field:two_handed/default", 1, true));
        assert!(text.contains("\"default\": true"));
        let text = text_of(p.turn("type:Weapon/field:damage/max", 1, false));
        assert!(text.contains("\"max\": 1000"));
    }

    #[test]
    fn schema_add_and_delete() {
        let mut p = schema_view();
        let text = text_of(p.add_to_schema(&["Weapon", "speed", "f32"]));
        assert!(text.contains("{ \"name\": \"two_handed\", \"type\": \"bool\", \"default\": false },\n        { \"name\": \"speed\", \"type\": \"f32\" }"));
        assert_eq!(
            p.add_to_schema(&["Weapon", "damage", "f32"]),
            Err("Weapon.damage exists".into())
        );
        assert_eq!(
            p.add_to_schema(&["Weapon", "x", "Boss"]),
            Err("Weapon.x: unknown type Boss".into())
        );
        let text = text_of(p.add_to_schema(&["Rarity", "mythic"]));
        assert!(text.contains("[\"common\", \"rare\", \"epic\", \"mythic\"]"));
        let text = text_of(p.add_to_schema(&["Color", "struct"]));
        assert!(
            text.contains("\"Color\": {\n      \"kind\": \"struct\",\n      \"fields\": []\n    }")
        );
        let text = text_of(p.add_to_schema(&["Size", "enum", "small"]));
        assert!(text.contains("\"values\": [\"small\"]"));
        assert_eq!(
            p.add_to_schema(&["Weapon"]),
            Err("add what to Weapon? (`:bi add Weapon <field> <type>`)".into())
        );
        assert_eq!(p.add("type:Weapon"), Ok(Edit::Prompt("bi add Weapon ".into())));
        assert_eq!(p.add("nothing"), Ok(Edit::Prompt("bi add ".into())));
        assert_eq!(p.delete("type:Vec2"), Err("Vec2 is used by Weapon.offset".into()));
        let text = text_of(p.delete("type:Weapon/field:tags"));
        assert!(!text.contains("tags"));
        let text = text_of(p.delete("type:Weapon/field:damage/max"));
        assert!(!text.contains("\"max\": 999"));
        assert_eq!(p.delete("type:Weapon/field:damage/step"), Err("step is not set".into()));
        assert_eq!(
            p.delete("type:Weapon/field:damage/type"),
            Err("a field keeps its name and type".into())
        );
        let text = text_of(p.delete("type:Rarity/value:2"));
        assert!(text.contains("[\"common\", \"rare\"]"));
        p.load(&text.replace("[\"common\", \"rare\"]", "[\"common\"]"), None, Index::default())
            .unwrap();
        assert_eq!(p.delete("type:Rarity/value:0"), Err("an enum keeps one value".into()));
        let text = text_of(p.add_to_schema(&["Loose", "struct"]));
        p.load(&text, None, Index::default()).unwrap();
        let text = text_of(p.delete("type:Loose"));
        assert!(!text.contains("Loose"));
    }

    #[test]
    fn migrate_and_prune() {
        assert_eq!(Props::migrate(DATA, Kind::Data), Err("already bi/1".into()));
        let old = DATA.replace("bi/1", "bi/0");
        assert_eq!(Props::migrate(&old, Kind::Data), Ok(DATA.to_string()));
        assert_eq!(Props::migrate("{}", Kind::Data), Err("no $dialect (want \"bi/1\")".into()));
        let mut p = data_view();
        let with_unknown = DATA.replace("\"hp\": 40,", "\"hp\": 40,\n      \"dmg\": 1,");
        p.load(&with_unknown, Some(Ok(SCHEMA.into())), Index::default()).unwrap();
        let (edit, n) = p.prune().unwrap();
        assert_eq!((edit, n), (Edit::Text(DATA.into()), 1));
        let written = p.normalised_text();
        assert!(
            written.contains("\"leader\": \"goblin_chief\",\n      \"dmg\": 1\n"),
            "unknown keys survive :w, after the fields: {written}"
        );
    }

    #[test]
    fn selection_walks_and_collapse_climbs() {
        let mut p = data_view();
        p.select(3);
        assert!(p.expand());
        assert_eq!(p.rows[3].expanded, true);
        p.select_by(2);
        assert_eq!(p.selected_row().unwrap().key, "inst:3/hp");
        p.collapse();
        assert_eq!(p.selected_row().unwrap().key, "inst:3", "to the parent");
        p.collapse();
        assert!(!p.rows[3].expanded, "then closed");
        p.select_by(-100);
        assert_eq!(p.selected, 0);
        assert!(p.toggle_expand());
        assert!(p.rows[0].expanded);
        p.select_by(1);
        assert!(!p.expand(), "a string has nothing to open");
    }
}
