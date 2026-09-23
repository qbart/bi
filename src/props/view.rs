//! The property view: rows over a schema or a data file, and every edit
//! the keys and `:bi` make, each returning the buffer's new text. A data
//! file's rows are widgets — sliders, checks, choices — laid out as the
//! schema asks; a schema's rows are a tree. See `docs/specs/props.md`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::{Map, Value};

use super::data::{self, DataFile, Index};
use super::schema::{
    Cond, FIELD_ATTRS, FieldDef, Schema, TypeDef, TypeExpr, Widget, compact, number, range_text,
    unquote,
};
use super::{Diagnostic, Kind, Level, check_dialect, is_identifier, summary, write_kind};
use crate::buffer::BufferId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// `Weapon rusty_sword`.
    Instance,
    /// A section of fields the schema grouped: `Stats`.
    Group,
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

/// How a row asks to be drawn. The value text is always there; the
/// widget says what to draw beside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RowWidget {
    /// Label and value, as a tree row.
    Plain,
    /// A section title: an instance.
    Header,
    /// A group's title.
    Group,
    /// A struct or a list: opens.
    Fold,
    /// A number with a range: a bar this far along.
    Slider(f32),
    /// A bool.
    Check(bool),
    /// An enum, a ref, an optional: `‹ value ›`.
    Choice,
    /// An enum as every value, the current one marked.
    Toggle,
    /// A string.
    Text,
    /// A colour: a brick painted these bytes, `r g b a`.
    Color([u8; 4]),
    /// A curve: sixteen samples across `0..1`, each `0..=7` of the way up.
    Curve([u8; 16]),
    /// A gradient: sixteen colours sampled across `0..1`.
    Gradient([[u8; 4]; 16]),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Stable across rebuilds: `inst:3/drops/[1]`, `inst:0/@Stats`,
    /// `type:Weapon/field:damage/default`.
    pub key: String,
    pub depth: usize,
    pub label: String,
    pub value: String,
    pub kind: RowKind,
    pub widget: RowWidget,
    /// The value is the default, not stored: drawn dim.
    pub inherited: bool,
    /// The schema said so: turned and set nothing, drawn muted.
    pub readonly: bool,
    /// `h` and `l` turn this row's value rather than open or close it.
    pub turnable: bool,
    pub warning: Option<String>,
    pub doc: Option<String>,
    pub expandable: bool,
    pub expanded: bool,
}

impl Row {
    fn new(key: &str, depth: usize, label: &str, kind: RowKind) -> Row {
        Row {
            key: key.into(),
            depth,
            label: label.into(),
            value: String::new(),
            kind,
            widget: RowWidget::Plain,
            inherited: false,
            readonly: false,
            turnable: false,
            warning: None,
            doc: None,
            expandable: false,
            expanded: false,
        }
    }
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
    RenameType { old: String, new: String },
}

impl Refactor {
    /// The rewrite applied to one data document. `schema` is the schema
    /// as it was before the change, which is what the data still spells.
    pub fn apply(&self, doc: &mut Value, schema: &Schema) -> bool {
        match self {
            Refactor::RenameId { ty, old, new } => data::rename_id(doc, schema, ty, old, new),
            Refactor::RenameField { ty, old, new } => data::rename_field(doc, schema, ty, old, new),
            Refactor::RenameEnumValue { en, old, new } => {
                data::rename_enum_value(doc, schema, en, old, new)
            }
            Refactor::RenameType { old, new } => data::rename_type(doc, old, new),
        }
    }
}

/// Which editor `Enter` opens over a row's value. See
/// `docs/specs/props.md` §Colours, curves and gradients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Color,
    Curve,
    Gradient,
}

impl ToolKind {
    pub fn name(self) -> &'static str {
        match self {
            ToolKind::Color => "colour",
            ToolKind::Curve => "curve",
            ToolKind::Gradient => "gradient",
        }
    }
}

/// One path segment below an instance or a type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    Index(usize),
}

/// What `y` took from a row: a value and the type it has, for `p` to
/// check against the row it lands on. See `docs/specs/props.md`.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub ty: TypeExpr,
    pub value: Value,
}

/// A field or a group inside a struct's layout.
enum Item {
    Field(usize),
    Group(GroupNode),
}

struct GroupNode {
    name: String,
    key: String,
    items: Vec<Item>,
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
    /// Rows opened by hand; a group that starts collapsed is in here when
    /// opened.
    expanded: BTreeSet<String>,
    /// Groups closed by hand, of the ones that start open.
    collapsed: BTreeSet<String>,
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
            collapsed: BTreeSet::new(),
            error: None,
            seen: None,
            schema_seen: None,
        }
    }

    /// The text a fresh file of `kind` starts with: the header and the
    /// empty container, `$schema` as given.
    pub fn skeleton(kind: Kind, schema: Option<&str>) -> String {
        let mut doc = Map::new();
        doc.insert("$dialect".into(), Value::String(super::DIALECT.into()));
        match kind {
            Kind::Schema => {
                doc.insert("types".into(), Value::Object(Map::new()));
            }
            Kind::Data => {
                doc.insert("$schema".into(), Value::String(schema.unwrap_or("").into()));
                doc.insert("instances".into(), Value::Array(Vec::new()));
            }
        }
        write_kind(kind, &Value::Object(doc))
    }

    /// `:bi schema <path>` on any parseable data text: `$schema` set,
    /// second key of the file.
    pub fn with_schema_ref(text: &str, rel: &str) -> Result<String, String> {
        let doc: Value = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
        let Value::Object(map) = doc else { return Err("not a JSON object".into()) };
        let mut out = Map::new();
        if let Some(d) = map.get("$dialect") {
            out.insert("$dialect".into(), d.clone());
        }
        out.insert("$schema".into(), Value::String(rel.into()));
        for (k, v) in map {
            if k != "$dialect" && k != "$schema" {
                out.insert(k, v);
            }
        }
        Ok(write_kind(Kind::Data, &Value::Object(out)))
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
                self.rows = vec![Row::new("error", 0, &message, RowKind::Error)];
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

    /// The first instance opened, for a view that just came up.
    pub fn expand_first(&mut self) {
        if self.kind == Kind::Data && self.rows.first().is_some_and(|r| r.kind == RowKind::Instance)
        {
            self.expanded.insert("inst:0".into());
            self.rebuild();
        }
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
            let mut row = Row::new(&key, 0, &format!("{} {}", inst.ty, inst.id), RowKind::Instance);
            row.widget = RowWidget::Header;
            row.doc = self.schema.get(&inst.ty).and_then(TypeDef::doc).map(str::to_string);
            row.expandable = true;
            row.expanded = expanded;
            rows.push(row);
            if !expanded {
                continue;
            }
            let Some(def) = self.schema.get(&inst.ty) else { continue };
            let stored = Value::Object(inst.values.clone());
            let resolved = self.schema.resolve(&TypeExpr::Named(inst.ty.clone()), &stored);
            self.struct_rows(rows, &key, 1, def, &resolved, Some(&stored), false, false);
        }
    }

    /// The fields of a struct value, laid out as the schema asks: sorted
    /// by `order`, under their groups, hidden by their conditions, then
    /// the unknown keys.
    #[allow(clippy::too_many_arguments)]
    fn struct_rows(
        &self,
        rows: &mut Vec<Row>,
        key: &str,
        depth: usize,
        def: &TypeDef,
        resolved: &Value,
        stored: Option<&Value>,
        inherited: bool,
        readonly: bool,
    ) {
        let fields = def.fields();
        let Some(map) = resolved.as_object() else { return };
        let stored_map = stored.and_then(Value::as_object);
        let mut order: Vec<usize> = (0..fields.len()).collect();
        order.sort_by(|&a, &b| {
            let (oa, ob) = (fields[a].order.unwrap_or(0.0), fields[b].order.unwrap_or(0.0));
            oa.partial_cmp(&ob).unwrap_or(std::cmp::Ordering::Equal)
        });
        // The layout tree: groups in order of first appearance.
        let mut root = GroupNode { name: String::new(), key: key.into(), items: Vec::new() };
        for index in order {
            let field = &fields[index];
            let mut node = &mut root;
            if let Some(group) = &field.group {
                for part in group.split('/') {
                    let gkey = format!("{}/@{part}", node.key);
                    let at = node
                        .items
                        .iter()
                        .position(|item| matches!(item, Item::Group(g) if g.name == part));
                    let at = match at {
                        Some(at) => at,
                        None => {
                            node.items.push(Item::Group(GroupNode {
                                name: part.into(),
                                key: gkey,
                                items: Vec::new(),
                            }));
                            node.items.len() - 1
                        }
                    };
                    node = match &mut node.items[at] {
                        Item::Group(g) => g,
                        Item::Field(_) => unreachable!("found by the group match"),
                    };
                }
            }
            node.items.push(Item::Field(index));
        }
        self.layout_rows(rows, &root, depth, def, map, stored_map, inherited, readonly);
        for (k, v) in map {
            if !fields.iter().any(|f| &f.name == k) {
                let unknown_key =
                    if depth == 1 { format!("{key}/?{k}") } else { format!("{key}/{k}") };
                let mut row = Row::new(&unknown_key, depth, k, RowKind::Unknown);
                row.value = v.to_string();
                row.inherited = inherited;
                rows.push(row);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_rows(
        &self,
        rows: &mut Vec<Row>,
        node: &GroupNode,
        depth: usize,
        def: &TypeDef,
        map: &Map<String, Value>,
        stored: Option<&Map<String, Value>>,
        inherited: bool,
        readonly: bool,
    ) {
        for item in &node.items {
            match item {
                Item::Group(group) => {
                    let options = def.group(&group.name).cloned().unwrap_or_default();
                    let open = self.group_open(&group.key, options.collapsed);
                    let mut row = Row::new(&group.key, depth, &group.name, RowKind::Group);
                    row.widget = RowWidget::Group;
                    row.doc = options.doc.clone();
                    row.expandable = true;
                    row.expanded = open;
                    rows.push(row);
                    if open {
                        self.layout_rows(
                            rows,
                            group,
                            depth + 1,
                            def,
                            map,
                            stored,
                            inherited,
                            readonly,
                        );
                    }
                }
                Item::Field(index) => {
                    let field = &def.fields()[*index];
                    if !field.shown(map) {
                        continue;
                    }
                    let Some(value) = map.get(&field.name) else { continue };
                    let sub_stored = stored.and_then(|m| m.get(&field.name));
                    // Groups are layout: a field's key is its data path.
                    let base = node.key.split("/@").next().unwrap_or(&node.key);
                    self.value_rows(
                        rows,
                        &format!("{base}/{}", field.name),
                        depth,
                        field.label(),
                        &field.ty,
                        value,
                        sub_stored,
                        inherited || sub_stored.is_none(),
                        readonly || field.readonly,
                        Some(field),
                        RowKind::Field,
                    );
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
        readonly: bool,
        field: Option<&FieldDef>,
        kind: RowKind,
    ) {
        let inner = match ty {
            TypeExpr::Optional(inner) if !resolved.is_null() => inner.as_ref(),
            other => other,
        };
        let inline = field.is_some_and(|f| f.widget == Some(Widget::Inline));
        let expandable = match (inner, resolved) {
            (TypeExpr::Named(n), Value::Object(_)) => {
                self.schema.fields_of(n).is_some_and(|f| !f.is_empty())
            }
            (TypeExpr::List(_), Value::Array(items)) => !items.is_empty(),
            _ => false,
        };
        let expanded = expandable && (inline || self.expanded.contains(key));
        let mut row = Row::new(key, depth, label, kind);
        row.value = self.value_text(ty, resolved);
        row.widget = self.widget_of(inner, resolved, field, expandable);
        if row.widget == RowWidget::Toggle
            && let (TypeExpr::Named(n), Some(current)) = (inner, resolved.as_str())
            && let Some(values) = self.schema.enum_values(n)
        {
            let parts: Vec<String> = values
                .iter()
                .map(|v| if v == current { format!("[{v}]") } else { v.clone() })
                .collect();
            row.value = parts.join(" ");
        }
        row.inherited = inherited;
        row.readonly = readonly;
        row.turnable = !expandable && turnable(inner, resolved);
        row.doc = field.and_then(|f| f.doc.clone());
        row.expandable = expandable && !inline;
        row.expanded = expanded;
        rows.push(row);
        if !expanded {
            return;
        }
        match (inner, resolved) {
            (TypeExpr::Named(n), Value::Object(_)) => {
                let Some(def) = self.schema.get(n) else { return };
                self.struct_rows(rows, key, depth + 1, def, resolved, stored, inherited, readonly);
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
                        readonly,
                        None,
                        RowKind::Item,
                    );
                }
            }
            _ => {}
        }
    }

    /// The widget a value draws as.
    fn widget_of(
        &self,
        ty: &TypeExpr,
        value: &Value,
        field: Option<&FieldDef>,
        expandable: bool,
    ) -> RowWidget {
        if expandable {
            return RowWidget::Fold;
        }
        match ty {
            TypeExpr::Bool => RowWidget::Check(value.as_bool().unwrap_or(false)),
            TypeExpr::Int(_) | TypeExpr::F32 | TypeExpr::F64 => {
                match field.and_then(|f| f.min.zip(f.max)) {
                    Some((min, max)) if max > min => {
                        let n = value.as_f64().unwrap_or(min);
                        RowWidget::Slider(((n - min) / (max - min)).clamp(0.0, 1.0) as f32)
                    }
                    _ => RowWidget::Plain,
                }
            }
            TypeExpr::Str => RowWidget::Text,
            TypeExpr::Rgb | TypeExpr::Rgba => {
                let alpha = *ty == TypeExpr::Rgba;
                match value.as_str().and_then(|s| super::color::parse(s, alpha)) {
                    Some(c) => RowWidget::Color(c),
                    None => RowWidget::Plain,
                }
            }
            TypeExpr::Curve => match super::schema::curve_of(value) {
                Ok(curve) => RowWidget::Curve(sparkline(&curve)),
                Err(_) => RowWidget::Plain,
            },
            TypeExpr::Gradient => match super::schema::gradient_of(value) {
                Ok(g) => {
                    let mut cells = [[0u8; 4]; 16];
                    for (i, cell) in cells.iter_mut().enumerate() {
                        *cell = crate::gradient::eval(&g, (i as f32 + 0.5) / 16.0);
                    }
                    RowWidget::Gradient(cells)
                }
                Err(_) => RowWidget::Plain,
            },
            TypeExpr::Named(n) if self.schema.enum_values(n).is_some() => {
                if field.is_some_and(|f| f.widget == Some(Widget::Toggle)) {
                    RowWidget::Toggle
                } else {
                    RowWidget::Choice
                }
            }
            TypeExpr::Ref(_) => RowWidget::Choice,
            TypeExpr::Optional(_) => RowWidget::Choice,
            _ => RowWidget::Plain,
        }
    }

    fn group_open(&self, key: &str, starts_collapsed: bool) -> bool {
        if starts_collapsed { self.expanded.contains(key) } else { !self.collapsed.contains(key) }
    }

    fn schema_rows(&self, rows: &mut Vec<Row>) {
        for (name, def) in &self.schema.types {
            let key = format!("type:{name}");
            let expanded = self.expanded.contains(&key);
            let mut row = Row::new(&key, 0, name, RowKind::Type);
            row.value = def.kind().into();
            row.doc = def.doc().map(str::to_string);
            row.expandable = true;
            row.expanded = expanded;
            rows.push(row);
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
                        let mut row = Row::new(&fkey, 1, &f.name, RowKind::FieldDef);
                        row.value = self.field_def_text(f);
                        row.doc = f.doc.clone();
                        row.expandable = true;
                        row.expanded = fexpanded;
                        rows.push(row);
                        if !fexpanded {
                            continue;
                        }
                        for attr in FIELD_ATTRS.iter().filter(|a| **a != "name") {
                            let value = self.attr_value(f, attr);
                            let mut row = attr_row(&format!("{fkey}/{attr}"), 2, attr, value);
                            row.turnable = match *attr {
                                "type" | "min" | "max" | "step" | "order" | "readonly"
                                | "widget" => true,
                                "default" => turnable(&f.ty, &self.schema.field_default(f)),
                                _ => false,
                            };
                            rows.push(row);
                        }
                    }
                }
                TypeDef::Enum { values, .. } => {
                    for (i, v) in values.iter().enumerate() {
                        rows.push(Row::new(&format!("{key}/value:{i}"), 1, v, RowKind::EnumValue));
                    }
                }
            }
        }
    }

    /// A field attribute as its row shows it; `None` when unset.
    fn attr_value(&self, f: &FieldDef, attr: &str) -> Option<String> {
        match attr {
            "type" => Some(f.ty.text()),
            "default" => {
                f.default.as_ref().map(|d| self.value_text(&f.ty, &self.schema.resolve(&f.ty, d)))
            }
            "min" => f.min.map(compact),
            "max" => f.max.map(compact),
            "step" => f.step.map(compact),
            "doc" => f.doc.as_ref().map(|d| format!("{d:?}")),
            "group" => f.group.clone(),
            "order" => f.order.map(compact),
            "label" => f.label.as_ref().map(|d| format!("{d:?}")),
            "readonly" => f.readonly.then(|| "true".into()),
            "show_if" => f.show_if.as_ref().map(Cond::text),
            "hide_if" => f.hide_if.as_ref().map(Cond::text),
            "widget" => f.widget.map(|w| w.text().into()),
            _ => None,
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
        if let Some(group) = &f.group {
            out.push_str(&format!("  @{group}"));
        }
        if f.readonly {
            out.push_str("  readonly");
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
            (TypeExpr::Rgb, Value::String(s)) | (TypeExpr::Rgba, Value::String(s)) => s.clone(),
            (TypeExpr::Curve, Value::Array(items)) => match items.len() {
                1 => "1 point".into(),
                n => format!("{n} points"),
            },
            (TypeExpr::Gradient, Value::Array(items)) => match items.len() {
                1 => "1 stop".into(),
                n => format!("{n} stops"),
            },
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

    /// A value as the ex line takes it back — `Enter`'s prefill. An
    /// absent value prefills nothing, so what is typed is the value.
    fn edit_text(&self, ty: &TypeExpr, value: &Value) -> String {
        match (ty, value) {
            (_, Value::Null) => String::new(),
            (TypeExpr::Optional(inner), v) => self.edit_text(inner, v),
            (TypeExpr::Rgb, Value::String(s)) | (TypeExpr::Rgba, Value::String(s)) => s.clone(),
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

    /// The row with `key` selected, when there is one.
    pub fn select_key(&mut self, key: &str) -> bool {
        match self.rows.iter().position(|r| r.key == key) {
            Some(at) => {
                self.selected = at;
                true
            }
            None => false,
        }
    }

    /// Every ancestor of `key` opened and the row selected — where a
    /// thing just added lands.
    pub fn reveal(&mut self, key: &str) -> bool {
        let mut prefix = String::new();
        for (i, part) in key.split('/').enumerate() {
            if i > 0 {
                prefix.push('/');
            }
            prefix.push_str(part);
            if prefix != key {
                self.expanded.insert(prefix.clone());
            }
        }
        self.rebuild();
        self.select_key(key)
    }

    /// `Tab` / `Shift-Tab`: the next or previous instance or type row.
    pub fn next_section(&mut self, back: bool) {
        let is_section = |r: &Row| matches!(r.kind, RowKind::Instance | RowKind::Type);
        let found = if back {
            self.rows[..self.selected].iter().rposition(is_section)
        } else {
            self.rows[self.selected + 1..]
                .iter()
                .position(is_section)
                .map(|i| i + self.selected + 1)
        };
        if let Some(at) = found {
            self.selected = at;
        }
    }

    fn set_open(&mut self, key: &str, kind: RowKind, open: bool) {
        if kind == RowKind::Group {
            let starts_collapsed = self.group_starts_collapsed(key);
            if starts_collapsed {
                if open {
                    self.expanded.insert(key.into())
                } else {
                    self.expanded.remove(key)
                };
            } else if open {
                self.collapsed.remove(key);
            } else {
                self.collapsed.insert(key.into());
            }
        } else if open {
            self.expanded.insert(key.into());
        } else {
            self.expanded.remove(key);
        }
    }

    /// Whether the schema said a group row starts closed.
    fn group_starts_collapsed(&self, key: &str) -> bool {
        let Some(data) = &self.data else { return false };
        let Ok((index, segs)) = parse_data_key(key.split("/@").next().unwrap_or(key)) else {
            return false;
        };
        let Some(inst) = data.instances.get(index) else { return false };
        let ty = match self.walk(&inst.ty, &segs) {
            Ok((TypeExpr::Named(n), ..)) => n,
            Ok((TypeExpr::Optional(inner), ..)) => match *inner {
                TypeExpr::Named(n) => n,
                _ => return false,
            },
            _ => return false,
        };
        let group = key.rsplit("/@").next().unwrap_or("");
        self.schema.get(&ty).and_then(|d| d.group(group)).is_some_and(|g| g.collapsed)
    }

    /// `l`: open the row. False when it has nothing to open.
    pub fn expand(&mut self) -> bool {
        let Some(row) = self.rows.get(self.selected) else { return false };
        if !row.expandable {
            return false;
        }
        if !row.expanded {
            let (key, kind) = (row.key.clone(), row.kind);
            self.set_open(&key, kind, true);
            self.rebuild();
        }
        true
    }

    /// `h`: close the row, or go to its parent.
    pub fn collapse(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        if row.expanded && row.expandable {
            let (key, kind) = (row.key.clone(), row.kind);
            self.set_open(&key, kind, false);
            self.rebuild();
            return;
        }
        self.parent();
    }

    /// `Backspace`: the parent row.
    pub fn parent(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
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
        let (key, kind, open) = (row.key.clone(), row.kind, !row.expanded);
        self.set_open(&key, kind, open);
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
        self.select_key(&key)
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
                if seg.starts_with('@') {
                    continue;
                }
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

    /// The type at the end of a segment chain from a struct, the field
    /// definition it belongs to when it is a field, and whether anything
    /// on the way was read-only.
    fn walk<'a>(
        &'a self,
        ty: &str,
        segs: &[Seg],
    ) -> Result<(TypeExpr, Option<&'a FieldDef>, bool), String> {
        let mut current = TypeExpr::Named(ty.into());
        let mut field = None;
        let mut readonly = false;
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
                    readonly |= f.readonly;
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
        Ok((current, field, readonly))
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
        let (ty, _, readonly) = self.walk(&inst.ty, &segs)?;
        if text.is_empty() {
            let (resolved, _) = self.resolved_at(index, &segs)?;
            return Err(format!("{path} = {}", self.edit_text(&ty, &resolved)));
        }
        if readonly {
            return Err(format!("{path} is read-only"));
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

    /// `y` on a row: the value there, typed, and its spelling for the
    /// register ring. An instance header takes the whole instance.
    pub fn yank(&self, key: &str) -> Result<(Clip, String), String> {
        if self.kind != Kind::Data {
            return Err("nothing typed to yank here".into());
        }
        if key.contains("/@") {
            return Err("a group: yank its fields one by one".into());
        }
        let (index, segs) = parse_data_key(key)?;
        let inst = self
            .data
            .as_ref()
            .ok_or("not a data file")?
            .instances
            .get(index)
            .ok_or("no instance")?;
        if segs.is_empty() {
            let clip = Clip {
                ty: TypeExpr::Named(inst.ty.clone()),
                value: Value::Object(inst.values.clone()),
            };
            return Ok((clip, format!("{} {}", inst.ty, inst.id)));
        }
        let (ty, ..) = self.walk(&inst.ty, &segs)?;
        let (value, _) = self.resolved_at(index, &segs)?;
        let text = self.edit_text(&ty, &value);
        Ok((Clip { ty, value }, text))
    }

    /// `p` on a row: `clip` written there when the types agree — the
    /// row's, or an optional of it — as one edit.
    pub fn paste(&self, key: &str, clip: &Clip) -> Result<Edit, String> {
        if self.kind != Kind::Data {
            return Err("nothing to paste onto here".into());
        }
        if key.contains("/@") {
            return Err("a group: paste onto its fields".into());
        }
        let (index, segs) = parse_data_key(key)?;
        let inst = self
            .data
            .as_ref()
            .ok_or("not a data file")?
            .instances
            .get(index)
            .ok_or("no instance")?;
        let mut raw = self.raw.clone();
        if segs.is_empty() {
            let (TypeExpr::Named(ty), Value::Object(values)) = (&clip.ty, &clip.value) else {
                return Err(format!("yanked {}, this is an instance", clip.ty.text()));
            };
            if *ty != inst.ty {
                return Err(format!("yanked a {ty}, this is a {}", inst.ty));
            }
            let item = raw["instances"]
                .get_mut(index)
                .and_then(Value::as_object_mut)
                .ok_or("no instance")?;
            item.retain(|k, _| k.starts_with('$'));
            for (k, v) in values {
                if !k.starts_with('$') {
                    item.insert(k.clone(), v.clone());
                }
            }
            let item = &mut raw["instances"][index];
            *item = normalise_one(item, &self.schema);
            return Ok(Edit::Text(write_kind(self.kind, &raw)));
        }
        let (ty, _, readonly) = self.walk(&inst.ty, &segs)?;
        if readonly {
            return Err("read-only".into());
        }
        let fits = ty == clip.ty || matches!(&ty, TypeExpr::Optional(inner) if **inner == clip.ty);
        if !fits {
            return Err(format!("yanked {}, this is {}", clip.ty.text(), ty.text()));
        }
        self.assign_at(&mut raw, index, &segs, clip.value.clone())?;
        Ok(Edit::Text(write_kind(self.kind, &raw)))
    }

    /// `dd` on a row: what it removes, or the prompt it needs.
    pub fn delete(&self, key: &str) -> Result<Edit, String> {
        match self.kind {
            Kind::Data => self.delete_data(key),
            Kind::Schema => self.delete_schema(key),
        }
    }

    fn delete_data(&self, key: &str) -> Result<Edit, String> {
        if key.contains("/@") {
            return Err("a group: remove its fields one by one".into());
        }
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
        if let Ok((_, _, true)) = self.walk(&inst.ty, &segs) {
            return Err("read-only".into());
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

    /// `Space`, `h`, `l`, `Ctrl-A`, `Ctrl-X` on a value: a bool flipped,
    /// a number stepped, an enum, a ref or an optional turned. `steps` is
    /// the direction and count; `space` says which key, since `Space`
    /// turns an optional off where the steps turn its value.
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
        let (ty, field, readonly) = self.walk(&inst.ty, &segs)?;
        if readonly {
            return Err("read-only".into());
        }
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
            TypeExpr::Int(kind) => {
                let step = field.and_then(|f| f.step).unwrap_or(1.0).round().max(1.0) as i128;
                let now = current
                    .as_i64()
                    .map(i128::from)
                    .or_else(|| current.as_u64().map(i128::from))
                    .unwrap_or(0);
                let mut next = now + steps as i128 * step;
                if let Some(min) = field.and_then(|f| f.min) {
                    next = next.max(min.ceil() as i128);
                }
                if let Some(max) = field.and_then(|f| f.max) {
                    next = next.min(max.floor() as i128);
                }
                let (lo, hi) = kind.range();
                Ok(super::schema::int_value(next.clamp(lo, hi)))
            }
            TypeExpr::F32 | TypeExpr::F64 => {
                let step = field.and_then(|f| f.step).unwrap_or(0.1);
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
            TypeExpr::Rgb | TypeExpr::Rgba => Err("a colour: Enter edits it".into()),
            TypeExpr::Curve => Err("a curve: Enter opens it".into()),
            TypeExpr::Gradient => Err("a gradient: Enter opens it".into()),
            TypeExpr::List(_) => Err("a list: open it, or `a` adds an item".into()),
        }
    }

    /// `Enter` on a colour, curve or gradient row: which editor, the
    /// row's path, and the edit that writes an inherited value into the
    /// instance first so there is a literal to edit. `None` on any other
    /// row; a read-only value refuses.
    pub fn tool_target(
        &self,
        key: &str,
    ) -> Result<Option<(ToolKind, String, Option<Edit>)>, String> {
        if self.kind != Kind::Data {
            return Ok(None);
        }
        let Some(row) = self.rows.iter().find(|r| r.key == key) else { return Ok(None) };
        if !matches!(row.kind, RowKind::Field | RowKind::Item) {
            return Ok(None);
        }
        let (index, segs) = parse_data_key(key)?;
        let inst = self.data.as_ref().ok_or("not a data file")?.instances.get(index);
        let Some(inst) = inst else { return Ok(None) };
        let (ty, _, readonly) = self.walk(&inst.ty, &segs)?;
        let (ty, optional) = match ty {
            TypeExpr::Optional(inner) => (*inner, true),
            other => (other, false),
        };
        let kind = match ty {
            TypeExpr::Curve => ToolKind::Curve,
            TypeExpr::Gradient => ToolKind::Gradient,
            TypeExpr::Rgb | TypeExpr::Rgba => ToolKind::Color,
            _ => return Ok(None),
        };
        if readonly {
            return Err("read-only".into());
        }
        let path = self.path_of(key).ok_or("no path")?;
        let (resolved, stored) = self.resolved_at(index, &segs)?;
        if optional && resolved.is_null() {
            // Absent: `Space` turns it on; Enter edits the ex line as ever.
            return Ok(None);
        }
        let too_few = match kind {
            ToolKind::Curve => resolved.as_array().is_none_or(Vec::is_empty),
            ToolKind::Gradient => resolved.as_array().is_none_or(|a| a.len() < 2),
            ToolKind::Color => false,
        };
        let edit = if stored && !too_few {
            None
        } else {
            // Inherited, absent or too short: the resolved value — the
            // linear preset for an empty curve, black to white for a
            // gradient — written whole, as one undo step.
            let value = match (kind, too_few) {
                (ToolKind::Curve, true) => super::schema::curve_value(&crate::curve::linear()),
                (ToolKind::Gradient, true) => {
                    super::schema::gradient_value(&crate::gradient::linear())
                }
                _ => resolved,
            };
            let mut raw = self.raw.clone();
            self.assign_at(&mut raw, index, &segs, value.clone())?;
            // Sparse storage drops a value equal to its default, and the
            // curve being written usually is the default; this one leaf
            // is stored anyway, so the tool has bytes to move.
            let mut at = raw
                .get_mut("instances")
                .and_then(Value::as_array_mut)
                .and_then(|a| a.get_mut(index))
                .ok_or("no instance")?;
            for (i, seg) in segs.iter().enumerate() {
                let last = i + 1 == segs.len();
                at = match seg {
                    Seg::Key(k) => {
                        let map = at.as_object_mut().ok_or("not a struct")?;
                        if last {
                            map.insert(k.clone(), value.clone());
                        } else if !map.get(k).is_some_and(Value::is_object) {
                            map.insert(k.clone(), Value::Object(Map::new()));
                        }
                        map.get_mut(k).ok_or("no field")?
                    }
                    Seg::Index(n) => {
                        let items = at.as_array_mut().ok_or("not a list")?;
                        let item = items.get_mut(*n).ok_or("no item")?;
                        if last {
                            *item = value.clone();
                        }
                        item
                    }
                };
            }
            Some(Edit::Text(write_kind(self.kind, &raw)))
        };
        Ok(Some((kind, path, edit)))
    }

    /// The byte span of the value at `path` in `text`, the buffer's text
    /// as it is now — where the curve tool anchors.
    pub fn locate(&self, text: &str, path: &str) -> Option<(usize, usize)> {
        let (index, segs) = self.parse_data_path(path).ok()?;
        super::locate::value_span(text, index, &segs)
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
        let (ty, _, readonly) = self.walk(&inst.ty, &segs)?;
        let item_ty = match &ty {
            TypeExpr::List(inner) => inner.as_ref().clone(),
            TypeExpr::Optional(inner) => match inner.as_ref() {
                TypeExpr::List(item) => item.as_ref().clone(),
                _ => return Err("not a list".into()),
            },
            _ => return Err("not a list (`:bi new` adds an instance)".into()),
        };
        if readonly {
            return Err("read-only".into());
        }
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
    /// an enum value in a schema; `:bi rename <Type> <New>` a type.
    pub fn rename(&self, spec: &str, new: &str) -> Result<Edit, String> {
        match (self.kind, spec.split_once('.')) {
            (Kind::Data, Some((ty, old))) => self.rename_id(ty, old, new),
            (Kind::Data, None) => Err("want <Type>.<id>".into()),
            (Kind::Schema, Some((ty, old))) => self.rename_in_schema(ty, old, new),
            (Kind::Schema, None) => self.rename_type(spec, new),
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
        let (ty, ..) = self.walk(&inst.ty, &segs)?;
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
                RowKind::Instance
                    | RowKind::Group
                    | RowKind::Type
                    | RowKind::FieldDef
                    | RowKind::Error
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
                let (ty, ..) = self.walk(&inst.ty, &segs).ok()?;
                let (value, _) = self.resolved_at(index, &segs).ok()?;
                self.edit_text(&ty, &value)
            }
            Kind::Schema => self.schema_edit_text(key).unwrap_or_default(),
        };
        Some(format!("bi set {path} {value}"))
    }

    /// `J` / `K`: the row moved among its siblings — an instance, a list
    /// item, a type, a field, an enum value. The key to select after.
    pub fn shift(&self, key: &str, down: bool) -> Result<(Edit, String), String> {
        match self.kind {
            Kind::Data => self.shift_data(key, down),
            Kind::Schema => self.shift_schema(key, down),
        }
    }

    fn shift_data(&self, key: &str, down: bool) -> Result<(Edit, String), String> {
        let (index, segs) = parse_data_key(key)?;
        let data = self.data.as_ref().ok_or("not a data file")?;
        let mut raw = self.raw.clone();
        if segs.is_empty() {
            let items = raw["instances"].as_array_mut().ok_or("no instances")?;
            let to = swap_target(index, items.len(), down).ok_or("nowhere to move")?;
            items.swap(index, to);
            return Ok((Edit::Text(write_kind(self.kind, &raw)), format!("inst:{to}")));
        }
        let Some(Seg::Index(n)) = segs.last().cloned() else {
            return Err("a field keeps its place; J and K move list items and instances".into());
        };
        let inst = data.instances.get(index).ok_or("no instance")?;
        let list_segs = &segs[..segs.len() - 1];
        let (_, _, readonly) = self.walk(&inst.ty, list_segs)?;
        if readonly {
            return Err("read-only".into());
        }
        let (current, _) = self.resolved_at(index, list_segs)?;
        let mut items = current.as_array().cloned().unwrap_or_default();
        let to = swap_target(n, items.len(), down).ok_or("nowhere to move")?;
        items.swap(n, to);
        self.assign_at(&mut raw, index, list_segs, Value::Array(items))?;
        let parent = key.rsplit_once('/').map(|(p, _)| p).unwrap_or(key);
        Ok((Edit::Text(write_kind(self.kind, &raw)), format!("{parent}/[{to}]")))
    }

    fn shift_schema(&self, key: &str, down: bool) -> Result<(Edit, String), String> {
        let mut parts = key.split('/');
        let name = parts.next().and_then(|k| k.strip_prefix("type:")).ok_or("nothing to move")?;
        let mut raw = self.raw.clone();
        match (parts.next(), parts.next()) {
            (None, _) => {
                let types = raw["types"].as_object_mut().ok_or("no types")?;
                let at = types.keys().position(|k| k == name).ok_or("no type")?;
                let to = swap_target(at, types.len(), down).ok_or("nowhere to move")?;
                let mut entries: Vec<(String, Value)> = std::mem::take(types).into_iter().collect();
                entries.swap(at, to);
                types.extend(entries);
                Ok((Edit::Text(write_kind(self.kind, &raw)), key.into()))
            }
            (Some(f), None) if f.starts_with("field:") => {
                let fname = &f["field:".len()..];
                let fields = raw["types"][name]["fields"].as_array_mut().ok_or("no fields")?;
                let at = fields
                    .iter()
                    .position(|x| x.get("name").and_then(Value::as_str) == Some(fname))
                    .ok_or("no field")?;
                let to = swap_target(at, fields.len(), down).ok_or("nowhere to move")?;
                fields.swap(at, to);
                Ok((Edit::Text(write_kind(self.kind, &raw)), key.into()))
            }
            (Some(v), None) if v.starts_with("value:") => {
                let i: usize = v["value:".len()..].parse().map_err(|_| "bad row")?;
                let values = raw["types"][name]["values"].as_array_mut().ok_or("no values")?;
                let to = swap_target(i, values.len(), down).ok_or("nowhere to move")?;
                values.swap(i, to);
                Ok((Edit::Text(write_kind(self.kind, &raw)), format!("type:{name}/value:{to}")))
            }
            _ => Err("an attribute keeps its place".into()),
        }
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
            return Some(def.doc().map(|d| format!("{d:?}")).unwrap_or_default());
        }
        if let Some(i) = second.strip_prefix("value:") {
            let i: usize = i.parse().ok()?;
            return self.schema.enum_values(name)?.get(i).cloned();
        }
        let fname = second.strip_prefix("field:")?;
        let f = self.schema.field(name, fname)?;
        let attr = parts.next()?;
        Some(match attr {
            "default" => f.default.as_ref().map(|d| self.edit_text(&f.ty, d)).unwrap_or_default(),
            "doc" | "label" => self.attr_value(f, attr).unwrap_or_default(),
            other => self.attr_value(f, other).unwrap_or_default(),
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
                if !FIELD_ATTRS.contains(&attr.as_str()) {
                    return Err(format!(
                        "not a field attribute: {attr} (want {})",
                        FIELD_ATTRS.join(", ")
                    ));
                }
                if text.is_empty() {
                    let value = self.attr_value(f, attr).unwrap_or_else(|| "-".into());
                    return Err(format!("{path} = {value}"));
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
                    "min" | "max" | "step" | "order" => {
                        if none {
                            None
                        } else {
                            let n: f64 =
                                text.parse().map_err(|_| format!("{path} wants a number"))?;
                            Some(number(n))
                        }
                    }
                    "doc" | "label" | "group" => {
                        if none {
                            None
                        } else {
                            Some(Value::String(unquote(text)))
                        }
                    }
                    "readonly" => match text {
                        "true" => Some(Value::Bool(true)),
                        "false" => None,
                        _ if none => None,
                        _ => return Err(format!("{path} wants true or false")),
                    },
                    "show_if" | "hide_if" => {
                        if none {
                            None
                        } else {
                            Cond::parse(text).map_err(|e| format!("{path} {e}"))?;
                            Some(Value::String(text.into()))
                        }
                    }
                    "widget" => {
                        if none {
                            None
                        } else {
                            Widget::parse(text).ok_or_else(|| {
                                format!("{path} wants {} or -", Widget::NAMES.join(", "))
                            })?;
                            Some(Value::String(text.into()))
                        }
                    }
                    _ => unreachable!("checked against FIELD_ATTRS"),
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
                if attr == "type" {
                    self.retype(&mut raw, &name, fname);
                }
                self.checked(raw)
            }
            [] => Err(format!("{path} is a type; name an attribute")),
            _ => Err(format!("not a schema path: {path}")),
        }
    }

    /// After a field's type changed: a default, a range or a widget that
    /// no longer fits it goes, rather than the change being refused.
    fn retype(&self, raw: &mut Value, ty: &str, fname: &str) {
        let Some(field) = field_mut(raw, ty, fname) else { return };
        let Some(new) =
            field.get("type").and_then(Value::as_str).and_then(|t| TypeExpr::parse(t).ok())
        else {
            return;
        };
        if let Some(default) = field.get("default")
            && self.schema.check(&new, default).is_err()
        {
            field.shift_remove("default");
        }
        if !new.is_numeric() {
            for key in ["min", "max", "step"] {
                field.shift_remove(key);
            }
        }
        let widget = field.get("widget").and_then(Value::as_str).and_then(Widget::parse);
        let fits = match widget {
            Some(Widget::Toggle) => self.schema.enum_values(&new.text()).is_some(),
            Some(Widget::Inline) => self.schema.is_struct(&new.text()),
            None => true,
        };
        if !fits {
            field.shift_remove("widget");
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
                // Conditions on siblings follow the name.
                for f in fields {
                    for key in ["show_if", "hide_if"] {
                        let cond = match key {
                            "show_if" => &f.show_if,
                            _ => &f.hide_if,
                        };
                        if let Some(c) = cond
                            && c.field == old
                        {
                            let renamed = Cond { field: new.into(), ..c.clone() };
                            if let Some(fm) = field_mut(&mut raw, ty, &f.name) {
                                fm.insert(key.into(), Value::String(renamed.text()));
                            }
                        }
                    }
                }
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

    /// `:bi rename Weapon Arm`: the type's key and every type expression
    /// naming it; the data files' `$type` follow.
    fn rename_type(&self, old: &str, new: &str) -> Result<Edit, String> {
        if self.schema.get(old).is_none() {
            return Err(format!("no type {old}"));
        }
        if new == old {
            return Err(format!("{old} is already called that"));
        }
        if !is_identifier(new)
            || super::schema::PRIMITIVES.contains(&new)
            || super::schema::GENERICS.contains(&new)
        {
            return Err(format!("{new:?} is not a type name"));
        }
        if self.schema.get(new).is_some() {
            return Err(format!("type {new} exists"));
        }
        let mut raw = self.raw.clone();
        let types = raw["types"].as_object_mut().ok_or("no types")?;
        let entries: Vec<(String, Value)> = std::mem::take(types)
            .into_iter()
            .map(|(k, v)| if k == old { (new.to_string(), v) } else { (k, v) })
            .collect();
        types.extend(entries);
        for (tname, def) in &self.schema.types {
            let tname = if tname == old { new } else { tname };
            for f in def.fields() {
                let renamed = f.ty.renamed(old, new);
                if renamed != f.ty
                    && let Some(fm) = field_mut(&mut raw, tname, &f.name)
                {
                    fm.insert("type".into(), Value::String(renamed.text()));
                }
            }
        }
        Schema::from_value(&raw).map_err(|e| summary(&e).unwrap_or_default())?;
        Ok(Edit::Refactor {
            text: write_kind(self.kind, &raw),
            refactor: Refactor::RenameType { old: old.into(), new: new.into() },
        })
    }

    /// `:bi add …` on a schema, and the key of the last thing added:
    ///
    /// ```text
    /// :bi add Rarity enum common rare epic       a type and its values
    /// :bi add Weapon struct name:string dmg:i32  a type and its fields
    /// :bi add Weapon speed f32                   one field
    /// :bi add Weapon speed:f32 weight:f32        fields
    /// :bi add Rarity mythic legendary            values
    /// ```
    pub fn add_to_schema(&self, args: &[&str]) -> Result<(Edit, Option<String>), String> {
        if self.kind != Kind::Schema {
            return Err("not a schema (`:bi new` adds an instance)".into());
        }
        let mut raw = self.raw.clone();
        let mut last = None;
        match args {
            [name, kind @ ("struct" | "enum"), rest @ ..] if self.schema.get(name).is_none() => {
                if !is_identifier(name)
                    || super::schema::PRIMITIVES.contains(name)
                    || super::schema::GENERICS.contains(name)
                {
                    return Err(format!("{name:?} is not a type name"));
                }
                let mut def = Map::new();
                def.insert("kind".into(), Value::String((*kind).into()));
                if *kind == "struct" {
                    let mut fields = Vec::new();
                    for pair in rest {
                        let (fname, ftype) = pair
                            .split_once(':')
                            .ok_or_else(|| format!("{pair}: want <field>:<type>"))?;
                        fields.push(field_def(fname, ftype)?);
                    }
                    def.insert("fields".into(), Value::Array(fields));
                } else {
                    let values: Vec<&str> =
                        if rest.is_empty() { vec!["none"] } else { rest.to_vec() };
                    def.insert(
                        "values".into(),
                        Value::Array(values.iter().map(|v| Value::String((*v).into())).collect()),
                    );
                }
                raw["types"]
                    .as_object_mut()
                    .ok_or("no types")?
                    .insert((*name).into(), Value::Object(def));
                last = Some(format!("type:{name}"));
            }
            [ty, rest @ ..] if self.schema.is_struct(ty) && !rest.is_empty() => {
                // `speed f32` or `speed:f32 weight:f32`.
                let pairs: Vec<(String, String)> = match rest {
                    [fname, ftype] if !fname.contains(':') => {
                        vec![((*fname).into(), (*ftype).into())]
                    }
                    many => many
                        .iter()
                        .map(|pair| {
                            pair.split_once(':')
                                .map(|(a, b)| (a.to_string(), b.to_string()))
                                .ok_or_else(|| format!("{pair}: want <field>:<type>"))
                        })
                        .collect::<Result<_, _>>()?,
                };
                for (fname, ftype) in &pairs {
                    if self.schema.field(ty, fname).is_some() {
                        return Err(format!("{ty}.{fname} exists"));
                    }
                    raw["types"][*ty]["fields"]
                        .as_array_mut()
                        .ok_or("no fields")?
                        .push(field_def(fname, ftype)?);
                    last = Some(format!("type:{ty}/field:{fname}"));
                }
            }
            [ty, values @ ..] if self.schema.enum_values(ty).is_some() && !values.is_empty() => {
                for value in values {
                    if self.schema.enum_values(ty).is_some_and(|v| v.iter().any(|x| x == value)) {
                        return Err(format!("{ty}.{value} exists"));
                    }
                    let list = raw["types"][*ty]["values"].as_array_mut().ok_or("no values")?;
                    list.push(Value::String((*value).into()));
                    last = Some(format!("type:{ty}/value:{}", list.len() - 1));
                }
            }
            [ty] if self.schema.is_struct(ty) => {
                return Err(format!("add what to {ty}? (`:bi add {ty} <field>:<type> …`)"));
            }
            [ty] if self.schema.get(ty).is_some() => {
                return Err(format!("add what to {ty}? (`:bi add {ty} <value> …`)"));
            }
            [name, ..] if self.schema.get(name).is_none() => {
                return Err(format!("no type {name} (`:bi add {name} struct|enum …` makes one)"));
            }
            _ => {
                return Err("add what? (`:bi add <Type> <field>:<type> …`, `:bi add <Enum> <value> …`, `:bi add <Name> struct|enum …`)".into());
            }
        }
        self.checked(raw).map(|edit| (edit, last))
    }

    fn add_schema(&self, key: &str) -> Result<Edit, String> {
        let name = key.strip_prefix("type:").and_then(|k| k.split('/').next());
        Ok(match name {
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
            "min" | "max" | "step" | "order" => {
                let current = match attr {
                    "min" => f.min,
                    "max" => f.max,
                    "step" => f.step,
                    _ => f.order,
                }
                .unwrap_or(0.0);
                let step =
                    if f.ty.is_integer() || attr == "order" { 1.0 } else { f.step.unwrap_or(0.1) };
                let scale = 10f64.powi(decimals(step) as i32);
                number(((current + steps as f64 * step) * scale).round() / scale)
            }
            "type" => {
                let choices = self.schema.type_choices();
                let at = choices.iter().position(|c| *c == f.ty.text());
                let next = match at {
                    Some(at) => (at as i64 + steps).rem_euclid(choices.len() as i64) as usize,
                    None => 0,
                };
                Value::String(choices[next].clone())
            }
            "readonly" => Value::Bool(!f.readonly),
            "widget" => {
                // none → toggle → inline → none, skipping what the type refuses.
                let mut options: Vec<Option<Widget>> = vec![None];
                if self.schema.enum_values(&f.ty.text()).is_some() {
                    options.push(Some(Widget::Toggle));
                }
                if self.schema.is_struct(&f.ty.text()) {
                    options.push(Some(Widget::Inline));
                }
                if options.len() == 1 {
                    return Err(format!("{} has no widget to pick", f.ty.text()));
                }
                let at = options.iter().position(|o| *o == f.widget).unwrap_or(0);
                let next = (at as i64 + steps).rem_euclid(options.len() as i64) as usize;
                match options[next] {
                    Some(w) => Value::String(w.text().into()),
                    None => Value::Null,
                }
            }
            _ => return Err(format!("{attr}: Enter edits it")),
        };
        let field = field_mut(&mut raw, name, fname).ok_or("no field")?;
        match (attr, value) {
            ("readonly", Value::Bool(false)) | ("widget", Value::Null) => {
                field.shift_remove(attr);
            }
            (_, value) => {
                field.insert(attr.into(), value);
            }
        }
        if attr == "type" {
            self.retype(&mut raw, name, fname);
        }
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

/// A curve across sixteen cells: `y` at the middle of each, clamped to
/// the unit square, as eighths.
fn sparkline(curve: &crate::curve::Curve) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, cell) in out.iter_mut().enumerate() {
        let x = (i as f32 + 0.5) / 16.0;
        let y = crate::curve::eval(curve, x).clamp(0.0, 1.0);
        *cell = (y * 7.0).round() as u8;
    }
    out
}

/// Whether `h` and `l` turn a value of this type.
fn turnable(ty: &TypeExpr, value: &Value) -> bool {
    match ty {
        TypeExpr::Bool | TypeExpr::Int(_) | TypeExpr::F32 | TypeExpr::F64 | TypeExpr::Ref(_) => {
            true
        }
        TypeExpr::Named(_) => value.is_string(),
        TypeExpr::Optional(inner) => value.is_null() || turnable(inner, value),
        _ => false,
    }
}

fn swap_target(at: usize, len: usize, down: bool) -> Option<usize> {
    if down { (at + 1 < len).then_some(at + 1) } else { at.checked_sub(1) }
}

/// A field definition for `:bi add`.
fn field_def(name: &str, ty: &str) -> Result<Value, String> {
    if !is_identifier(name) || name.starts_with('$') {
        return Err(format!("{name:?} is not an identifier"));
    }
    TypeExpr::parse(ty)?;
    let mut def = Map::new();
    def.insert("name".into(), Value::String(name.into()));
    def.insert("type".into(), Value::String(ty.into()));
    Ok(Value::Object(def))
}

fn attr_row(key: &str, depth: usize, label: &str, value: Option<String>) -> Row {
    let mut row = Row::new(key, depth, label, RowKind::Attr);
    row.value = value.clone().unwrap_or_else(|| "—".into());
    row.inherited = value.is_none();
    row
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
/// Group segments (`@Stats`) are layout, not data, and are skipped.
fn parse_data_key(key: &str) -> Result<(usize, Vec<Seg>), String> {
    let mut parts = key.split('/');
    let index: usize = parts
        .next()
        .and_then(|h| h.strip_prefix("inst:"))
        .and_then(|i| i.parse().ok())
        .ok_or("not a data row")?;
    let mut segs = Vec::new();
    for part in parts {
        if part.starts_with('@') {
            continue;
        }
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
/// created from their meaning, lists materialised whole.
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
        (TypeExpr::Named(_), [Seg::Key(k)]) => {
            let mut map = match stored {
                Some(Value::Object(m)) => m.clone(),
                _ => return Err(format!("{k} is already the default")),
            };
            if map.shift_remove(k).is_none() {
                return Err(format!("{k} is already the default"));
            }
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
    fn rows_carry_their_widgets() {
        let mut p = data_view();
        open(&mut p, "inst:3");
        let widget = |p: &Props, key: &str| p.rows.iter().find(|r| r.key == key).unwrap().widget;
        assert_eq!(widget(&p, "inst:3"), RowWidget::Header);
        assert_eq!(widget(&p, "inst:3/name"), RowWidget::Text);
        assert_eq!(widget(&p, "inst:3/hp"), RowWidget::Plain, "a min without a max is no slider");
        assert_eq!(widget(&p, "inst:3/speed"), RowWidget::Slider(0.14));
        assert_eq!(widget(&p, "inst:3/weapon"), RowWidget::Choice);
        assert_eq!(widget(&p, "inst:3/drops"), RowWidget::Fold);
        assert_eq!(widget(&p, "inst:3/leader"), RowWidget::Choice);
        open(&mut p, "inst:2");
        assert_eq!(widget(&p, "inst:2/two_handed"), RowWidget::Check(true));
        assert_eq!(widget(&p, "inst:2/damage"), RowWidget::Slider(40.0 / 999.0));
        assert_eq!(widget(&p, "inst:2/rarity"), RowWidget::Choice);
        let turnable =
            |p: &Props, key: &str| p.rows.iter().find(|r| r.key == key).unwrap().turnable;
        assert!(turnable(&p, "inst:2/damage"));
        assert!(turnable(&p, "inst:2/rarity"));
        assert!(turnable(&p, "inst:3/leader"));
        assert!(!turnable(&p, "inst:3/name"));
        assert!(!turnable(&p, "inst:3/drops"));
        assert!(!turnable(&p, "inst:3"));
        p.expand_first();
        assert!(p.rows[0].expanded);
        p.next_section(false);
        assert_eq!(p.selected_row().unwrap().key, "inst:1");
        p.next_section(true);
        assert_eq!(p.selected_row().unwrap().key, "inst:0");
    }

    const PAINT: &str = r##"{"$dialect":"bi/1","types":{
        "Fx":{"kind":"struct","fields":[
            {"name":"tint","type":"rgb","default":"#c83c1e"},
            {"name":"glow","type":"rgba"},
            {"name":"falloff","type":"curve"},
            {"name":"fixed","type":"curve","readonly":true},
            {"name":"maybe","type":"optional<curve>"},
            {"name":"ramp","type":"gradient"}]}}}"##;
    const PAINT_DATA: &str = r##"{"$dialect":"bi/1","$schema":"paint.bischema","instances":[
        {"$type":"Fx","$id":"fire","glow":"#ff880080",
         "falloff":[[0, 0], [0.5, 1, 0, 0, true], [1, 0]]},
        {"$type":"Fx","$id":"plain","tint":"#FFFFFF","falloff":[],
         "ramp":[[0, "#ff0000"], [1, "#0000ff80"]]}]}"##;

    fn paint_view() -> Props {
        let mut p = Props::new(Kind::Data, BufferId(1), Some(PathBuf::from("/p/fx.bidata")));
        p.load(PAINT_DATA, Some(Ok(PAINT.into())), Index::default()).unwrap();
        p
    }

    #[test]
    fn colour_and_curve_rows_draw_a_brick_and_a_sparkline_and_never_turn() {
        let mut p = paint_view();
        open(&mut p, "inst:0");
        let row = |p: &Props, key: &str| p.rows.iter().find(|r| r.key == key).unwrap().clone();
        let tint = row(&p, "inst:0/tint");
        assert_eq!(tint.widget, RowWidget::Color([0xc8, 0x3c, 0x1e, 0xff]));
        assert_eq!(tint.value, "#c83c1e");
        assert!(tint.inherited && !tint.turnable && !tint.expandable);
        let glow = row(&p, "inst:0/glow");
        assert_eq!(glow.widget, RowWidget::Color([0xff, 0x88, 0x00, 0x80]));
        let falloff = row(&p, "inst:0/falloff");
        assert_eq!(falloff.value, "3 points");
        let RowWidget::Curve(samples) = falloff.widget else { panic!("{:?}", falloff.widget) };
        assert_eq!(samples[0], 0, "starts at 0");
        assert_eq!(samples[7].max(samples[8]), 7, "peaks at the middle");
        assert!(samples[3] > samples[0] && samples[3] < samples[7], "rises between");
        assert!(!falloff.turnable && !falloff.expandable);
        let linear = row(&p, "inst:0/fixed");
        assert_eq!(linear.value, "2 points");
        assert!(linear.readonly && linear.inherited);
        assert_eq!(row(&p, "inst:0/maybe").widget, RowWidget::Choice, "an absent optional");
        let ramp = row(&p, "inst:0/ramp");
        assert_eq!(ramp.value, "2 stops");
        let RowWidget::Gradient(cells) = ramp.widget else { panic!("{:?}", ramp.widget) };
        assert_eq!(cells[0], [8, 8, 8, 255], "black to white, sampled mid-cell");
        assert_eq!(cells[15], [247, 247, 247, 255]);
        assert!(ramp.inherited && !ramp.turnable);
        open(&mut p, "inst:1");
        let RowWidget::Gradient(cells) = row(&p, "inst:1/ramp").widget else {
            panic!("a gradient")
        };
        assert_eq!(cells[0], [247, 0, 8, 251], "red to translucent blue");
        assert_eq!(p.turn("inst:0/ramp", 1, false), Err("a gradient: Enter opens it".into()));
        assert_eq!(
            p.tool_target("inst:1/ramp"),
            Ok(Some((ToolKind::Gradient, "plain.ramp".into(), None)))
        );
        let (kind, _, edit) = p.tool_target("inst:0/ramp").unwrap().unwrap();
        assert_eq!(kind, ToolKind::Gradient);
        let Some(Edit::Text(text)) = edit else { panic!("inherited: written first") };
        assert!(text.contains("\"ramp\": [[0, \"#000000ff\"], [1, \"#ffffffff\"]]"), "{text}");

        assert_eq!(p.turn("inst:0/tint", 1, false), Err("a colour: Enter edits it".into()));
        assert_eq!(p.turn("inst:0/falloff", 1, false), Err("a curve: Enter opens it".into()));
        assert_eq!(p.edit_line("inst:0/tint"), Some("bi set fire.tint #c83c1e".into()));
        assert_eq!(
            p.edit_line("inst:0/falloff"),
            Some("bi set fire.falloff [[0,0],[0.5,1,0,0,true],[1,0]]".into())
        );
        let text = text_of(p.set("fire.tint", "0080ff"));
        assert!(text.contains("\"tint\": \"#0080ff\""), "{text}");
        assert_eq!(p.set("fire.glow", "blue"), Err("fire.glow wants a colour as #rrggbbaa".into()));
        let text = text_of(p.set("fire.falloff", "[[0, 1], [1, 0]]"));
        assert!(text.contains("\"falloff\": [[0, 1], [1, 0]]"), "{text}");
        let text = text_of(p.delete("inst:0/glow"));
        assert!(!text.contains("glow"), "back to the default: {text}");
    }

    #[test]
    fn enter_on_a_curve_names_its_path_and_materialises_an_inherited_one() {
        let mut p = paint_view();
        open(&mut p, "inst:0");
        open(&mut p, "inst:1");
        let (kind, path, edit) = p.tool_target("inst:0/tint").unwrap().unwrap();
        assert_eq!((kind, path.as_str()), (ToolKind::Color, "fire.tint"));
        let Some(Edit::Text(text)) = edit else {
            panic!("an inherited colour is written: {edit:?}")
        };
        assert!(text.contains("\"tint\": \"#c83c1e\""), "{text}");
        assert_eq!(
            p.tool_target("inst:0/glow"),
            Ok(Some((ToolKind::Color, "fire.glow".into(), None))),
            "stored: nothing to write"
        );
        assert_eq!(p.tool_target("inst:0"), Ok(None));
        assert_eq!(
            p.tool_target("inst:0/falloff"),
            Ok(Some((ToolKind::Curve, "fire.falloff".into(), None)))
        );
        assert_eq!(p.tool_target("inst:0/fixed"), Err("read-only".into()));
        assert_eq!(p.tool_target("inst:0/maybe"), Ok(None), "absent: nothing to open");
        let (_, path, edit) = p.tool_target("inst:1/falloff").unwrap().unwrap();
        assert_eq!(path, "plain.falloff");
        let Some(Edit::Text(text)) = edit else { panic!("an empty curve is seeded: {edit:?}") };
        assert!(
            text.contains("\"falloff\": [[0, 0, 1, 0, true], [1, 1, 1, 1, true]]"),
            "the linear preset: {text}"
        );
        p.load(&text, Some(Ok(PAINT.into())), Index::default()).unwrap();
        assert_eq!(
            p.tool_target("inst:1/falloff"),
            Ok(Some((ToolKind::Curve, "plain.falloff".into(), None))),
            "stored now"
        );
        let (s, e) = p.locate(&text, "plain.falloff").expect("located");
        assert_eq!(&text[s..e], "[[0, 0, 1, 0, true], [1, 1, 1, 1, true]]");
        assert_eq!(p.locate(&text, "plain.nothing"), None);
        assert_eq!(p.locate(&text, "plain.tint").map(|(s, e)| &text[s..e]), Some("\"#ffffff\""));
    }

    const LAYOUT: &str = r#"{"$dialect":"bi/1","types":{
        "Rarity":{"kind":"enum","values":["common","rare","epic"]},
        "Vec2":{"kind":"struct","fields":[{"name":"x","type":"f32"},{"name":"y","type":"f32"}]},
        "Weapon":{"kind":"struct","groups":{"Advanced":{"collapsed":true}},"fields":[
            {"name":"name","type":"string","order":-1},
            {"name":"damage","type":"i32","default":10,"min":0,"max":100,"group":"Stats"},
            {"name":"crit","type":"f32","group":"Stats/Combat","show_if":"damage > 50"},
            {"name":"rarity","type":"Rarity","widget":"toggle","label":"Tier"},
            {"name":"offset","type":"Vec2","widget":"inline","group":"Advanced"},
            {"name":"id_hash","type":"u32","readonly":true,"group":"Advanced"},
            {"name":"two_handed","type":"bool","hide_if":"rarity == common"}
        ]}}}"#;

    fn layout_view(instances: &str) -> Props {
        let mut p = Props::new(Kind::Data, BufferId(1), Some(PathBuf::from("/p/l.bidata")));
        let text =
            format!(r#"{{"$dialect":"bi/1","$schema":"l.bischema","instances":{instances}}}"#);
        p.load(&text, Some(Ok(LAYOUT.into())), Index::default()).unwrap();
        p
    }

    #[test]
    fn the_layout_groups_orders_hides_and_labels() {
        let mut p = layout_view(r#"[{"$type":"Weapon","$id":"a","damage":60,"rarity":"rare"}]"#);
        open(&mut p, "inst:0");
        assert_eq!(
            lines(&p),
            [
                "Weapon a",
                "  ~name \"\"",
                "  Stats",
                "    damage 60",
                "    Combat",
                "      ~crit 0",
                "  Tier common [rare] epic",
                "  Advanced",
                "  ~two_handed false",
            ]
        );
        let keys: Vec<&str> = p.rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "inst:0",
                "inst:0/name",
                "inst:0/@Stats",
                "inst:0/damage",
                "inst:0/@Stats/@Combat",
                "inst:0/crit",
                "inst:0/rarity",
                "inst:0/@Advanced",
                "inst:0/two_handed",
            ]
        );
        assert_eq!(p.rows[6].widget, RowWidget::Toggle);
        assert_eq!(p.rows[2].kind, RowKind::Group);
        assert!(!p.rows[7].expanded, "the schema said collapsed");
        // Open the collapsed group: the inline struct is open with no fold.
        p.select(7);
        assert!(p.expand());
        assert_eq!(
            lines(&p)[7..12],
            [
                "  Advanced",
                "    ~offset { x 0, y 0 }",
                "      ~x 0",
                "      ~y 0",
                "    ~id_hash 0"
            ]
        );
        let offset = p.rows.iter().find(|r| r.key == "inst:0/offset").unwrap();
        assert!(!offset.expandable && offset.expanded);
        let hash = p.rows.iter().find(|r| r.key == "inst:0/id_hash").unwrap();
        assert!(hash.readonly);
        assert_eq!(p.turn("inst:0/id_hash", 1, false), Err("read-only".into()));
        assert_eq!(p.set("a.id_hash", "3"), Err("a.id_hash is read-only".into()));
        assert_eq!(p.delete("inst:0/id_hash"), Err("read-only".into()));
        // Close it again, and the closed group survives a rebuild.
        p.select(7);
        p.collapse();
        assert!(!p.rows[7].expanded);
        // A group that starts open closes by hand.
        p.select(2);
        p.collapse();
        assert!(!p.rows[2].expanded);
        assert_eq!(p.rows[3].key, "inst:0/rarity");
        p.expand();
        assert!(p.rows[2].expanded);
        // Conditions follow the values.
        let text = text_of(p.set("a.damage", "20"));
        assert!(text.contains("\"damage\": 20"));
        p.load(&text, Some(Ok(LAYOUT.into())), Index::default()).unwrap();
        assert!(!p.rows.iter().any(|r| r.key == "inst:0/crit"), "damage > 50 no longer holds");
        let text = text_of(p.set("a.rarity", "common"));
        p.load(&text, Some(Ok(LAYOUT.into())), Index::default()).unwrap();
        assert!(!p.rows.iter().any(|r| r.key == "inst:0/two_handed"), "hidden when common");
        assert_eq!(p.path_of("inst:0/@Stats/@Combat"), Some("a".into()));
        assert_eq!(p.path_of("inst:0/damage"), Some("a.damage".into()));
        assert_eq!(p.delete("inst:0/@Stats"), Err("a group: remove its fields one by one".into()));
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
    fn skeletons_and_schema_refs() {
        assert_eq!(
            Props::skeleton(Kind::Data, Some("game.bischema")),
            "{\n  \"$dialect\": \"bi/1\",\n  \"$schema\": \"game.bischema\",\n  \"instances\": []\n}\n"
        );
        assert_eq!(
            Props::skeleton(Kind::Schema, None),
            "{\n  \"$dialect\": \"bi/1\",\n  \"types\": {}\n}\n"
        );
        let fixed =
            Props::with_schema_ref(&Props::skeleton(Kind::Data, None), "../game.bischema").unwrap();
        assert!(fixed.contains("\"$dialect\": \"bi/1\",\n  \"$schema\": \"../game.bischema\",\n"));
        assert!(Props::with_schema_ref("{", "x").is_err());
        let mut p = Props::new(Kind::Data, BufferId(1), None);
        assert_eq!(
            p.load(&Props::skeleton(Kind::Data, None), None, Index::default()),
            Err("no $schema".into())
        );
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
        assert_eq!(p.rename("Enemy", "Foe"), Err("want <Type>.<id>".into()));
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
        let text = after(p.turn("inst:3/hp", 1, true), &mut p);
        assert!(text.contains("\"hp\": 41"), "Space steps a number too: {text}");
        let text = after(p.turn("inst:3/hp", 5, false), &mut p);
        assert!(text.contains("\"hp\": 46"));
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
        let mut p = layout_view(r#"[{"$type":"Weapon","$id":"a","id_hash":5}]"#);
        let text = text_of(p.turn("inst:0/damage", -100, false));
        assert!(text.contains("\"damage\": 0"), "an integer clamps to min: {text}");
        p.load(&text, Some(Ok(LAYOUT.into())), Index::default()).unwrap();
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
        assert!(
            text.ends_with("    {\n      \"$type\": \"Enemy\",\n      \"$id\": \"orc\",\n      \"weapon\": \"\"\n    }\n  ]\n}\n"),
            "{text}"
        );
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
    fn j_and_k_move_instances_items_fields_values_and_types() {
        let mut p = data_view();
        let (edit, key) = p.shift("inst:0", true).unwrap();
        assert_eq!(key, "inst:1");
        let text = after(Ok(edit), &mut p);
        assert_eq!(p.data.as_ref().unwrap().instances[1].id, "rusty_sword");
        assert!(text.contains("\"$id\": \"dagger\",\n      \"name\": \"Dagger\""));
        assert_eq!(p.shift("inst:0", false).unwrap_err(), "nowhere to move");
        let (edit, key) = p.shift("inst:3/drops/[0]", true).unwrap();
        assert_eq!(key, "inst:3/drops/[1]");
        let text = text_of(Ok(edit));
        assert!(text.contains("\"drops\": [\"dagger\", \"rusty_sword\"]"));
        assert!(p.shift("inst:3/hp", true).is_err());

        let mut s = schema_view();
        let (edit, key) = s.shift("type:Weapon/field:damage", false).unwrap();
        assert_eq!(key, "type:Weapon/field:damage");
        let text = text_of(Ok(edit));
        assert!(text.contains("\"fields\": [\n        { \"name\": \"damage\""));
        let (edit, key) = s.shift("type:Rarity/value:0", true).unwrap();
        assert_eq!(key, "type:Rarity/value:1");
        assert!(text_of(Ok(edit)).contains("[\"rare\", \"common\", \"epic\"]"));
        let (edit, _) = s.shift("type:Rarity", true).unwrap();
        let text = text_of(Ok(edit));
        s.load(&text, None, Index::default()).unwrap();
        let names: Vec<&str> = s.schema.types.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Vec2", "Rarity", "Weapon", "Enemy"]);
        assert!(s.shift("type:Weapon/field:damage/min", true).is_err());
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
        open(&mut p, "inst:4");
        assert_eq!(
            p.edit_line("inst:4/leader"),
            Some("bi set goblin_chief.leader ".into()),
            "nothing to prefill for null"
        );
        assert_eq!(p.target("inst:3/weapon"), Ok(("Weapon".into(), "rusty_sword".into())));
        assert_eq!(p.target("inst:3/drops/[1]"), Ok(("Weapon".into(), "dagger".into())));
        assert_eq!(p.target("inst:3/leader"), Ok(("Enemy".into(), "goblin_chief".into())));
        assert_eq!(p.target("inst:3/hp"), Err("not a ref".into()));
        assert!(p.select_instance("Enemy", "goblin_chief"));
        assert_eq!(p.selected_row().unwrap().key, "inst:4");
        assert!(p.selected_row().unwrap().expanded);
        let shared = DATA.replace("\"$id\": \"goblin\"", "\"$id\": \"dagger\"");
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
            lines(&p)[weapon..weapon + 17],
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
                "    ~group —",
                "    ~order —",
                "    ~label —",
                "    ~readonly —",
                "    ~show_if —",
                "    ~hide_if —",
                "    ~widget —",
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
            Some("bi set Weapon.damage.step ".into())
        );
        assert_eq!(p.edit_line("type:Rarity/value:1"), Some("bi set Rarity.values[1] rare".into()));
        assert_eq!(p.edit_line("type:Weapon/field:damage"), None);
        assert_eq!(p.edit_line("type:Weapon/doc"), Some("bi set Weapon.doc ".into()));
    }

    #[test]
    fn schema_edits_are_checked_and_renames_refactor() {
        let mut p = schema_view();
        let text = text_of(p.set("Weapon.damage.default", "20"));
        assert!(text.contains("\"default\": 20, \"min\": 0"));
        p.load(&text, None, Index::default()).unwrap();
        assert_eq!(
            p.set("Weapon.damage.default", "lots"),
            Err("Weapon.damage.default wants a whole number -2147483648..2147483647".into())
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
        assert!(text.contains("\"type\": \"i64\", \"default\": 20"));
        let text = text_of(p.set("Weapon.damage.type", "string"));
        assert!(
            text.contains("{ \"name\": \"damage\", \"type\": \"string\" }"),
            "the default and the range went with the type: {text}"
        );
        let text = text_of(p.set("Weapon.doc", "A thing to hit with"));
        assert!(text.contains("\"doc\": \"A thing to hit with\"\n"));
        let text = text_of(p.set("Rarity.doc", "-"));
        assert!(!text.contains("Drop tier"));
        assert_eq!(p.set("Weapon.damage.default", ""), Err("Weapon.damage.default = 20".into()));
        // Layout attributes.
        let text = text_of(p.set("Weapon.damage.group", "Stats/Combat"));
        assert!(text.contains("\"group\": \"Stats/Combat\""));
        let text = text_of(p.set("Weapon.damage.show_if", "two_handed"));
        assert!(text.contains("\"show_if\": \"two_handed\""));
        assert_eq!(
            p.set("Weapon.damage.show_if", "nothing"),
            Err("Weapon.damage: show_if names no field nothing".into())
        );
        assert_eq!(
            p.set("Weapon.damage.widget", "toggle"),
            Err("Weapon.damage: widget toggle wants an enum".into())
        );
        let text = text_of(p.set("Weapon.rarity.widget", "toggle"));
        assert!(text.contains("\"widget\": \"toggle\""));
        assert_eq!(
            p.set("Weapon.rarity.widget", "knob"),
            Err("Weapon.rarity.widget wants toggle, inline or -".into())
        );
        let text = text_of(p.set("Weapon.damage.readonly", "true"));
        assert!(text.contains("\"readonly\": true"));
        let text = text_of(p.set("Weapon.damage.label", "Damage (HP)"));
        assert!(text.contains("\"label\": \"Damage (HP)\""));
        let text = text_of(p.set("Weapon.damage.order", "-1"));
        assert!(text.contains("\"order\": -1"));
        assert_eq!(
            p.set("Weapon.damage.colour", "red"),
            Err(format!("not a field attribute: colour (want {})", FIELD_ATTRS.join(", ")))
        );
        // Renames.
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
        let Edit::Refactor { text, refactor } = p.rename("Weapon", "Arm").unwrap() else {
            panic!()
        };
        assert_eq!(refactor, Refactor::RenameType { old: "Weapon".into(), new: "Arm".into() });
        assert!(text.contains("\"Arm\": {\n"));
        assert!(text.contains("\"type\": \"ref<Arm>\""));
        assert!(text.contains("\"type\": \"list<ref<Arm>>\""));
        assert!(!text.contains("Weapon"));
        assert_eq!(p.rename("Weapon", "Enemy"), Err("type Enemy exists".into()));
        assert_eq!(p.rename("Weapon", "list"), Err("\"list\" is not a type name".into()));
        // Turning attributes.
        let text = text_of(p.turn("type:Weapon/field:two_handed/default", 1, true));
        assert!(text.contains("\"default\": true"));
        let text = text_of(p.turn("type:Weapon/field:damage/max", 1, false));
        assert!(text.contains("\"max\": 1000"));
        let text = text_of(p.turn("type:Weapon/field:damage/readonly", 1, true));
        assert!(text.contains("\"readonly\": true"));
        let text = text_of(p.turn("type:Weapon/field:rarity/widget", 1, true));
        assert!(text.contains("\"widget\": \"toggle\""));
        assert_eq!(
            p.turn("type:Weapon/field:damage/widget", 1, true),
            Err("i32 has no widget to pick".into())
        );
        let text = text_of(p.turn("type:Weapon/field:name/type", 1, false));
        assert!(
            text.contains("{ \"name\": \"name\", \"type\": \"rgb\" }"),
            "string → the next primitive: {text}"
        );
        let text = text_of(p.turn("type:Weapon/field:name/type", 5, false));
        assert!(
            text.contains("{ \"name\": \"name\", \"type\": \"Rarity\" }"),
            "past the primitives, the first type: {text}"
        );
        let text = text_of(p.turn("type:Weapon/field:name/type", -1, false));
        assert!(text.contains("{ \"name\": \"name\", \"type\": \"f64\" }"), "{text}");
    }

    #[test]
    fn schema_add_and_delete() {
        let mut p = schema_view();
        let (edit, key) = p.add_to_schema(&["Weapon", "speed", "f32"]).unwrap();
        assert_eq!(key.as_deref(), Some("type:Weapon/field:speed"));
        assert!(text_of(Ok(edit)).contains("{ \"name\": \"two_handed\", \"type\": \"bool\", \"default\": false },\n        { \"name\": \"speed\", \"type\": \"f32\" }"));
        let (edit, key) = p.add_to_schema(&["Weapon", "speed:f32", "weight:u8"]).unwrap();
        assert_eq!(key.as_deref(), Some("type:Weapon/field:weight"));
        let text = text_of(Ok(edit));
        assert!(text.contains("{ \"name\": \"speed\", \"type\": \"f32\" },\n        { \"name\": \"weight\", \"type\": \"u8\" }"));
        assert_eq!(
            p.add_to_schema(&["Weapon", "damage", "f32"]),
            Err("Weapon.damage exists".into())
        );
        assert_eq!(
            p.add_to_schema(&["Weapon", "x", "Boss"]),
            Err("Weapon.x: unknown type Boss".into())
        );
        assert_eq!(p.add_to_schema(&["Weapon", "x"]), Err("x: want <field>:<type>".into()));
        let (edit, key) = p.add_to_schema(&["Rarity", "mythic", "legendary"]).unwrap();
        assert_eq!(key.as_deref(), Some("type:Rarity/value:4"));
        assert!(
            text_of(Ok(edit))
                .contains("[\"common\", \"rare\", \"epic\", \"mythic\", \"legendary\"]")
        );
        assert_eq!(p.add_to_schema(&["Rarity", "epic"]), Err("Rarity.epic exists".into()));
        let (edit, key) = p.add_to_schema(&["Color", "struct", "r:u8", "g:u8", "b:u8"]).unwrap();
        assert_eq!(key.as_deref(), Some("type:Color"));
        let text = text_of(Ok(edit));
        assert!(text.contains("\"Color\": {\n      \"kind\": \"struct\",\n      \"fields\": [\n        { \"name\": \"r\", \"type\": \"u8\" },\n        { \"name\": \"g\", \"type\": \"u8\" },\n        { \"name\": \"b\", \"type\": \"u8\" }\n      ]\n    }"));
        let (edit, _) = p.add_to_schema(&["Size", "enum", "small", "large"]).unwrap();
        assert!(text_of(Ok(edit)).contains("\"values\": [\"small\", \"large\"]"));
        let (edit, _) = p.add_to_schema(&["Empty", "enum"]).unwrap();
        assert!(text_of(Ok(edit)).contains("\"values\": [\"none\"]"));
        assert_eq!(p.add_to_schema(&["u8", "struct"]), Err("\"u8\" is not a type name".into()));
        assert_eq!(
            p.add_to_schema(&["Weapon"]),
            Err("add what to Weapon? (`:bi add Weapon <field>:<type> …`)".into())
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
        let (edit, _) = p.add_to_schema(&["Loose", "struct"]).unwrap();
        p.load(&text_of(Ok(edit)), None, Index::default()).unwrap();
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
        assert!(p.rows[3].expanded);
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
        p.parent();
        assert_eq!(p.selected, 0);
        assert!(p.select_key("inst:0/damage"));
        assert!(!p.select_key("nowhere"));
    }
}
