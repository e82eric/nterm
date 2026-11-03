//! What each workspace layout decides: where panes go, what resizing does, and
//! how focus moves. Each layout is one type implementing `Arrange`;
//! `Layout::arrange` maps the configured layout to it.
use super::workspace::{Appearance, Layout, Workspace};
use egui::{Rect, pos2};
use std::{collections::BTreeMap, ops::Range};

/// The direction space is divided along.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Axis {
    /// Side by side: divides the width.
    X,
    /// Stacked: divides the height.
    Y,
}
impl Axis {
    pub(super) fn cross(self) -> Self {
        match self {
            Self::X => Self::Y,
            Self::Y => Self::X,
        }
    }
}
/// Which part of the outer split holds the main pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Side {
    First,
    Second,
}
/// A focus or move request in a screen direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Motion {
    Left,
    Right,
    Up,
    Down,
}
impl Motion {
    fn backwards(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }
    fn direction(self) -> egui::Vec2 {
        match self {
            Self::Left => egui::vec2(-1.0, 0.0),
            Self::Right => egui::vec2(1.0, 0.0),
            Self::Up => egui::vec2(0.0, -1.0),
            Self::Down => egui::vec2(0.0, 1.0),
        }
    }
}

/// The smallest a pane gets along a divided axis, when it fits.
const MINIMUM: f32 = 48.0;

fn length(rect: Rect, axis: Axis) -> f32 {
    match axis {
        Axis::X => rect.width(),
        Axis::Y => rect.height(),
    }
}
/// The left (X) or top (Y) edge of `rect`.
fn start(rect: Rect, axis: Axis) -> f32 {
    match axis {
        Axis::X => rect.left(),
        Axis::Y => rect.top(),
    }
}

/// The workspace area inside the outer gap, and the gap between panes.
pub(super) struct Area {
    pub(super) rect: Rect,
    pub(super) gap: f32,
}
impl Area {
    /// Gaps shrink when the area is too small for them; `panes` is at least 1.
    pub(super) fn new(bounds: Rect, style: &Appearance, panes: usize) -> Self {
        let outer = style
            .outer_gap
            .min((bounds.width().min(bounds.height()) - 1.0).max(0.0) / 2.0);
        let rect = bounds.shrink(outer);
        let gap = style
            .inner_gap
            .min((rect.width().min(rect.height()) / panes as f32 - 1.0).max(0.0));
        Self { rect, gap }
    }
    /// The space `split` divides along `axis`: everything but one gap.
    fn split_space(&self, axis: Axis) -> f32 {
        (length(self.rect, axis) - self.gap).max(0.0)
    }
    /// Splits along `axis`, giving the first part `ratio` (0.1 to 0.9) of the
    /// space, with at least `MINIMUM` pixels on each side when it fits.
    fn split(&self, axis: Axis, ratio: f32) -> (Rect, Rect) {
        let (b, gap) = (self.rect, self.gap);
        let available = self.split_space(axis);
        let minimum = MINIMUM.min(available / 2.0);
        let first = (available * ratio.clamp(0.1, 0.9)).clamp(minimum, available - minimum);
        match axis {
            Axis::X => (
                Rect::from_min_max(b.min, pos2(b.left() + first, b.bottom())),
                Rect::from_min_max(pos2(b.left() + first + gap, b.top()), b.max),
            ),
            Axis::Y => (
                Rect::from_min_max(b.min, pos2(b.right(), b.top() + first)),
                Rect::from_min_max(pos2(b.left(), b.top() + first + gap), b.max),
            ),
        }
    }
    /// Each pane's guaranteed minimum, and the space shared out by weight,
    /// when `partition` divides `rect` among `panes` along `axis`.
    fn partition_space(&self, rect: Rect, panes: usize, axis: Axis) -> (f32, f32) {
        let panes = panes.max(1);
        let available = (length(rect, axis) - self.gap * (panes - 1) as f32).max(0.0);
        let minimum = MINIMUM.min(available / panes as f32);
        (minimum, (available - minimum * panes as f32).max(0.0))
    }
    /// Divides `rect` among `ids` along `axis` in proportion to their weights,
    /// keeping a `MINIMUM` per pane when it fits.
    fn partition(
        &self,
        rect: Rect,
        ids: &[u64],
        axis: Axis,
        weights: &BTreeMap<u64, f32>,
    ) -> Vec<(u64, Rect)> {
        let horizontal = axis == Axis::X;
        let (minimum, extra) = self.partition_space(rect, ids.len(), axis);
        let weight = |id: &u64| weight(weights, *id);
        let total = total_weight(weights, ids);
        let mut start = if horizontal { rect.left() } else { rect.top() };
        ids.iter()
            .enumerate()
            .map(|(i, id)| {
                let end = if i + 1 == ids.len() {
                    if horizontal {
                        rect.right()
                    } else {
                        rect.bottom()
                    }
                } else {
                    start + minimum + extra * weight(id) / total
                };
                let pane = if horizontal {
                    Rect::from_min_max(pos2(start, rect.top()), pos2(end, rect.bottom()))
                } else {
                    Rect::from_min_max(pos2(rect.left(), start), pos2(rect.right(), end))
                };
                start = end + self.gap;
                (*id, pane)
            })
            .collect()
    }
}

/// A pane's weight within its stack; panes without one weigh 1.
fn weight(weights: &BTreeMap<u64, f32>, id: u64) -> f32 {
    weights.get(&id).copied().unwrap_or(1.0)
}
fn total_weight(weights: &BTreeMap<u64, f32>, ids: &[u64]) -> f32 {
    ids.iter().map(|id| weight(weights, *id)).sum()
}

/// How a layout divides space: one split along `axis`, then each side's
/// panes stacked across it. Drawing, keyboard resizing and mouse resizing all
/// follow this, so they agree on which panes share a divider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Splits {
    axis: Axis,
    /// The side whose share `main_ratio` is.
    main: Side,
    /// Positions in `panes` on each side, in screen order (left or top first).
    groups: [Range<usize>; 2],
}

/// A movable divider. Identified by the panes it separates rather than its
/// position, so it stays the same divider while the window or other panes
/// change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DividerId {
    /// The outer split.
    Main,
    /// Between two neighbors in a stack.
    Between(u64, u64),
}
/// Where a divider is on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DividerAt {
    pub(super) id: DividerId,
    /// The direction it moves: `X` for a vertical line.
    pub(super) axis: Axis,
    /// Its position along `axis`, in the middle of the gap.
    pub(super) at: f32,
    /// The gap it sits in.
    pub(super) gap: f32,
    /// The rectangle it runs across, end to end.
    pub(super) span: Rect,
}

impl Splits {
    fn pair_group(&self, w: &Workspace, a: u64, b: u64) -> Option<Range<usize>> {
        self.groups
            .iter()
            .find(|g| w.panes[(*g).clone()].windows(2).any(|p| p == [a, b]))
            .cloned()
    }
    /// The dividers that can move: the outer split when there is space to
    /// divide, and those between neighbors in a stack with room above the
    /// minimum. `rects` are this layout's rectangles.
    pub(super) fn dividers(
        &self,
        w: &Workspace,
        area: &Area,
        rects: &[(u64, Rect)],
    ) -> Vec<DividerAt> {
        let rect = |id| rects.iter().find(|(i, _)| *i == id).unwrap().1;
        let span = |ids: &[u64]| ids.iter().fold(Rect::NOTHING, |r, id| r.union(rect(*id)));
        let end = |r: Rect, axis| start(r, axis) + length(r, axis) + area.gap / 2.0;
        let mut result = Vec::new();
        if area.split_space(self.axis) > 0.0 {
            let first = span(&w.panes[self.groups[0].clone()]);
            result.push(DividerAt {
                id: DividerId::Main,
                axis: self.axis,
                at: end(first, self.axis),
                gap: area.gap,
                span: area.rect,
            });
        }
        let across = self.axis.cross();
        for group in &self.groups {
            let ids = &w.panes[group.clone()];
            let whole = span(ids);
            if ids.len() < 2 || area.partition_space(whole, ids.len(), across).1 <= 0.0 {
                continue;
            }
            for pair in ids.windows(2) {
                result.push(DividerAt {
                    id: DividerId::Between(pair[0], pair[1]),
                    axis: across,
                    at: end(rect(pair[0]), across),
                    gap: area.gap,
                    span: whole,
                });
            }
        }
        result
    }
    /// Moves a divider so its center is at `to`, as far as the limits allow.
    /// Solves `split` or `partition` for the ratio or weights that put it
    /// there. `rects` are this layout's rectangles before the move.
    pub(super) fn move_divider(
        &self,
        w: &mut Workspace,
        area: &Area,
        rects: &[(u64, Rect)],
        id: DividerId,
        to: f32,
    ) {
        let edge = to - area.gap / 2.0;
        match id {
            DividerId::Main => {
                let available = area.split_space(self.axis);
                if available <= 0.0 {
                    return;
                }
                let first = (edge - start(area.rect, self.axis)) / available;
                let ratio = match self.main {
                    Side::First => first,
                    Side::Second => 1.0 - first,
                };
                w.main_ratio = ratio.clamp(0.1, 0.9);
            }
            DividerId::Between(a, b) => {
                let Some(group) = self.pair_group(w, a, b) else {
                    return;
                };
                let rect = |id| rects.iter().find(|(i, _)| *i == id).unwrap().1;
                let (ra, rb) = (rect(a), rect(b));
                let across = self.axis.cross();
                let whole = w.panes[group.clone()]
                    .iter()
                    .fold(Rect::NOTHING, |r, id| r.union(rect(*id)));
                // `partition` gives each pane the minimum plus a share of the
                // rest by weight, so `a`'s share of the pair's room above the
                // minimum is its share of their combined weight.
                let (minimum, _) = area.partition_space(whole, group.len(), across);
                let room = length(ra, across) + length(rb, across) - 2.0 * minimum;
                if room <= 0.0 {
                    return;
                }
                let share = ((edge - start(ra, across) - minimum) / room).clamp(0.05, 0.95);
                let total = weight(w.weights(), a) + weight(w.weights(), b);
                let weights = w.weights_mut();
                weights.insert(a, total * share);
                weights.insert(b, total * (1.0 - share));
            }
        }
    }
    fn rectangles(&self, w: &Workspace, area: &Area) -> Vec<(u64, Rect)> {
        let ratio = match self.main {
            Side::First => w.main_ratio,
            Side::Second => 1.0 - w.main_ratio,
        };
        let (first, second) = area.split(self.axis, ratio);
        let across = self.axis.cross();
        let [a, b] = &self.groups;
        let mut result = area.partition(first, &w.panes[a.clone()], across, w.weights());
        result.extend(area.partition(second, &w.panes[b.clone()], across, w.weights()));
        result
    }
}

/// A layout's behavior. `Workspace` checks the shared preconditions first:
/// every method sees at least two panes, and `index` is the focused pane's
/// position in `w.panes`.
pub(super) trait Arrange {
    /// How the layout divides space, or `None` when nothing is split. The
    /// default `rectangles` and `resize` follow it.
    fn splits(&self, _w: &Workspace) -> Option<Splits> {
        None
    }
    /// Where each visible pane goes. The default lays out `splits`.
    fn rectangles(&self, w: &Workspace, area: &Area) -> Vec<(u64, Rect)> {
        match self.splits(w) {
            Some(splits) => splits.rectangles(w, area),
            None => vec![(w.panes[0], area.rect)],
        }
    }
    /// Reorders panes so the focused one is on screen, after focus, layout,
    /// or pane order changes. The default does nothing.
    fn reveal(&self, _w: &mut Workspace) {}
    /// Moves the divider nearest the focused pane along `axis`: the outer
    /// split, or one within the focused pane's side of `splits`.
    fn resize(&self, w: &mut Workspace, index: usize, axis: Axis, forward: bool) {
        let Some(splits) = self.splits(w) else {
            return;
        };
        if axis == splits.axis {
            resize_main(w, forward, splits.main == Side::Second);
        } else if let Some(group) = splits.groups.into_iter().find(|g| g.contains(&index)) {
            resize_within(w, index, group, forward);
        }
    }
    /// The pane a motion from the focused pane reaches. The default is the
    /// nearest visible pane in that direction.
    fn target(
        &self,
        w: &Workspace,
        index: usize,
        motion: Motion,
        rects: &[(u64, Rect)],
    ) -> Option<usize> {
        spatial_target(w, index, motion, rects)
    }
    /// Moves focus and returns whether anything changed.
    fn focus(
        &self,
        w: &mut Workspace,
        index: usize,
        motion: Motion,
        rects: &[(u64, Rect)],
    ) -> bool {
        let Some(target) = self.target(w, index, motion, rects) else {
            return false;
        };
        let id = w.panes[target];
        w.focus(id);
        true
    }
    /// Moves the focused pane and returns whether anything changed. The
    /// default swaps it with `target`, keeping focus on it.
    fn move_pane(
        &self,
        w: &mut Workspace,
        index: usize,
        motion: Motion,
        rects: &[(u64, Rect)],
    ) -> bool {
        let Some(target) = self.target(w, index, motion, rects) else {
            return false;
        };
        w.panes.swap(index, target);
        true
    }
    /// The focused pane's 1-based number in the status bar.
    fn pane_number(&self, w: &Workspace, index: usize) -> usize {
        let _ = w;
        index + 1
    }
}

impl Layout {
    pub(super) fn arrange(self) -> &'static dyn Arrange {
        match self {
            Self::Tile => &Tile { main: Side::First },
            Self::TileReversed => &Tile { main: Side::Second },
            Self::Grid => &Grid { split: Axis::X },
            Self::HorizontalGrid => &Grid { split: Axis::Y },
            Self::Monocle => &Monocle,
            Self::Deck => &Deck,
        }
    }
}

/// One main pane beside a vertical stack of the rest.
struct Tile {
    main: Side,
}
impl Arrange for Tile {
    fn splits(&self, w: &Workspace) -> Option<Splits> {
        let (main, stack) = (0..1, 1..w.panes.len());
        Some(Splits {
            axis: Axis::X,
            main: self.main,
            groups: match self.main {
                Side::First => [main, stack],
                Side::Second => [stack, main],
            },
        })
    }
}

/// SimpleWindowManager's two-part grid: `floor(n/2)` panes in the first part
/// and the rest in the second, each divided across the other axis.
struct Grid {
    split: Axis,
}
impl Arrange for Grid {
    fn splits(&self, w: &Workspace) -> Option<Splits> {
        let half = w.panes.len() / 2;
        Some(Splits {
            axis: self.split,
            main: Side::First,
            groups: [0..half, half..w.panes.len()],
        })
    }
}

/// The focused pane alone. Focus and moves follow list order, since hidden
/// panes have no screen position.
struct Monocle;
impl Arrange for Monocle {
    fn rectangles(&self, w: &Workspace, area: &Area) -> Vec<(u64, Rect)> {
        vec![(w.focused.unwrap_or(w.panes[0]), area.rect)]
    }
    fn target(
        &self,
        w: &Workspace,
        index: usize,
        motion: Motion,
        _: &[(u64, Rect)],
    ) -> Option<usize> {
        Some(order_target(w.panes.len(), index, motion))
    }
}

/// The main pane beside one stack pane at a time, cycled like SimpleWindowManager.
/// As in SWM, pane order is the deck's only state: `panes[0]` is the main
/// pane and `panes[1]` the visible stack pane.
pub(super) struct Deck;
impl Deck {
    /// Rotates the deck while focus stays on the selected side.
    pub(super) fn cycle(w: &mut Workspace, forward: bool) {
        let n = w.panes.len();
        if n < 2 {
            return;
        }
        let Some(position) = w.panes.iter().position(|id| Some(*id) == w.focused) else {
            return;
        };
        if position == 0 {
            if forward {
                w.panes.rotate_left(1);
                w.panes.swap(0, 1);
            } else {
                w.panes.swap(0, n - 1);
            }
            w.focus(w.panes[0]);
        } else {
            // `reveal` keeps a focused stack pane in slot 1.
            if forward {
                w.panes[1..].rotate_left(1);
            } else {
                w.panes[1..].rotate_right(1);
            }
            w.focus(w.panes[1]);
        }
    }
}
impl Arrange for Deck {
    /// Only the main pane and the visible stack pane are laid out, one per
    /// side, so only the outer split resizes.
    fn splits(&self, _w: &Workspace) -> Option<Splits> {
        Some(Splits {
            axis: Axis::X,
            main: Side::First,
            groups: [0..1, 1..2],
        })
    }
    /// Rotates the stack so a focused hidden pane comes to slot 1, as SWM
    /// does on arrange. Rotating keeps the stack's cyclic order.
    fn reveal(&self, w: &mut Workspace) {
        if let Some(position) = w.panes.iter().position(|id| Some(*id) == w.focused)
            && position > 1
        {
            w.panes[1..].rotate_left(position - 1);
        }
    }
    /// As in SWM: right from the main pane and left from the stack swap the
    /// two visible panes; up/down cycle the deck.
    fn move_pane(
        &self,
        w: &mut Workspace,
        index: usize,
        motion: Motion,
        _: &[(u64, Rect)],
    ) -> bool {
        match motion {
            Motion::Down => Self::cycle(w, true),
            Motion::Up => Self::cycle(w, false),
            Motion::Right if index == 0 => w.panes.swap(0, 1),
            Motion::Left if index > 0 => w.panes.swap(0, index),
            Motion::Left | Motion::Right => return false,
        }
        true
    }
    /// Up/down cycle the deck; left focuses the main pane, right the stack.
    fn focus(&self, w: &mut Workspace, index: usize, motion: Motion, _: &[(u64, Rect)]) -> bool {
        match motion {
            Motion::Down => Self::cycle(w, true),
            Motion::Up => Self::cycle(w, false),
            Motion::Left => {
                let id = w.panes[0];
                w.focus(id);
            }
            Motion::Right if index == 0 => {
                let id = w.panes[1];
                w.focus(id);
            }
            Motion::Right => return false,
        }
        true
    }
    /// Cycling rotates `panes`, so number panes by creation order instead;
    /// IDs only increase.
    fn pane_number(&self, w: &Workspace, index: usize) -> usize {
        let id = w.panes[index];
        w.panes.iter().filter(|p| **p < id).count() + 1
    }
}

fn order_target(panes: usize, index: usize, motion: Motion) -> usize {
    if motion.backwards() {
        (index + panes - 1) % panes
    } else {
        (index + 1) % panes
    }
}
/// The nearest visible pane in the motion's direction, topmost or leftmost
/// on a tie, or list order when the focused pane is not on screen.
fn spatial_target(
    w: &Workspace,
    index: usize,
    motion: Motion,
    rects: &[(u64, Rect)],
) -> Option<usize> {
    let focused = w.panes[index];
    let Some((_, from)) = rects.iter().find(|(id, _)| *id == focused) else {
        return Some(order_target(w.panes.len(), index, motion));
    };
    let direction = motion.direction();
    let center = from.center();
    // A target must share rows (left/right) or columns (up/down) with the
    // focused pane, so a pane in the next row is never "to the right".
    let beside = |r: &Rect| match motion {
        Motion::Left | Motion::Right => r.top() < from.bottom() && r.bottom() > from.top(),
        Motion::Up | Motion::Down => r.left() < from.right() && r.right() > from.left(),
    };
    // The nearest column (or row) wins; within it, the top (or leftmost)
    // pane, so a tall pane beside a stack always lands on the first one.
    let key = |r: &Rect| {
        let cross = match motion {
            Motion::Left | Motion::Right => r.top(),
            Motion::Up | Motion::Down => r.left(),
        };
        ((r.center() - center).dot(direction).round(), cross)
    };
    let (id, _) = rects
        .iter()
        .filter(|(id, r)| *id != focused && beside(r) && (r.center() - center).dot(direction) > 0.5)
        .min_by(|(_, a), (_, b)| {
            let (a, b) = (key(a), key(b));
            a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1))
        })?;
    w.panes.iter().position(|p| p == id)
}
/// Moves the outer split. With the main pane on the second side, growing the
/// first part shrinks the main pane.
fn resize_main(w: &mut Workspace, forward: bool, reversed: bool) {
    let delta = if forward { 0.05 } else { -0.05 };
    let sign = if reversed { -1.0 } else { 1.0 };
    w.main_ratio = (w.main_ratio + sign * delta).clamp(0.1, 0.9);
}
/// Moves the divider between the focused pane and a neighbor in `group`,
/// keeping each at least 5% of their combined weight.
fn resize_within(w: &mut Workspace, index: usize, group: Range<usize>, forward: bool) {
    if !group.contains(&index) || group.len() < 2 {
        return;
    }
    let i = index - group.start;
    let boundary = if forward {
        i.min(group.len() - 2)
    } else {
        i.saturating_sub(1)
    };
    let (a, b) = (
        w.panes[group.start + boundary],
        w.panes[group.start + boundary + 1],
    );
    let delta = if forward { 0.05 } else { -0.05 };
    let before = weight(w.weights(), a);
    let after = weight(w.weights(), b);
    let total = before + after;
    let resized = (before + delta * total).clamp(total * 0.05, total * 0.95);
    let weights = w.weights_mut();
    weights.insert(a, resized);
    weights.insert(b, total - resized);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn workspace(layout: Layout, panes: &[u64], focused: u64) -> Workspace {
        let mut w = Workspace::new(1, 1);
        w.layout = layout;
        w.panes = panes.to_vec();
        w.focus(focused);
        w
    }
    fn rects(w: &Workspace) -> Vec<(u64, Rect)> {
        let bounds = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
        w.rectangles(bounds, &Appearance::default())
    }
    #[test]
    fn tile_focus_follows_the_screen_and_moves_swap_with_the_target() {
        let mut w = workspace(Layout::Tile, &[1, 2, 3], 1);
        let r = rects(&w);
        assert!(w.navigate(Motion::Right, false, &r));
        assert_eq!(w.focused, Some(2));
        assert!(w.navigate(Motion::Down, false, &r));
        assert_eq!(w.focused, Some(3));
        // Nothing lies further down.
        assert!(!w.navigate(Motion::Down, false, &r));
        assert!(w.navigate(Motion::Left, true, &r));
        assert_eq!(w.panes, vec![3, 2, 1]);
        assert_eq!(w.focused, Some(3));
    }
    #[test]
    fn moving_from_a_tall_pane_lands_on_the_top_one_beside_it() {
        for (layout, motion) in [
            (Layout::Tile, Motion::Right),
            (Layout::TileReversed, Motion::Left),
        ] {
            let mut w = workspace(layout, &[1, 2, 3, 4], 1);
            let r = rects(&w);
            assert!(w.navigate(motion, false, &r), "{layout:?}");
            assert_eq!(w.focused, Some(2), "{layout:?}");
        }
        // Horizontal grid, one pane on top and two below: down goes left.
        let mut w = workspace(Layout::HorizontalGrid, &[1, 2, 3], 1);
        let r = rects(&w);
        assert!(w.navigate(Motion::Down, false, &r));
        assert_eq!(w.focused, Some(2));
    }
    #[test]
    fn tile_and_grid_up_and_down_stay_in_the_column() {
        for layout in [Layout::Tile, Layout::TileReversed, Layout::Grid] {
            // Tile: main pane beside two stacked. Grid: one left, two right.
            let mut w = workspace(layout, &[1, 2, 3], 1);
            let r = rects(&w);
            assert!(!w.navigate(Motion::Up, false, &r), "{layout:?}");
            assert!(!w.navigate(Motion::Down, false, &r), "{layout:?}");
        }
    }
    #[test]
    fn horizontal_grid_left_and_right_stay_in_the_row() {
        // One pane on top, two below.
        let mut w = workspace(Layout::HorizontalGrid, &[1, 2, 3], 2);
        let r = rects(&w);
        assert!(!w.navigate(Motion::Left, false, &r));
        assert!(w.navigate(Motion::Right, false, &r));
        assert_eq!(w.focused, Some(3));
        assert!(!w.navigate(Motion::Right, false, &r));
        assert!(w.navigate(Motion::Up, false, &r));
        assert_eq!(w.focused, Some(1));
        assert!(!w.navigate(Motion::Left, false, &r));
        assert!(!w.navigate(Motion::Right, false, &r));
    }
    #[test]
    fn monocle_follows_list_order() {
        let mut w = workspace(Layout::Monocle, &[1, 2, 3], 1);
        let r = rects(&w);
        assert!(w.navigate(Motion::Left, false, &r));
        assert_eq!(w.focused, Some(3));
    }
    #[test]
    fn deck_moves_match_swm() {
        let mut w = workspace(Layout::Deck, &[1, 2, 3], 2);
        let r = rects(&w);
        // Up/down cycle the stack, like focusing.
        assert!(w.navigate(Motion::Down, true, &r));
        assert_eq!((w.panes.clone(), w.focused), (vec![1, 3, 2], Some(3)));
        // Left swaps the visible stack pane with the main pane.
        assert!(w.navigate(Motion::Left, true, &r));
        assert_eq!((w.panes.clone(), w.focused), (vec![3, 1, 2], Some(3)));
        assert!(!w.navigate(Motion::Left, true, &r));
        // Right swaps back.
        assert!(w.navigate(Motion::Right, true, &r));
        assert_eq!((w.panes.clone(), w.focused), (vec![1, 3, 2], Some(3)));
        assert!(!w.navigate(Motion::Right, true, &r));
    }
    #[test]
    fn deck_left_and_right_switch_between_main_and_stack() {
        let mut w = workspace(Layout::Deck, &[1, 2, 3], 3);
        let r = rects(&w);
        assert!(w.navigate(Motion::Left, false, &r));
        assert_eq!(w.focused, Some(1));
        assert!(w.navigate(Motion::Right, false, &r));
        assert_eq!(w.focused, Some(3));
        assert!(!w.navigate(Motion::Right, false, &r));
    }
    #[test]
    fn deck_keeps_a_focused_stack_pane_visible_through_main_focus() {
        let mut w = workspace(Layout::Deck, &[1, 2, 3, 4, 5], 1);
        // Clicking or Lua-focusing a hidden pane rotates it into view.
        w.focus(4);
        assert_eq!(w.panes, vec![1, 4, 5, 2, 3]);
        let r = rects(&w);
        assert!(w.navigate(Motion::Left, false, &r));
        assert_eq!(w.focused, Some(1));
        assert_eq!(rects(&w)[1].0, 4);
        assert!(w.navigate(Motion::Right, false, &r));
        assert_eq!(w.focused, Some(4));
        // Closing the visible stack pane shows the next one.
        w.remove(4);
        assert_eq!((w.panes[1], w.focused), (5, Some(5)));
    }
    #[test]
    fn deck_numbers_panes_by_creation_order_through_cycling() {
        let mut w = workspace(Layout::Deck, &[1, 2, 3], 2);
        assert_eq!(w.pane_number(), 2);
        Deck::cycle(&mut w, true);
        assert_eq!(w.focused, Some(3));
        assert_eq!(w.pane_number(), 3);
        w.layout = Layout::Tile;
        assert_eq!(w.pane_number(), 2);
    }
}
