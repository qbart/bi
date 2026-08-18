//! Windows, and the tree that arranges them.
//!
//! Geometry is here rather than in the frontend: "which pane is left of this
//! one" is not a question about terminals, and a second frontend should not
//! have to answer it again. [`Rect`] is four integers and names nothing.
//!
//! See `docs/specs/windows.md`.

use crate::buffer::BufferId;
use crate::selection::Selections;
use crate::tree::Tree;

/// A window's identity, stable across splits and closes.
///
/// Handed out monotonically and never reused within a session, so nothing has
/// to be fixed up when a neighbour closes — which is exactly what an index into
/// a `Vec` would need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u32);

/// What a window is showing.
///
/// The tree lives in the window rather than in a list beside the buffers,
/// because the reason buffers are shared does not apply to it: two ropes over
/// one path would diverge, and a tree has no document to diverge. What two tree
/// panes would share is expansion state, and wanting a different one is why you
/// opened the second pane. See `docs/specs/tree.md`.
#[derive(Debug, Clone)]
pub enum Content {
    Text(Text),
    Tree(Tree),
}

/// Which keymap a window wants, and which renderer.
///
/// An enum rather than an `is_tree` flag: the next pane kind should be a
/// variant and a compiler error, not a second boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Text,
    Tree,
}

impl Content {
    pub fn kind(&self) -> ContentKind {
        match self {
            Content::Text(_) => ContentKind::Text,
            Content::Tree(_) => ContentKind::Tree,
        }
    }

    /// The buffer behind this content, if it is text at all.
    pub fn buffer(&self) -> Option<BufferId> {
        match self {
            Content::Text(text) => Some(text.buffer),
            Content::Tree(_) => None,
        }
    }
}

/// A window's view onto a buffer: which one, and where in it.
///
/// The cursor and the scroll row sit in here rather than on [`Window`] so that a
/// tree pane cannot carry a `Selections` that means nothing. It is also what
/// lets `Editor::view` hand out the selections directly and refuse a tree
/// outright, so the editing commands never learn that trees exist.
#[derive(Debug, Clone)]
pub struct Text {
    pub buffer: BufferId,
    pub selections: Selections,
    /// First visible row.
    pub scroll: usize,
}

/// One view onto one buffer, or onto one directory.
///
/// Everything here is per-view rather than per-file: two windows on one buffer
/// have their own cursor and their own scroll, which is the point of opening
/// the second one.
#[derive(Debug, Clone)]
pub struct Window {
    pub id: WindowId,
    pub content: Content,
    /// What this window showed before, for `Ctrl-^` and `:b#`.
    ///
    /// A whole `Content` rather than a `BufferId`, because opening a file from a
    /// tree in a single-window session displaces the tree, and `Ctrl-^` has to
    /// bring it back with its expansion rather than re-reading the directory.
    pub alt: Option<Content>,
    /// The text area the frontend last drew, which is all the scrolling
    /// commands need — and the reason they need no viewport type.
    pub height: usize,
    pub width: usize,
}

impl Window {
    pub fn new(id: WindowId, buffer: BufferId) -> Self {
        Self::showing(id, Content::Text(Text::new(buffer)))
    }

    pub fn showing(id: WindowId, content: Content) -> Self {
        Self { id, content, alt: None, height: 0, width: 0 }
    }

    /// The buffer this window shows, if it shows one at all.
    pub fn buffer(&self) -> Option<BufferId> {
        self.content.buffer()
    }

    /// The buffer this window showed before, for `Ctrl-^` and `:b#`.
    pub fn alt_buffer(&self) -> Option<BufferId> {
        self.alt.as_ref().and_then(Content::buffer)
    }

    pub fn text(&self) -> Option<&Text> {
        match &self.content {
            Content::Text(text) => Some(text),
            Content::Tree(_) => None,
        }
    }

    pub fn text_mut(&mut self) -> Option<&mut Text> {
        match &mut self.content {
            Content::Text(text) => Some(text),
            Content::Tree(_) => None,
        }
    }

    pub fn tree(&self) -> Option<&Tree> {
        match &self.content {
            Content::Tree(tree) => Some(tree),
            Content::Text(_) => None,
        }
    }

    pub fn tree_mut(&mut self) -> Option<&mut Tree> {
        match &mut self.content {
            Content::Tree(tree) => Some(tree),
            Content::Text(_) => None,
        }
    }

    /// Shows `content`, keeping what was here as the alternate.
    pub fn show(&mut self, content: Content) {
        self.alt = Some(std::mem::replace(&mut self.content, content));
    }
}

impl Text {
    pub fn new(buffer: BufferId) -> Self {
        Self { buffer, selections: Selections::default(), scroll: 0 }
    }
}

/// Which side of the window being split the new one takes.
///
/// `After` is what a split wants: the new window opens *beside* what you were
/// reading rather than on top of the space it occupied, so focus moving into
/// it is something you can see. Vim's default is `Before` — and the first
/// thing most vim users do is set `splitright` and `splitbelow` to get this.
///
/// `Before` is still what the tree sidebar wants, because a file tree belongs
/// on the left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Before,
    After,
}

/// How a split divides its children.
///
/// `Vertical` divides with a vertical line — children side by side, which is
/// what `:vsplit` makes. `Horizontal` stacks them, which is `:split`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Horizontal,
    Vertical,
}

/// A direction to look in, for `Ctrl-W h j k l`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Up,
    Down,
}

impl Side {
    /// The axis this side moves along.
    fn axis(self) -> Dir {
        match self {
            Side::Left | Side::Right => Dir::Vertical,
            Side::Up | Side::Down => Dir::Horizontal,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Node {
    Leaf(WindowId),
    Split { dir: Dir, children: Vec<Child> },
}

/// A child and its share of the parent.
///
/// The weight rides on the child rather than in a parallel `Vec<f32>`, so "one
/// weight per child" is a type rather than an invariant to maintain by hand
/// across every split and close.
#[derive(Debug, Clone)]
pub struct Child {
    /// Share of the parent's extent along its `dir`. Siblings sum to 1.
    pub weight: f32,
    pub node: Node,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self { x, y, width, height }
    }

    fn right(self) -> u16 {
        self.x + self.width
    }

    fn bottom(self) -> u16 {
        self.y + self.height
    }

    /// Whether two rects overlap along the axis `side` does *not* travel —
    /// the test for "is this one actually beside me, or merely past me".
    fn overlaps(self, other: Rect, side: Side) -> bool {
        match side.axis() {
            Dir::Vertical => self.y < other.bottom() && other.y < self.bottom(),
            Dir::Horizontal => self.x < other.right() && other.x < self.right(),
        }
    }
}

/// What the frontend reserves, and the smallest pane it can draw in.
///
/// All of it is the frontend's business rather than the tree's: a status row
/// is a terminal convention, and a core that hard-coded one would be quietly
/// asserting every frontend has it.
#[derive(Debug, Clone, Copy)]
pub struct Chrome {
    /// Columns between the children of a `Vertical` split.
    pub columns: u16,
    /// Rows between the children of a `Horizontal` split.
    pub rows: u16,
    pub min_width: u16,
    pub min_height: u16,
    /// How wide a tree pane opens. A judgement about reading, not a fact about
    /// geometry, so it sits here with the frontend's other judgements rather
    /// than being a number in the core. It is a starting width: the pane keeps
    /// its share of the terminal from then on, like every other pane.
    pub tree_width: u16,
}

impl Default for Chrome {
    fn default() -> Self {
        Self { columns: 0, rows: 0, min_width: 1, min_height: 1, tree_width: 1 }
    }
}

impl Chrome {
    /// The gap between children of a split running `dir`.
    fn gap(&self, dir: Dir) -> u16 {
        match dir {
            Dir::Vertical => self.columns,
            Dir::Horizontal => self.rows,
        }
    }

    /// The floor along `dir`'s axis.
    fn min(&self, dir: Dir) -> u16 {
        match dir {
            Dir::Vertical => self.min_width,
            Dir::Horizontal => self.min_height,
        }
    }
}

/// The window tree.
///
/// Holds only ids at its leaves; the windows themselves live in a flat list on
/// `Editor`. Splitting and closing are then pure tree surgery with no state to
/// carry along.
#[derive(Debug, Clone)]
pub struct Layout {
    root: Node,
}

impl Layout {
    pub fn new(first: WindowId) -> Self {
        Self { root: Node::Leaf(first) }
    }

    pub fn root(&self) -> &Node {
        &self.root
    }

    /// Every window, in layout order: top to bottom, left to right.
    pub fn leaves(&self) -> Vec<WindowId> {
        let mut out = Vec::new();
        collect(&self.root, &mut out);
        out
    }

    pub fn len(&self) -> usize {
        self.leaves().len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn contains(&self, id: WindowId) -> bool {
        self.leaves().contains(&id)
    }

    /// Splits `at` in two, putting `new` first — above for a horizontal split,
    /// left for a vertical one, which is where vim puts it.
    ///
    /// Returns whether there was room. A split that cannot give both halves
    /// `chrome`'s floor is refused rather than making a pane with nothing in
    /// it.
    pub fn split(
        &mut self,
        at: WindowId,
        new: WindowId,
        dir: Dir,
        place: Place,
        area: Rect,
        chrome: &Chrome,
    ) -> bool {
        let Some(rect) = self.rect_of(at, area, chrome) else { return false };
        // Both halves and the gap between them have to fit.
        if extent(rect, dir) < chrome.min(dir) * 2 + chrome.gap(dir) {
            return false;
        }
        split_at(&mut self.root, at, new, dir, place);
        true
    }

    /// Removes `id` and gives its space to the sibling that grows into it.
    ///
    /// Returns where focus goes: the first leaf of that sibling in layout
    /// order — the top-left-most window of the subtree that grew. `None` when
    /// `id` was the only window, which the caller refuses.
    pub fn close(&mut self, id: WindowId) -> Option<WindowId> {
        if self.leaves().len() < 2 {
            return None;
        }
        let heir = close_at(&mut self.root, id)?;
        Some(heir)
    }

    /// Closes everything but `keep`.
    pub fn only(&mut self, keep: WindowId) {
        if self.contains(keep) {
            self.root = Node::Leaf(keep);
        }
    }

    /// One rect per window, tiling `area` exactly.
    pub fn rects(&self, area: Rect, chrome: &Chrome) -> Vec<(WindowId, Rect)> {
        let mut out = Vec::new();
        place(&self.root, area, chrome, &mut out);
        out
    }

    pub fn rect_of(&self, id: WindowId, area: Rect, chrome: &Chrome) -> Option<Rect> {
        self.rects(area, chrome).into_iter().find(|&(w, _)| w == id).map(|(_, r)| r)
    }

    /// The window `side` of `from`, or `None` at the edge of the screen.
    ///
    /// The nearest one whose rect overlaps `from`'s span on the other axis;
    /// ties break towards `anchor`, which is where the cursor is on the axis
    /// being travelled across.
    ///
    /// Geometry rather than tree structure, because the tree's idea of "next
    /// child" and the screen's idea of "to the right" stop agreeing the moment
    /// layouts nest.
    pub fn neighbour(
        &self,
        from: WindowId,
        side: Side,
        area: Rect,
        chrome: &Chrome,
        anchor: u16,
    ) -> Option<WindowId> {
        let rects = self.rects(area, chrome);
        let me = rects.iter().find(|&&(id, _)| id == from).map(|&(_, r)| r)?;

        rects
            .iter()
            .filter(|&&(id, _)| id != from)
            .filter(|&&(_, r)| match side {
                Side::Left => r.right() <= me.x,
                Side::Right => r.x >= me.right(),
                Side::Up => r.bottom() <= me.y,
                Side::Down => r.y >= me.bottom(),
            })
            .filter(|&&(_, r)| me.overlaps(r, side))
            .min_by_key(|&&(_, r)| {
                // Distance in the direction travelled first, then how far the
                // candidate sits from the cursor on the other axis.
                let gap = match side {
                    Side::Left => me.x - r.right(),
                    Side::Right => r.x - me.right(),
                    Side::Up => me.y - r.bottom(),
                    Side::Down => r.y - me.bottom(),
                };
                let off = match side.axis() {
                    Dir::Vertical => distance(anchor, r.y, r.bottom()),
                    Dir::Horizontal => distance(anchor, r.x, r.right()),
                };
                (gap, off)
            })
            .map(|&(id, _)| id)
    }

    /// The window after `from` in layout order, wrapping. `back` goes the other
    /// way. This is `Ctrl-W w` / `Ctrl-W W`.
    pub fn cycle(&self, from: WindowId, back: bool) -> Option<WindowId> {
        let leaves = self.leaves();
        let i = leaves.iter().position(|&id| id == from)?;
        let n = leaves.len();
        let next = if back { (i + n - 1) % n } else { (i + 1) % n };
        Some(leaves[next])
    }

    /// Grows `id` by `cells` along `axis`, taking the space from a sibling.
    ///
    /// Acts on the deepest ancestor split that runs along `axis` — the one
    /// whose divider the user is actually pushing. Returns whether anything
    /// moved: at the top level there is no such ancestor, and at the floor
    /// there is no room.
    pub fn resize(
        &mut self,
        id: WindowId,
        axis: Dir,
        cells: i32,
        area: Rect,
        chrome: &Chrome,
    ) -> bool {
        matches!(adjust(&mut self.root, area, chrome, id, axis, cells), Adjust::Done)
    }

    /// Gives every split's children equal weight. `Ctrl-W =`.
    pub fn equalize(&mut self) {
        equalize(&mut self.root);
    }
}

/// How far `v` sits outside `lo..hi`. Zero when it is inside.
fn distance(v: u16, lo: u16, hi: u16) -> u16 {
    if v < lo {
        lo - v
    } else if v >= hi {
        v + 1 - hi
    } else {
        0
    }
}

fn collect(node: &Node, out: &mut Vec<WindowId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Split { children, .. } => {
            for child in children {
                collect(&child.node, out);
            }
        }
    }
}

/// Replaces the leaf `at` with a split holding `new` and `at`, evenly.
///
/// A split of the same direction as its parent joins the parent instead of
/// nesting inside it, so three `:vsplit`s give three columns rather than a
/// right-leaning staircase that resizes strangely.
fn split_at(node: &mut Node, at: WindowId, new: WindowId, dir: Dir, place: Place) -> bool {
    match node {
        Node::Leaf(id) if *id == at => {
            let (first, second) = match place {
                Place::Before => (new, at),
                Place::After => (at, new),
            };
            *node = Node::Split {
                dir,
                children: vec![
                    Child { weight: 0.5, node: Node::Leaf(first) },
                    Child { weight: 0.5, node: Node::Leaf(second) },
                ],
            };
            true
        }
        Node::Leaf(_) => false,
        Node::Split { dir: split_dir, children } => {
            let same = *split_dir == dir;
            for i in 0..children.len() {
                if same && matches!(children[i].node, Node::Leaf(id) if id == at) {
                    // Halve the child's share and give one half to the newcomer,
                    // so the siblings keep theirs.
                    let half = children[i].weight / 2.0;
                    children[i].weight = half;
                    let side = match place {
                        Place::Before => i,
                        Place::After => i + 1,
                    };
                    children.insert(side, Child { weight: half, node: Node::Leaf(new) });
                    return true;
                }
                if split_at(&mut children[i].node, at, new, dir, place) {
                    return true;
                }
            }
            false
        }
    }
}

/// Removes the leaf `id`, collapsing a split left with one child.
///
/// Returns the first leaf of whatever grew into the space.
fn close_at(node: &mut Node, id: WindowId) -> Option<WindowId> {
    let Node::Split { children, .. } = node else { return None };

    if let Some(i) = children.iter().position(|c| matches!(c.node, Node::Leaf(w) if w == id)) {
        let weight = children[i].weight;
        children.remove(i);
        // The neighbour that takes the space: the next one, or the previous
        // when the last child went.
        let heir = i.min(children.len() - 1);
        children[heir].weight += weight;
        let mut first = Vec::new();
        collect(&children[heir].node, &mut first);
        let focus = first[0];

        if children.len() == 1 {
            let only = children.remove(0);
            *node = only.node;
        }
        return Some(focus);
    }

    for child in children.iter_mut() {
        if let Some(focus) = close_at(&mut child.node, id) {
            // A nested split may have collapsed to a leaf; nothing to fix here,
            // since weights are per-child and the child kept its own.
            return Some(focus);
        }
    }
    None
}

/// Lays a node out in `rect`, appending one entry per leaf.
///
/// Children are placed by accumulating fractional edges and rounding each one,
/// so the rects tile the parent exactly — anything else leaves a one-cell seam
/// that appears and disappears as the terminal is resized.
fn place(node: &Node, rect: Rect, chrome: &Chrome, out: &mut Vec<(WindowId, Rect)>) {
    match node {
        Node::Leaf(id) => out.push((*id, rect)),
        Node::Split { dir, children } => {
            for (child, rect) in children.iter().zip(divide(rect, *dir, children, chrome)) {
                place(&child.node, rect, chrome, out);
            }
        }
    }
}

/// The extent of `rect` along `dir`, which is what weights divide.
fn extent(rect: Rect, dir: Dir) -> u16 {
    match dir {
        Dir::Vertical => rect.width,
        Dir::Horizontal => rect.height,
    }
}

/// The cells a split has left to share once its gaps are taken out.
fn shareable(rect: Rect, dir: Dir, children: usize, chrome: &Chrome) -> u16 {
    let gaps = chrome.gap(dir) * (children as u16).saturating_sub(1);
    extent(rect, dir).saturating_sub(gaps)
}

/// One rect per child, tiling `rect` exactly.
///
/// Edges accumulate fractionally and each is rounded once, so the rects meet
/// with no seam — anything else leaves a one-cell gap that appears and
/// disappears as the terminal is resized. The last child takes whatever is
/// left, which is what makes the sum exact rather than nearly exact.
///
/// One function rather than one per caller: `adjust` needs the same extents to
/// know what a cell is worth, and two copies of this arithmetic would drift.
fn divide(rect: Rect, dir: Dir, children: &[Child], chrome: &Chrome) -> Vec<Rect> {
    let gap = chrome.gap(dir);
    let avail = shareable(rect, dir, children.len(), chrome);

    let mut out = Vec::with_capacity(children.len());
    let mut acc = 0f32;
    let mut used = 0u16;
    for (i, child) in children.iter().enumerate() {
        acc += child.weight;
        let edge = if i + 1 == children.len() {
            avail
        } else {
            ((acc * avail as f32).round() as u16).min(avail)
        };
        let size = edge.saturating_sub(used);
        let offset = used + gap * i as u16;
        out.push(match dir {
            Dir::Vertical => Rect::new(rect.x + offset, rect.y, size, rect.height),
            Dir::Horizontal => Rect::new(rect.x, rect.y + offset, rect.width, size),
        });
        used = edge;
    }
    out
}

enum Adjust {
    NotFound,
    /// The target is in this subtree, and no split along the axis has claimed
    /// the adjustment yet.
    Found,
    Done,
}

fn adjust(
    node: &mut Node,
    rect: Rect,
    chrome: &Chrome,
    target: WindowId,
    axis: Dir,
    cells: i32,
) -> Adjust {
    let (dir, children) = match node {
        Node::Leaf(id) => {
            return if *id == target { Adjust::Found } else { Adjust::NotFound };
        }
        Node::Split { dir, children } => (*dir, children),
    };

    // Child rects, so the recursion knows the extents it is dividing.
    let rects = divide(rect, dir, children, chrome);

    for i in 0..children.len() {
        match adjust(&mut children[i].node, rects[i], chrome, target, axis, cells) {
            Adjust::Done => return Adjust::Done,
            Adjust::NotFound => continue,
            Adjust::Found => {
                if dir != axis {
                    return Adjust::Found;
                }
                // This is the divider being pushed.
                let avail = shareable(rect, dir, children.len(), chrome) as f32;
                if avail <= 0.0 {
                    return Adjust::Done;
                }
                // Take from the next sibling, or the previous when last.
                let j = if i + 1 < children.len() { i + 1 } else { i.wrapping_sub(1) };
                if j >= children.len() {
                    return Adjust::Found;
                }
                let delta = cells as f32 / avail;
                let floor = chrome.min(dir) as f32 / avail;
                let (a, b) = (children[i].weight + delta, children[j].weight - delta);
                if a < floor || b < floor {
                    return Adjust::Done;
                }
                children[i].weight = a;
                children[j].weight = b;
                return Adjust::Done;
            }
        }
    }
    Adjust::NotFound
}

fn equalize(node: &mut Node) {
    if let Node::Split { children, .. } = node {
        let share = 1.0 / children.len() as f32;
        for child in children.iter_mut() {
            child.weight = share;
            equalize(&mut child.node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME: Chrome =
        Chrome { columns: 1, rows: 0, min_width: 8, min_height: 2, tree_width: 30 };

    fn area() -> Rect {
        Rect::new(0, 0, 80, 24)
    }

    fn ids(n: u32) -> Vec<WindowId> {
        (0..n).map(WindowId).collect()
    }

    #[test]
    fn a_fresh_layout_is_one_window_filling_everything() {
        let layout = Layout::new(WindowId(0));
        assert_eq!(layout.rects(area(), &CHROME), vec![(WindowId(0), area())]);
    }

    #[test]
    fn a_vertical_split_puts_the_new_window_on_the_left() {
        let w = ids(2);
        let mut layout = Layout::new(w[0]);
        assert!(layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME));

        let rects = layout.rects(area(), &CHROME);
        // 80 wide, one column of rule between: 40 | rule | 39.
        assert_eq!(rects[0], (w[1], Rect::new(0, 0, 40, 24)), "the new window is first");
        assert_eq!(rects[1], (w[0], Rect::new(41, 0, 39, 24)));
    }

    #[test]
    fn a_horizontal_split_puts_the_new_window_above() {
        let w = ids(2);
        let mut layout = Layout::new(w[0]);
        assert!(layout.split(w[0], w[1], Dir::Horizontal, Place::Before, area(), &CHROME));

        let rects = layout.rects(area(), &CHROME);
        assert_eq!(rects[0], (w[1], Rect::new(0, 0, 80, 12)));
        assert_eq!(rects[1], (w[0], Rect::new(0, 12, 80, 12)));
    }

    #[test]
    fn rects_tile_the_area_exactly_however_the_cells_divide() {
        // 81 columns across three panes with two rules: 79 to share, which
        // divides into 26, 26 and 27 rather than losing a cell to rounding.
        let w = ids(3);
        let mut layout = Layout::new(w[0]);
        let area = Rect::new(0, 0, 81, 24);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area, &CHROME);
        layout.split(w[1], w[2], Dir::Vertical, Place::Before, area, &CHROME);
        layout.equalize();

        let rects = layout.rects(area, &CHROME);
        let covered: u16 = rects.iter().map(|(_, r)| r.width).sum();
        assert_eq!(covered + 2, 81, "every column is either a pane or a rule");
        for pair in rects.windows(2) {
            assert_eq!(pair[1].1.x, pair[0].1.right() + 1, "no gap and no overlap");
        }
    }

    #[test]
    fn splitting_the_same_way_twice_widens_the_row_rather_than_nesting() {
        let w = ids(3);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);
        layout.split(w[1], w[2], Dir::Vertical, Place::Before, area(), &CHROME);

        match layout.root() {
            Node::Split { dir: Dir::Vertical, children } => {
                assert_eq!(children.len(), 3, "one row of three, not a staircase");
            }
            other => panic!("expected one vertical split, got {other:?}"),
        }
    }

    #[test]
    fn splitting_the_other_way_nests() {
        let w = ids(3);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);
        layout.split(w[0], w[2], Dir::Horizontal, Place::Before, area(), &CHROME);

        assert_eq!(layout.leaves(), vec![w[1], w[2], w[0]]);
        let rects = layout.rects(area(), &CHROME);
        let right: Vec<_> = rects.iter().filter(|(_, r)| r.x == 41).collect();
        assert_eq!(right.len(), 2, "the right column is now two stacked panes");
    }

    #[test]
    fn a_split_with_no_room_is_refused() {
        let w = ids(2);
        let mut layout = Layout::new(w[0]);
        // 3 rows cannot hold two 2-row panes.
        let cramped = Rect::new(0, 0, 80, 3);
        assert!(!layout.split(w[0], w[1], Dir::Horizontal, Place::Before, cramped, &CHROME));
        assert_eq!(layout.len(), 1, "and nothing changed");
    }

    #[test]
    fn closing_gives_the_space_to_the_sibling_and_focuses_it() {
        let w = ids(2);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);

        assert_eq!(layout.close(w[1]), Some(w[0]));
        assert_eq!(layout.rects(area(), &CHROME), vec![(w[0], area())]);
    }

    #[test]
    fn closing_focuses_the_first_leaf_of_a_sibling_that_is_itself_a_split() {
        let w = ids(3);
        let mut layout = Layout::new(w[0]);
        // Left: w0. Right column: w2 above w1.
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);
        layout.split(w[1], w[2], Dir::Horizontal, Place::Before, area(), &CHROME);

        // Close the left pane; the stacked column grows into it, and focus
        // lands on its top-left-most window.
        assert_eq!(layout.close(w[1]), Some(w[2]));
    }

    #[test]
    fn the_last_window_cannot_be_closed() {
        let mut layout = Layout::new(WindowId(0));
        assert_eq!(layout.close(WindowId(0)), None);
        assert_eq!(layout.len(), 1);
    }

    #[test]
    fn only_leaves_one_window_filling_everything() {
        let w = ids(3);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);
        layout.split(w[1], w[2], Dir::Horizontal, Place::Before, area(), &CHROME);

        layout.only(w[0]);
        assert_eq!(layout.rects(area(), &CHROME), vec![(w[0], area())]);
    }

    #[test]
    fn directional_switching_reads_the_screen_rather_than_the_tree() {
        // Left column is one tall pane; right column is two stacked. From the
        // left, `l` picks by where the cursor is, not by tree order.
        let w = ids(3);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);
        layout.split(w[0], w[2], Dir::Horizontal, Place::Before, area(), &CHROME);
        // w1 is the left column; the right column is w2 above w0.

        let l = |anchor| layout.neighbour(w[1], Side::Right, area(), &CHROME, anchor);
        assert_eq!(l(2), Some(w[2]), "a cursor near the top reaches the top pane");
        assert_eq!(l(20), Some(w[0]), "and one near the bottom reaches the bottom one");
    }

    #[test]
    fn there_is_nothing_past_the_edge_of_the_screen() {
        let w = ids(2);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);

        assert_eq!(layout.neighbour(w[1], Side::Left, area(), &CHROME, 0), None);
        assert_eq!(layout.neighbour(w[1], Side::Up, area(), &CHROME, 0), None);
        assert_eq!(layout.neighbour(w[0], Side::Right, area(), &CHROME, 0), None);
    }

    #[test]
    fn a_pane_that_only_sits_past_another_is_not_beside_it() {
        // Top row spans the width; below it, two columns. From the bottom-left
        // pane, `k` finds the top row, but `l` from it must not.
        let w = ids(3);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Horizontal, Place::Before, area(), &CHROME);
        layout.split(w[0], w[2], Dir::Vertical, Place::Before, area(), &CHROME);
        // w1 across the top; w2 and w0 side by side below.

        assert_eq!(layout.neighbour(w[2], Side::Up, area(), &CHROME, 0), Some(w[1]));
        assert_eq!(layout.neighbour(w[2], Side::Right, area(), &CHROME, 20), Some(w[0]));
        assert_eq!(layout.neighbour(w[1], Side::Up, area(), &CHROME, 0), None);
    }

    #[test]
    fn cycling_wraps_in_layout_order() {
        let w = ids(3);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);
        layout.split(w[1], w[2], Dir::Vertical, Place::Before, area(), &CHROME);

        let order = layout.leaves();
        assert_eq!(layout.cycle(order[0], false), Some(order[1]));
        assert_eq!(layout.cycle(order[2], false), Some(order[0]), "forwards, wrapping");
        assert_eq!(layout.cycle(order[0], true), Some(order[2]), "and backwards");
    }

    #[test]
    fn resizing_moves_the_divider_by_the_cells_asked_for() {
        let w = ids(2);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);
        let before = layout.rect_of(w[1], area(), &CHROME).unwrap().width;

        assert!(layout.resize(w[1], Dir::Vertical, 5, area(), &CHROME));
        let after = layout.rect_of(w[1], area(), &CHROME).unwrap().width;
        assert_eq!(after, before + 5);
    }

    #[test]
    fn resizing_along_an_axis_with_no_divider_does_nothing() {
        let w = ids(2);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);

        // Side by side, so there is no horizontal divider to push.
        assert!(!layout.resize(w[1], Dir::Horizontal, 3, area(), &CHROME));
    }

    #[test]
    fn resizing_stops_at_the_floor_rather_than_squeezing_a_pane_out() {
        let w = ids(2);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);

        layout.resize(w[1], Dir::Vertical, 200, area(), &CHROME);
        let other = layout.rect_of(w[0], area(), &CHROME).unwrap();
        assert!(other.width >= CHROME.min_width, "the sibling keeps its floor: {other:?}");
    }

    #[test]
    fn resizing_pushes_the_nearest_divider_not_the_outermost() {
        // Left column, right column split in two. Growing the top-right pane
        // vertically must move the divider inside the right column.
        let w = ids(3);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);
        layout.split(w[0], w[2], Dir::Horizontal, Place::Before, area(), &CHROME);

        let left_before = layout.rect_of(w[1], area(), &CHROME).unwrap();
        assert!(layout.resize(w[2], Dir::Horizontal, 3, area(), &CHROME));

        assert_eq!(layout.rect_of(w[1], area(), &CHROME).unwrap(), left_before, "left untouched");
        assert_eq!(layout.rect_of(w[2], area(), &CHROME).unwrap().height, 12 + 3);
    }

    #[test]
    fn equalize_undoes_a_resize() {
        let w = ids(2);
        let mut layout = Layout::new(w[0]);
        layout.split(w[0], w[1], Dir::Vertical, Place::Before, area(), &CHROME);
        layout.resize(w[1], Dir::Vertical, 10, area(), &CHROME);

        layout.equalize();
        assert_eq!(layout.rect_of(w[1], area(), &CHROME).unwrap().width, 40);
    }
}
