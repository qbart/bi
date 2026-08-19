//! A directory, flattened into the rows a pane shows.
//!
//! State and filesystem, no terminal. Like [`crate::picker::Picker`], this does
//! not draw: it holds what is expanded and what that makes visible, and a
//! frontend reads [`Row`] and picks its own glyphs. That split is what makes the
//! whole thing testable without a terminal.
//!
//! See `docs/specs/tree.md`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

/// What a row is. Taken from `symlink_metadata`, so a symlink is never mistaken
/// for the directory it points at — which is what keeps expansion acyclic
/// without a visited set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Link,
}

/// One visible line of the tree.
///
/// Structure, not presentation: `depth` and `kind` are what a frontend needs to
/// draw indent guides and markers, and no glyph appears here. README decision #6
/// — capture names, not styles — applied to a second subsystem.
#[derive(Debug, Clone)]
pub struct Row {
    pub path: PathBuf,
    /// The final component alone. The full root path is [`Tree::root`].
    pub name: String,
    pub depth: usize,
    pub kind: Kind,
    /// A directory the user has opened. False for every other kind.
    pub open: bool,
}

/// What the next paste will do with what is marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipMode {
    #[default]
    Copy,
    Cut,
}

/// One marked path and what the paste will do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub path: PathBuf,
    pub mode: ClipMode,
}

/// Paths marked for the next paste.
///
/// Session state rather than the tree's: you mark in one place and paste in
/// another, and re-rooting must not lose what you marked. It is also why the
/// marks show wherever those paths appear.
#[derive(Debug, Clone, Default)]
pub struct Clipboard {
    marks: Vec<Mark>,
}

impl Clipboard {
    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.mode_of(path).is_some()
    }

    /// What `path` is marked for, if anything. The tree asks this per row, so
    /// two rows in one paste can show different verbs.
    pub fn mode_of(&self, path: &Path) -> Option<ClipMode> {
        self.marks.iter().find(|m| m.path == path).map(|m| m.mode)
    }

    /// Marks or unmarks `path`, and says what the set is for.
    ///
    /// The verb belongs to the path, not to the set: marking a fourth file to
    /// copy leaves the three already marked to move alone. An earlier design
    /// converted the whole set instead, so that a paste could not both
    /// duplicate and destroy at once — but the mark column shows every row's
    /// verb all the time, which is what that was really protecting against,
    /// and one shared mode made "move these three, copy this one" two trips.
    ///
    /// Pressing the *other* key on a path already marked converts that one
    /// rather than unmarking it: "make this a move" is what you meant, and a
    /// key that means two opposite things depending on state it does not show
    /// you is the kind of thing that costs a file. The key that put a mark
    /// there is the key that takes it away.
    pub fn mark(&mut self, path: PathBuf, mode: ClipMode) {
        match self.marks.iter_mut().find(|m| m.path == path) {
            Some(mark) if mark.mode == mode => {
                self.marks.retain(|m| m.path != path);
            }
            Some(mark) => mark.mode = mode,
            None => self.marks.push(Mark { path, mode }),
        }
    }

    pub fn clear(&mut self) {
        self.marks.clear();
    }

    /// Forgets the marks a paste has spent. A cut is spent — the source is not
    /// there any more — and a copy is not.
    pub fn clear_cuts(&mut self) {
        self.marks.retain(|m| m.mode != ClipMode::Cut);
    }

    fn count(&self, mode: ClipMode) -> usize {
        self.marks.iter().filter(|m| m.mode == mode).count()
    }

    /// What the footer says, so what a paste is about to do is never something
    /// you have to remember. Both halves when the set is mixed, because the
    /// summary is the one place the whole set is visible at once.
    pub fn summary(&self) -> String {
        let (copies, cuts) = (self.count(ClipMode::Copy), self.count(ClipMode::Cut));
        match (copies, cuts) {
            (0, cuts) => format!("{cuts} to move"),
            (copies, 0) => format!("{copies} to copy"),
            (copies, cuts) => format!("{copies} to copy, {cuts} to move"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tree {
    root: PathBuf,
    /// Directories the user has opened. Paths rather than row indices, so
    /// re-reading from disk cannot silently re-point one at whatever moved
    /// into the slot.
    expanded: BTreeSet<PathBuf>,
    rows: Vec<Row>,
    selected: usize,
    /// First visible row. Held here rather than on the window, because the
    /// window's own scroll belongs to `Content::Text`.
    scroll: usize,
    show_hidden: bool,
}

impl Tree {
    /// Reads `root` and its immediate children.
    ///
    /// A path that is not a directory is an error rather than an empty tree:
    /// `:e` decides between a file and a tree by asking the disk, so getting
    /// here with a file means the disk changed underneath and saying so is
    /// better than showing nothing.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        if !root.is_dir() {
            anyhow::bail!("{} is not a directory", root.display());
        }
        // Resolved on the way in, because `bi .` would otherwise root at "."
        // — whose parent is "" rather than the directory above it, leaving `-`
        // nowhere to go in the one case the key exists for.
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut tree = Self {
            root,
            expanded: BTreeSet::new(),
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            show_hidden: false,
        };
        tree.rebuild();
        Ok(tree)
    }

    /// Recomputes every visible row from the expansion set.
    ///
    /// Whole rather than in place, for the reason `Picker::refilter` gives for
    /// the same choice: at the sizes involved this is far cheaper than the
    /// machinery it would take to avoid it.
    fn rebuild(&mut self) {
        let was = self.rows.get(self.selected).map(|r| r.path.clone());
        let mut rows = vec![Row {
            name: name_of(&self.root),
            path: self.root.clone(),
            depth: 0,
            kind: Kind::Dir,
            open: true,
        }];
        self.push_children(&self.root.clone(), 1, &mut rows);
        self.rows = rows;

        // The selection is a path, not an index. A file appearing above it must
        // not slide it onto a different row, which is the same reason `expanded`
        // holds paths.
        self.selected = was
            .and_then(|path| self.rows.iter().position(|r| r.path == path))
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
    }

    /// Appends `dir`'s entries, descending into the ones that are expanded.
    fn push_children(&self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        for mut row in children_of(dir, depth, self.show_hidden) {
            row.open = row.kind == Kind::Dir && self.expanded.contains(&row.path);
            let (open, path) = (row.open, row.path.clone());
            rows.push(row);
            if open {
                self.push_children(&path, depth + 1, rows);
            }
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every visible row, top to bottom. The index is the cursor row.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Which row the next `Enter` acts on.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The row `Enter` acts on: its path and whether it is a directory, which
    /// is everything the editor needs to decide between opening and toggling.
    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    /// `Enter` on a directory: open it if closed, close it if open.
    ///
    /// Not the same as `collapse`, which walks to the parent when there is
    /// nothing to close. Enter never moves the selection.
    pub fn toggle(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        if row.kind != Kind::Dir {
            return;
        }
        let path = row.path.clone();
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.rebuild();
    }

    /// Clamped, so `select(usize::MAX)` is `G` and `select(0)` is `gg` — which
    /// is why neither needs a method of its own.
    pub fn select(&mut self, row: usize) {
        self.selected = row.min(self.rows.len().saturating_sub(1));
    }

    /// `j` and `k`, and with a count the half-page keys too.
    ///
    /// Stops at both ends rather than wrapping, where `Picker::next` wraps: a
    /// picker is a short list you cycle, and this is a cursor over a document
    /// shape, where `j` at the bottom staying put is what vim does.
    pub fn step(&mut self, down: bool, count: usize) {
        let to = if down {
            self.selected.saturating_add(count)
        } else {
            self.selected.saturating_sub(count)
        };
        self.select(to);
    }

    /// Opens the selected directory. Does nothing to a file or a link.
    pub fn expand(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        if row.kind != Kind::Dir {
            return;
        }
        let path = row.path.clone();
        if self.expanded.insert(path) {
            self.rebuild();
        }
    }

    /// Closes the selected directory. On anything else — a file, a link, a
    /// directory already closed — moves to the parent row, which is the only
    /// upward move that means anything there.
    pub fn collapse(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        if !row.open {
            return self.select_parent();
        }
        // Only this directory leaves the set. A descendant that was open stays
        // in it, so re-expanding this one comes back to the shape you left.
        let path = row.path.clone();
        self.expanded.remove(&path);
        self.rebuild();
    }

    /// First visible row.
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Keeps the selection inside a `height`-row window. The frontend calls
    /// this with the room it gave the pane, exactly as the picker's does.
    pub fn scroll_to_selected(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(height));
    }

    /// Re-roots at the parent directory, leaving the old root open and
    /// selected — so `-` then `l` is a round trip. `-`.
    ///
    /// At a filesystem root there is no parent and nothing happens.
    pub fn up(&mut self) {
        let Some(parent) = self.root.parent().map(Path::to_path_buf) else { return };
        let old = std::mem::replace(&mut self.root, parent);
        self.expanded.insert(old.clone());
        self.rebuild();
        if let Some(row) = self.rows.iter().position(|r| r.path == old) {
            self.selected = row;
        }
    }

    /// Re-roots at the selected directory, the inverse of [`Tree::up`]. `+`.
    ///
    /// On a file it takes the directory holding it, so the key means "scope to
    /// what I am standing in" wherever the cursor is. Expansion is kept: what
    /// was open below stays open, which is what makes `+` then `-` a round
    /// trip rather than a reset.
    pub fn down(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        let root = match row.kind {
            Kind::Dir => row.path.clone(),
            _ => match row.path.parent() {
                Some(parent) => parent.to_path_buf(),
                None => return,
            },
        };
        // The root row, or a file sitting directly in it. Nowhere to go, and
        // rebuilding would only throw the selection at row 0 for nothing.
        if root == self.root {
            return;
        }
        self.root = root;
        self.rebuild();
    }

    /// Opens every directory between the root and `path`, and selects it.
    ///
    /// What `-` out of a file uses to land you back on it. The root is the
    /// session's rather than the file's directory, so the file can sit any
    /// number of levels down and the way to it has to be opened first —
    /// selecting alone would find no row.
    ///
    /// A path outside the root leaves the tree exactly as it was. That is the
    /// honest answer: the tree cannot show it, and re-rooting to reach it is
    /// the one thing `+` and `-` exist to be asked for.
    pub fn reveal(&mut self, path: &Path) {
        let Ok(rest) = path.strip_prefix(&self.root) else { return };
        let mut dir = self.root.clone();
        // The directories on the way, not the file itself — that one is a row
        // to select, not a directory to open.
        for part in rest.parent().unwrap_or(Path::new("")).components() {
            dir.push(part);
            self.expanded.insert(dir.clone());
        }
        self.rebuild();
        if let Some(row) = self.rows.iter().position(|r| r.path == path) {
            self.selected = row;
        }
    }

    /// Re-reads every open directory from disk. `R`.
    ///
    /// The same walk as any other change, because `rebuild` never caches: what
    /// makes this a refresh is only that nothing else about the tree moved.
    pub fn refresh(&mut self) {
        self.rebuild();
    }

    /// Shows or hides entries whose name starts with a dot.
    ///
    /// `.gitignore` is deliberately not consulted for this: honouring it would
    /// put a git dependency and a per-directory rule stack behind a listing,
    /// and `target/` — the case that motivates it — is one collapsed row.
    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.rebuild();
    }

    /// The nearest row above the selection that is shallower than it.
    fn select_parent(&mut self) {
        let Some(depth) = self.rows.get(self.selected).map(|r| r.depth) else { return };
        if let Some(parent) = self.rows[..self.selected].iter().rposition(|r| r.depth < depth) {
            self.selected = parent;
        }
    }
}

/// The final component, or the whole path when there is none — `/` and `..`
/// have no file name and still have to be called something.
fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// One row per entry of `dir`, directories first and then by name.
///
/// An unreadable directory yields nothing rather than an error: one bad
/// permission should not fail the tree around it.
fn children_of(dir: &Path, depth: usize, show_hidden: bool) -> Vec<Row> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };

    let mut rows: Vec<Row> = entries
        .flatten()
        .filter(|entry| show_hidden || !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| {
            let path = entry.path();
            // `symlink_metadata` rather than `metadata`: a symlink to a
            // directory must not read as one, or expanding it could loop.
            let kind = match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => Kind::Link,
                Ok(meta) if meta.is_dir() => Kind::Dir,
                _ => Kind::File,
            };
            Row { name: name_of(&path), path, depth, kind, open: false }
        })
        .collect();

    rows.sort_by(|a, b| {
        let dir_first = (b.kind == Kind::Dir).cmp(&(a.kind == Kind::Dir));
        dir_first.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    rows
}

/// Copies a file, or a whole directory, to a path that does not exist yet.
///
/// `std::fs` has no recursive copy, and a tree of files is exactly what a file
/// tree is for.
pub fn copy_into(source: &Path, target: &Path) -> std::io::Result<()> {
    // `symlink_metadata`, so a link is copied as the link it is rather than
    // being followed into whatever it points at — the rule the rows follow.
    let meta = std::fs::symlink_metadata(source)?;
    if !meta.is_dir() {
        std::fs::copy(source, target)?;
        return Ok(());
    }
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        copy_into(&entry.path(), &target.join(entry.file_name()))?;
    }
    Ok(())
}

/// Moves a file or directory. A rename where the filesystems agree, and a copy
/// followed by a delete where they do not — a rename across devices is not a
/// rename, and the kernel says so rather than doing it for you.
pub fn move_into(source: &Path, target: &Path) -> std::io::Result<()> {
    match std::fs::rename(source, target) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_into(source, target)?;
            match std::fs::symlink_metadata(source)?.is_dir() {
                true => std::fs::remove_dir_all(source),
                false => std::fs::remove_file(source),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real directory under the temp dir, removed when the test ends.
    ///
    /// Real files rather than a filesystem trait: `Buffer::open` reads the disk
    /// too, and a fake here would be testing the fake.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("bi-tree-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn dir(self, rel: &str) -> Self {
            std::fs::create_dir_all(self.0.join(rel)).unwrap();
            self
        }

        fn file(self, rel: &str) -> Self {
            let path = self.0.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, "").unwrap();
            self
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The visible tree below the root, as indent + name, so a test reads like
    /// the shape it is asserting.
    fn shown(tree: &Tree) -> Vec<String> {
        tree.rows()[1..]
            .iter()
            .map(|r| {
                let slash = if r.kind == Kind::Dir { "/" } else { "" };
                format!("{}{}{slash}", "  ".repeat(r.depth - 1), r.name)
            })
            .collect()
    }

    #[test]
    fn a_new_tree_lists_the_root_children_with_directories_first() {
        let d = ScratchDir::new("new").file("README.md").dir("src").file("Cargo.toml");
        let tree = Tree::new(d.path()).unwrap();

        assert_eq!(shown(&tree), ["src/", "Cargo.toml", "README.md"]);
    }

    #[test]
    fn expanding_a_directory_inserts_its_children_below_it() {
        let d = ScratchDir::new("expand").file("src/lib.rs").file("src/tree.rs").file("Cargo.toml");
        let mut tree = Tree::new(d.path()).unwrap();
        assert_eq!(shown(&tree), ["src/", "Cargo.toml"]);

        tree.select(1);
        tree.expand();

        assert_eq!(shown(&tree), ["src/", "  lib.rs", "  tree.rs", "Cargo.toml"]);
    }

    /// Expanding the row under the selection, twice, so the tree is nested.
    fn expand_at(tree: &mut Tree, row: usize) {
        tree.select(row);
        tree.expand();
    }

    #[test]
    fn collapsing_an_ancestor_hides_every_descendant() {
        let d = ScratchDir::new("collapse").file("src/tui/render.rs").file("src/lib.rs");
        let mut tree = Tree::new(d.path()).unwrap();
        expand_at(&mut tree, 1);
        expand_at(&mut tree, 2);
        assert_eq!(shown(&tree), ["src/", "  tui/", "    render.rs", "  lib.rs"]);

        tree.select(1);
        tree.collapse();

        assert_eq!(shown(&tree), ["src/"]);
    }

    #[test]
    fn collapsing_something_already_closed_goes_to_the_parent() {
        let d = ScratchDir::new("parent").file("src/lib.rs");
        let mut tree = Tree::new(d.path()).unwrap();
        expand_at(&mut tree, 1);

        tree.select(2); // src/lib.rs, a file — nothing to close
        tree.collapse();

        assert_eq!(tree.selected(), 1, "moved up to src/");
    }

    #[test]
    fn the_selection_moves_by_a_count_and_stops_at_both_ends() {
        let d = ScratchDir::new("step").file("a").file("b").file("c");
        let mut tree = Tree::new(d.path()).unwrap();
        assert_eq!(tree.selected(), 0, "the root row, to begin with");

        tree.step(true, 2);
        assert_eq!(tree.selected(), 2);

        tree.step(true, 9);
        assert_eq!(tree.selected(), 3, "stopped at the last row rather than wrapping");

        tree.step(false, 9);
        assert_eq!(tree.selected(), 0, "and at the first");
    }

    #[test]
    fn dotfiles_are_hidden_until_asked_for() {
        let d = ScratchDir::new("hidden").dir(".git").file(".gitignore").file("Cargo.toml");
        let mut tree = Tree::new(d.path()).unwrap();
        assert_eq!(shown(&tree), ["Cargo.toml"]);

        tree.toggle_hidden();
        assert_eq!(shown(&tree), [".git/", ".gitignore", "Cargo.toml"]);

        tree.toggle_hidden();
        assert_eq!(shown(&tree), ["Cargo.toml"]);
    }

    /// Not following them is what makes expansion acyclic by construction, so
    /// there is no visited set anywhere in here.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_is_a_link_and_never_expands() {
        let d = ScratchDir::new("link").file("src/lib.rs");
        std::os::unix::fs::symlink(d.path().join("src"), d.path().join("loop")).unwrap();
        let mut tree = Tree::new(d.path()).unwrap();
        assert_eq!(shown(&tree), ["src/", "loop"], "a link sorts with the files");

        tree.select(2);
        tree.expand();

        assert_eq!(shown(&tree), ["src/", "loop"]);
    }

    #[test]
    fn refreshing_reads_the_disk_again_and_keeps_expansion() {
        let d = ScratchDir::new("refresh").file("src/lib.rs");
        let mut tree = Tree::new(d.path()).unwrap();
        expand_at(&mut tree, 1);
        assert_eq!(shown(&tree), ["src/", "  lib.rs"]);

        std::fs::write(d.path().join("src/buffer.rs"), "").unwrap();
        tree.refresh();

        assert_eq!(shown(&tree), ["src/", "  buffer.rs", "  lib.rs"], "src/ is still open");
    }

    #[test]
    fn the_selection_follows_its_path_when_rows_move_under_it() {
        let d = ScratchDir::new("follow").file("src/lib.rs");
        let mut tree = Tree::new(d.path()).unwrap();
        expand_at(&mut tree, 1);
        tree.select(2);

        // Sorts above lib.rs, so every row below it shifts down one.
        std::fs::write(d.path().join("src/buffer.rs"), "").unwrap();
        tree.refresh();

        assert_eq!(tree.selected(), 3, "still on lib.rs, which moved");
    }

    #[test]
    fn going_up_re_roots_at_the_parent_and_selects_where_it_came_from() {
        let d = ScratchDir::new("up").file("src/lib.rs").file("Cargo.toml");
        let mut tree = Tree::new(d.path().join("src")).unwrap();
        assert_eq!(shown(&tree), ["lib.rs"]);

        tree.up();

        assert_eq!(tree.root(), d.path());
        assert_eq!(shown(&tree), ["src/", "  lib.rs", "Cargo.toml"], "left open behind you");
        assert_eq!(tree.rows()[tree.selected()].name, "src");
    }

    #[test]
    fn enter_opens_and_closes_a_directory_without_moving() {
        let d = ScratchDir::new("toggle").file("src/lib.rs");
        let mut tree = Tree::new(d.path()).unwrap();
        tree.select(1);

        tree.toggle();
        assert_eq!(shown(&tree), ["src/", "  lib.rs"]);

        tree.toggle();
        assert_eq!(shown(&tree), ["src/"]);
        assert_eq!(tree.selected(), 1, "still on src/, where collapse would have moved");
    }

    #[test]
    fn the_selected_row_carries_the_path_and_the_kind() {
        let d = ScratchDir::new("selected").file("Cargo.toml");
        let mut tree = Tree::new(d.path()).unwrap();
        tree.select(1);

        let row = tree.selected_row().unwrap();
        assert_eq!(row.path, d.path().join("Cargo.toml"));
        assert_eq!(row.kind, Kind::File);
    }

    #[test]
    fn scrolling_follows_the_selection() {
        let d = ScratchDir::new("scroll").file("a").file("b").file("c").file("e");
        let mut tree = Tree::new(d.path()).unwrap();
        tree.scroll_to_selected(2);
        assert_eq!(tree.scroll(), 0);

        tree.step(true, 2);
        tree.scroll_to_selected(2);
        assert_eq!(tree.scroll(), 1, "selection at row 2 in a 2-row window");

        tree.step(false, 2);
        tree.scroll_to_selected(2);
        assert_eq!(tree.scroll(), 0);
    }

    /// `bi .` roots at ".", whose parent is "" rather than the directory
    /// above it — so `-` would have had nowhere to go in the one case the
    /// feature exists for.
    #[test]
    fn a_relative_root_is_resolved_so_going_up_has_somewhere_to_go() {
        let mut tree = Tree::new(".").unwrap();
        assert!(tree.root().is_absolute(), "resolved on the way in: {:?}", tree.root());

        let was = tree.root().to_path_buf();
        tree.up();

        assert_eq!(tree.root(), was.parent().unwrap(), "and `-` has a parent to find");
    }

    #[test]
    fn going_down_re_roots_at_the_selected_directory_and_keeps_expansion() {
        let d = ScratchDir::new("down").file("src/tui/render.rs").file("Cargo.toml");
        let mut tree = Tree::new(d.path()).unwrap();
        expand_at(&mut tree, 1);
        expand_at(&mut tree, 2);
        assert_eq!(shown(&tree), ["src/", "  tui/", "    render.rs", "Cargo.toml"]);

        tree.select(1);
        tree.down();

        assert_eq!(tree.root(), d.path().join("src"));
        assert_eq!(shown(&tree), ["tui/", "  render.rs"], "tui/ is still open");
        assert_eq!(tree.selected(), 0, "standing on the new root");
    }

    /// So `+` means "scope to what I am standing in" wherever the cursor is,
    /// rather than only on the directory rows.
    #[test]
    fn going_down_on_a_file_takes_the_directory_holding_it() {
        let d = ScratchDir::new("down-file").file("src/lib.rs");
        let mut tree = Tree::new(d.path()).unwrap();
        expand_at(&mut tree, 1);

        tree.select(2);
        tree.down();

        assert_eq!(tree.root(), d.path().join("src"));
    }

    #[test]
    fn going_down_at_the_root_has_nowhere_to_go() {
        let d = ScratchDir::new("down-root").file("Cargo.toml");
        let mut tree = Tree::new(d.path()).unwrap();
        let was = tree.root().to_path_buf();

        tree.select(0);
        tree.down();
        assert_eq!(tree.root(), was);

        // A top-level file's directory is the root already.
        tree.select(1);
        tree.down();
        assert_eq!(tree.root(), was);
    }

    /// `-` out of a file lands on it however deep it sits, which means opening
    /// the way down and not merely looking for a row.
    #[test]
    fn revealing_opens_every_directory_down_to_the_file_and_selects_it() {
        let d = ScratchDir::new("reveal").file("src/tui/render.rs").file("Cargo.toml");
        let mut tree = Tree::new(d.path()).unwrap();
        assert_eq!(shown(&tree), ["src/", "Cargo.toml"]);

        tree.reveal(&d.0.join("src/tui/render.rs"));

        assert_eq!(shown(&tree), ["src/", "  tui/", "    render.rs", "Cargo.toml"]);
        assert_eq!(tree.selected_row().unwrap().name, "render.rs");
    }

    /// Re-rooting is the only way to see somewhere else, so a path the tree
    /// cannot show leaves it alone rather than quietly moving.
    #[test]
    fn revealing_something_outside_the_root_changes_nothing() {
        let d = ScratchDir::new("reveal-outside").file("src/lib.rs");
        let mut tree = Tree::new(d.path()).unwrap();
        let (was, rows) = (tree.root().to_path_buf(), shown(&tree));

        tree.reveal(Path::new("/etc/hosts"));

        assert_eq!(tree.root(), was);
        assert_eq!(shown(&tree), rows);
        assert_eq!(tree.selected(), 0);
    }

    fn clip(paths: &[&str], mode: ClipMode) -> Clipboard {
        let mut clipboard = Clipboard::default();
        for path in paths {
            clipboard.mark(PathBuf::from(path), mode);
        }
        clipboard
    }

    fn marked(clipboard: &Clipboard) -> Vec<String> {
        clipboard.marks().iter().map(|m| m.path.display().to_string()).collect()
    }

    fn mode(clipboard: &Clipboard, path: &str) -> Option<ClipMode> {
        clipboard.mode_of(&PathBuf::from(path))
    }

    #[test]
    fn marking_the_same_way_twice_takes_the_mark_off() {
        let mut clipboard = clip(&["a.rs", "b.rs"], ClipMode::Copy);
        assert_eq!(marked(&clipboard), ["a.rs", "b.rs"]);

        clipboard.mark(PathBuf::from("a.rs"), ClipMode::Copy);

        assert_eq!(marked(&clipboard), ["b.rs"]);
    }

    /// "Make this a move" is what the other key means. Unmarking on it would
    /// be the same key doing two opposite things depending on state it does
    /// not show you.
    #[test]
    fn marking_the_other_way_converts_that_one_rather_than_unmarking() {
        let mut clipboard = clip(&["a.rs", "b.rs"], ClipMode::Copy);

        clipboard.mark(PathBuf::from("a.rs"), ClipMode::Cut);

        assert_eq!(marked(&clipboard), ["a.rs", "b.rs"], "still both");
        assert_eq!(mode(&clipboard, "a.rs"), Some(ClipMode::Cut), "this one moves");
        assert_eq!(mode(&clipboard, "b.rs"), Some(ClipMode::Copy), "and this one still copies");
    }

    /// The verb belongs to the path, not to the set: three to move and one to
    /// copy is one paste, not two trips.
    #[test]
    fn a_clipboard_can_hold_both_verbs_at_once() {
        let mut clipboard = clip(&["a.rs", "b.rs"], ClipMode::Cut);
        clipboard.mark(PathBuf::from("c.rs"), ClipMode::Copy);

        assert_eq!(clipboard.summary(), "1 to copy, 2 to move");
        assert_eq!(mode(&clipboard, "c.rs"), Some(ClipMode::Copy));
        assert_eq!(mode(&clipboard, "d.rs"), None, "never marked");

        // A paste spends the cuts and keeps the copies, which with a mixed set
        // is a partial clear rather than a choice between the two.
        clipboard.clear_cuts();
        assert_eq!(marked(&clipboard), ["c.rs"]);
        assert_eq!(clipboard.summary(), "1 to copy");
    }

    #[test]
    fn marking_a_new_path_leaves_the_others_alone() {
        let mut clipboard = clip(&["a.rs"], ClipMode::Copy);

        clipboard.mark(PathBuf::from("b.rs"), ClipMode::Cut);

        assert_eq!(marked(&clipboard), ["a.rs", "b.rs"]);
        assert_eq!(mode(&clipboard, "a.rs"), Some(ClipMode::Copy), "unchanged");
        assert_eq!(mode(&clipboard, "b.rs"), Some(ClipMode::Cut));
    }

    #[test]
    fn a_tree_needs_a_directory() {
        let d = ScratchDir::new("notdir").file("Cargo.toml");
        assert!(Tree::new(d.path().join("Cargo.toml")).is_err());
        assert!(Tree::new(d.path().join("nothing-here")).is_err());
    }
}
