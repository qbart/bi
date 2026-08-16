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
        let mut tree = Self {
            root: root.to_path_buf(),
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
            let path = std::env::temp_dir().join(format!("bee-tree-{}-{name}", std::process::id()));
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

    #[test]
    fn a_tree_needs_a_directory() {
        let d = ScratchDir::new("notdir").file("Cargo.toml");
        assert!(Tree::new(d.path().join("Cargo.toml")).is_err());
        assert!(Tree::new(d.path().join("nothing-here")).is_err());
    }
}
