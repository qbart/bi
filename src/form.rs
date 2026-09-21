//! A pane of fields a tool's knobs live in, driven by list keys.
//!
//! Core state with no terminal in it, like a tree or a results list: the
//! frontend draws the fields however it draws. Every change bumps a
//! generation the owning tool watches. See `docs/specs/form.md`.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Float(f32),
    Int(i64),
    Bool(bool),
    /// An index into a `Kind::Choice`'s options.
    Choice(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Float { min: f32, max: f32, step: f32 },
    Int { min: i64, max: i64, step: i64 },
    Bool,
    Choice { options: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    name: String,
    label: String,
    kind: Kind,
    value: Value,
}

impl Field {
    pub fn float(name: &str, label: &str, min: f32, max: f32, step: f32, value: f32) -> Self {
        let kind = Kind::Float { min, max, step };
        Self {
            name: name.into(),
            label: label.into(),
            kind,
            value: Value::Float(value.clamp(min, max)),
        }
    }

    pub fn int(name: &str, label: &str, min: i64, max: i64, step: i64, value: i64) -> Self {
        let kind = Kind::Int { min, max, step: step.max(1) };
        Self {
            name: name.into(),
            label: label.into(),
            kind,
            value: Value::Int(value.clamp(min, max)),
        }
    }

    pub fn bool(name: &str, label: &str, value: bool) -> Self {
        Self { name: name.into(), label: label.into(), kind: Kind::Bool, value: Value::Bool(value) }
    }

    pub fn choice(name: &str, label: &str, options: &[&str], index: usize) -> Self {
        let options: Vec<String> = options.iter().map(|o| o.to_string()).collect();
        let index = index.min(options.len().saturating_sub(1));
        Self {
            name: name.into(),
            label: label.into(),
            kind: Kind::Choice { options },
            value: Value::Choice(index),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn kind(&self) -> &Kind {
        &self.kind
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    /// How many decimals a float shows: as many as its step needs, at
    /// least one, so `2.5` stays `2.5` and a step of `0.01` shows `2.50`.
    fn decimals(&self) -> usize {
        match self.kind {
            Kind::Float { step, .. } => {
                let mut d = 0;
                let mut s = step as f64;
                while (s - s.round()).abs() > 1e-6 && d < 6 {
                    s *= 10.0;
                    d += 1;
                }
                d.max(1)
            }
            _ => 0,
        }
    }

    /// The value as the pane shows it.
    pub fn display(&self) -> String {
        match (&self.kind, &self.value) {
            (Kind::Float { .. }, Value::Float(v)) => format!("{:.*}", self.decimals(), v),
            (Kind::Int { .. }, Value::Int(v)) => v.to_string(),
            (Kind::Bool, Value::Bool(v)) => if *v { "on" } else { "off" }.to_string(),
            (Kind::Choice { options }, Value::Choice(i)) => {
                options.get(*i).cloned().unwrap_or_default()
            }
            _ => String::new(),
        }
    }

    /// The value as the ex line would type it back.
    pub fn text(&self) -> String {
        match (&self.kind, &self.value) {
            (Kind::Bool, Value::Bool(v)) => v.to_string(),
            _ => self.display(),
        }
    }

    /// How far along its range a number is, `0..1`; `None` for a bool or
    /// a choice, which have no slider.
    pub fn fraction(&self) -> Option<f32> {
        match (&self.kind, &self.value) {
            (Kind::Float { min, max, .. }, Value::Float(v)) if max > min => {
                Some((v - min) / (max - min))
            }
            (Kind::Int { min, max, .. }, Value::Int(v)) if max > min => {
                Some((v - min) as f32 / (max - min) as f32)
            }
            _ => None,
        }
    }

    /// What `set` wants, for the refusal.
    fn wants(&self) -> String {
        match &self.kind {
            Kind::Float { min, max, .. } => {
                format!("{} wants a number {}..{}", self.name, compact(*min), compact(*max))
            }
            Kind::Int { min, max, .. } => {
                format!("{} wants a whole number {min}..{max}", self.name)
            }
            Kind::Bool => format!("{} wants true or false", self.name),
            Kind::Choice { options } => {
                format!("{} wants one of {}", self.name, options.join(", "))
            }
        }
    }

    fn parse(&self, text: &str) -> Result<Value, String> {
        let text = text.trim();
        match &self.kind {
            Kind::Float { min, max, .. } => match text.parse::<f32>() {
                Ok(v) if v.is_finite() => Ok(Value::Float(v.clamp(*min, *max))),
                _ => Err(self.wants()),
            },
            Kind::Int { min, max, .. } => match text.parse::<i64>() {
                Ok(v) => Ok(Value::Int(v.clamp(*min, *max))),
                _ => Err(self.wants()),
            },
            Kind::Bool => match text {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err(self.wants()),
            },
            Kind::Choice { options } => match options.iter().position(|o| o == text) {
                Some(i) => Ok(Value::Choice(i)),
                None => Err(self.wants()),
            },
        }
    }

    /// The value `steps` of the field's step away — clamped for numbers,
    /// wrapped for a choice, flipped for a bool.
    fn nudged(&self, steps: i64) -> Value {
        match (&self.kind, &self.value) {
            (Kind::Float { min, max, step }, Value::Float(v)) => {
                let raw = *v as f64 + steps as f64 * *step as f64;
                let scale = 10f64.powi(self.decimals() as i32);
                let snapped = (raw * scale).round() / scale;
                Value::Float((snapped as f32).clamp(*min, *max))
            }
            (Kind::Int { min, max, step }, Value::Int(v)) => {
                Value::Int((v + steps * step).clamp(*min, *max))
            }
            (Kind::Bool, Value::Bool(v)) => Value::Bool(if steps % 2 != 0 { !v } else { *v }),
            (Kind::Choice { options }, Value::Choice(i)) if !options.is_empty() => {
                let len = options.len() as i64;
                Value::Choice((*i as i64 + steps).rem_euclid(len) as usize)
            }
            (_, v) => v.clone(),
        }
    }
}

/// `20` for `20.0`, `0.1` for `0.1`: a range end written the short way.
fn compact(v: f32) -> String {
    if v.fract() == 0.0 { format!("{}", v as i64) } else { v.to_string() }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    tool: String,
    title: String,
    fields: Vec<Field>,
    selected: usize,
    undo: Vec<(usize, Value)>,
    redo: Vec<(usize, Value)>,
    generation: u64,
}

impl Form {
    pub fn new(tool: &str, title: &str) -> Self {
        Self {
            tool: tool.into(),
            title: title.into(),
            fields: Vec::new(),
            selected: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            generation: 0,
        }
    }

    pub fn push(&mut self, field: Field) {
        self.fields.push(field);
    }

    /// The `:tool <name>` that owns this form.
    pub fn tool(&self) -> &str {
        &self.tool
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Bumped by every change, undo and redo included: what the owning
    /// tool compares to know its picture is stale.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn select(&mut self, index: usize) {
        self.selected = index.min(self.fields.len().saturating_sub(1));
    }

    pub fn select_by(&mut self, delta: i64) {
        let last = self.fields.len().saturating_sub(1) as i64;
        self.selected = (self.selected as i64 + delta).clamp(0, last) as usize;
    }

    pub fn first(&mut self) {
        self.selected = 0;
    }

    pub fn last(&mut self) {
        self.selected = self.fields.len().saturating_sub(1);
    }

    /// `h` and `l`: the selected field `steps` of its step along.
    pub fn nudge(&mut self, steps: i64) {
        let Some(field) = self.fields.get(self.selected) else { return };
        let value = field.nudged(steps);
        self.change(self.selected, value);
    }

    /// `Space`: a bool flipped, a choice advanced.
    pub fn toggle(&mut self) {
        let Some(field) = self.fields.get(self.selected) else { return };
        let value = match field.kind {
            Kind::Bool | Kind::Choice { .. } => field.nudged(1),
            _ => return,
        };
        self.change(self.selected, value);
    }

    /// `Tab`: the field named `map`, when there is one, advanced — from
    /// anywhere, without moving the selection.
    pub fn cycle_map(&mut self) -> bool {
        let Some(index) = self.fields.iter().position(|f| f.name == "map") else { return false };
        let value = self.fields[index].nudged(1);
        self.change(index, value);
        true
    }

    /// `:tool <owner> <field> <value>`.
    pub fn set(&mut self, name: &str, text: &str) -> Result<(), String> {
        let Some(index) = self.fields.iter().position(|f| f.name == name) else {
            let names: Vec<&str> = self.fields.iter().map(|f| f.name.as_str()).collect();
            return Err(format!("no field {name} ({})", names.join(", ")));
        };
        let value = self.fields[index].parse(text)?;
        self.change(index, value);
        Ok(())
    }

    /// `:tool <owner> <field>` — the value as it would be typed.
    pub fn value_text(&self, name: &str) -> Option<String> {
        self.field(name).map(Field::text)
    }

    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    pub fn get_f32(&self, name: &str) -> f32 {
        match self.field(name).map(Field::value) {
            Some(Value::Float(v)) => *v,
            Some(Value::Int(v)) => *v as f32,
            _ => 0.0,
        }
    }

    pub fn get_i64(&self, name: &str) -> i64 {
        match self.field(name).map(Field::value) {
            Some(Value::Int(v)) => *v,
            Some(Value::Float(v)) => *v as i64,
            _ => 0,
        }
    }

    pub fn get_bool(&self, name: &str) -> bool {
        matches!(self.field(name).map(Field::value), Some(Value::Bool(true)))
    }

    pub fn get_choice(&self, name: &str) -> &str {
        match self.field(name) {
            Some(Field { kind: Kind::Choice { options }, value: Value::Choice(i), .. }) => {
                options.get(*i).map(String::as_str).unwrap_or("")
            }
            _ => "",
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some((index, value)) = self.undo.pop() else { return false };
        let now = std::mem::replace(&mut self.fields[index].value, value);
        self.redo.push((index, now));
        self.generation += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some((index, value)) = self.redo.pop() else { return false };
        let now = std::mem::replace(&mut self.fields[index].value, value);
        self.undo.push((index, now));
        self.generation += 1;
        true
    }

    fn change(&mut self, index: usize, value: Value) {
        if self.fields[index].value == value {
            return;
        }
        let was = std::mem::replace(&mut self.fields[index].value, value);
        self.undo.push((index, was));
        self.redo.clear();
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Form {
        let mut form = Form::new("normalmap", "Normal map");
        form.push(Field::choice("map", "Map", &["normal", "displacement"], 0));
        form.push(Field::float("strength", "Strength", 0.1, 20.0, 0.1, 2.5));
        form.push(Field::int("blur", "Blur", 0, 32, 1, 0));
        form.push(Field::bool("invert_r", "Invert R", false));
        form.push(Field::choice("filter", "Filter", &["sobel", "scharr", "prewitt"], 0));
        form
    }

    #[test]
    fn nudge_moves_by_the_step_and_clamps() {
        let mut f = form();
        f.select(1);
        f.nudge(1);
        assert_eq!(f.get_f32("strength"), 2.6);
        f.nudge(-30);
        assert_eq!(f.get_f32("strength"), 0.1, "clamped low");
        f.nudge(1000);
        assert_eq!(f.get_f32("strength"), 20.0, "clamped high");

        f.select(2);
        f.nudge(3);
        assert_eq!(f.get_i64("blur"), 3);
        f.nudge(-99);
        assert_eq!(f.get_i64("blur"), 0);
    }

    #[test]
    fn toggle_flips_a_bool_and_cycles_a_choice_with_wrap() {
        let mut f = form();
        f.select(3);
        f.toggle();
        assert!(f.get_bool("invert_r"));
        f.toggle();
        assert!(!f.get_bool("invert_r"));

        f.select(4);
        f.toggle();
        assert_eq!(f.get_choice("filter"), "scharr");
        f.toggle();
        f.toggle();
        assert_eq!(f.get_choice("filter"), "sobel", "wrapped");
        f.nudge(-1);
        assert_eq!(f.get_choice("filter"), "prewitt", "h on a choice cycles back");
    }

    #[test]
    fn selection_walks_the_fields_with_counts_and_ends() {
        let mut f = form();
        f.select_by(2);
        assert_eq!(f.selected(), 2);
        f.select_by(-9);
        assert_eq!(f.selected(), 0);
        f.last();
        assert_eq!(f.selected(), 4);
        f.first();
        assert_eq!(f.selected(), 0);
    }

    #[test]
    fn set_by_name_parses_per_kind_and_refuses_with_the_options() {
        let mut f = form();
        f.set("strength", "3.25").unwrap();
        assert_eq!(f.get_f32("strength"), 3.25);
        f.set("strength", "99").unwrap();
        assert_eq!(f.get_f32("strength"), 20.0, "clamped, not refused");
        assert_eq!(f.set("strength", "wide"), Err("strength wants a number 0.1..20".into()));
        assert_eq!(f.set("blur", "2.5"), Err("blur wants a whole number 0..32".into()));
        f.set("invert_r", "true").unwrap();
        assert!(f.get_bool("invert_r"));
        assert_eq!(f.set("invert_r", "yes"), Err("invert_r wants true or false".into()));
        f.set("filter", "prewitt").unwrap();
        assert_eq!(f.get_choice("filter"), "prewitt");
        assert_eq!(
            f.set("filter", "box"),
            Err("filter wants one of sobel, scharr, prewitt".into())
        );
        assert_eq!(
            f.set("gain", "1"),
            Err("no field gain (map, strength, blur, invert_r, filter)".into())
        );
    }

    #[test]
    fn undo_and_redo_walk_the_changes_and_bump_the_generation() {
        let mut f = form();
        let g0 = f.generation();
        f.select(1);
        f.nudge(2);
        f.set("filter", "scharr").unwrap();
        assert!(f.generation() > g0);
        let g1 = f.generation();

        assert!(f.undo());
        assert_eq!(f.get_choice("filter"), "sobel");
        assert!(f.generation() > g1, "undo is a change the owner must see");
        assert!(f.undo());
        assert_eq!(f.get_f32("strength"), 2.5);
        assert!(!f.undo());

        assert!(f.redo());
        assert_eq!(f.get_f32("strength"), 2.7);
        f.nudge(1);
        assert!(!f.redo(), "a new change drops redo");
    }

    #[test]
    fn tab_cycles_the_field_named_map_from_anywhere() {
        let mut f = form();
        f.select(3);
        assert!(f.cycle_map());
        assert_eq!(f.get_choice("map"), "displacement");
        assert_eq!(f.selected(), 3, "without moving the selection");
        assert!(f.cycle_map());
        assert_eq!(f.get_choice("map"), "normal");

        let mut bare = Form::new("x", "X");
        bare.push(Field::bool("on", "On", false));
        assert!(!bare.cycle_map());
    }

    #[test]
    fn a_field_says_how_it_reads_and_how_full_it_is() {
        let f = form();
        assert_eq!(f.fields()[1].display(), "2.5");
        assert_eq!(f.fields()[2].display(), "0");
        assert_eq!(f.fields()[3].display(), "off");
        assert_eq!(f.fields()[4].display(), "sobel");
        assert!((f.fields()[1].fraction().unwrap() - (2.4 / 19.9)).abs() < 1e-5);
        assert_eq!(f.fields()[3].fraction(), None, "a bool has no slider");
        assert_eq!(f.value_text("strength"), Some("2.5".to_string()));
    }
}
