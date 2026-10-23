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
    fn cross(self) -> Self {
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

/// The workspace area inside the outer gap, and the gap between panes.
pub(super) struct Area {
    pub(super) rect: Rect,
    gap: f32,
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
    /// Splits along `axis`, giving the first part `ratio` (0.1 to 0.9) of the
    /// space, with at least 48 pixels on each side when it fits.
    fn split(&self, axis: Axis, ratio: f32) -> (Rect, Rect) {
        let (b, gap) = (self.rect, self.gap);
        let length = match axis {
            Axis::X => b.width(),
            Axis::Y => b.height(),
        };
        let available = (length - gap).max(0.0);
        let minimum = 48.0_f32.min(available / 2.0);
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
    /// Divides `rect` among `ids` along `axis` in proportion to their weights,
    /// keeping a 48-pixel minimum per pane when it fits.
    fn partition(
        &self,
        rect: Rect,
        ids: &[u64],
        axis: Axis,
        weights: &BTreeMap<u64, f32>,
    ) -> Vec<(u64, Rect)> {
        let horizontal = axis == Axis::X;
        let available = ((if horizontal {
            rect.width()
        } else {
            rect.height()
        }) - self.gap * ids.len().saturating_sub(1) as f32)
            .max(0.0);
        let minimum = 48.0_f32.min(available / ids.len().max(1) as f32);
        let extra = (available - minimum * ids.len() as f32).max(0.0);
        let weight = |id: &u64| weights.get(id).copied().unwrap_or(1.0);
        let total = ids.iter().map(weight).sum::<f32>();
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

/// A layout's behavior. `Workspace` checks the shared preconditions first:
/// every method sees at least two panes, and `index` is the focused pane's
/// position in `w.panes`.
pub(super) trait Arrange {
    /// Where each visible pane goes.
    fn rectangles(&self, w: &Workspace, area: &Area) -> Vec<(u64, Rect)>;
    /// Reorders panes so the focused one is on screen, after focus, layout,
    /// or pane order changes. The default does nothing.
    fn reveal(&self, _w: &mut Workspace) {}
    /// Moves the divider nearest the focused pane along `axis`. The default
    /// does nothing.
    fn resize(&self, _w: &mut Workspace, _index: usize, _axis: Axis, _forward: bool) {}
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
    fn rectangles(&self, w: &Workspace, area: &Area) -> Vec<(u64, Rect)> {
        // `main_ratio` is the main pane's share, whichever side it is on.
        let ratio = match self.main {
            Side::First => w.main_ratio,
            Side::Second => 1.0 - w.main_ratio,
        };
        let (first, second) = area.split(Axis::X, ratio);
        let (main, stack) = match self.main {
            Side::First => (first, second),
            Side::Second => (second, first),
        };
        let mut result = vec![(w.panes[0], main)];
        result.extend(area.partition(stack, &w.panes[1..], Axis::Y, w.weights()));
        result
    }
    fn resize(&self, w: &mut Workspace, index: usize, axis: Axis, forward: bool) {
        match axis {
            Axis::X => resize_main(w, forward, self.main == Side::Second),
            Axis::Y => resize_within(w, index, 1..w.panes.len(), forward),
        }
    }
}

/// SimpleWindowManager's two-part grid: `floor(n/2)` panes in the first part
/// and the rest in the second, each divided across the other axis.
struct Grid {
    split: Axis,
}
impl Arrange for Grid {
    fn rectangles(&self, w: &Workspace, area: &Area) -> Vec<(u64, Rect)> {
        let (first, second) = area.split(self.split, w.main_ratio);
        let half = w.panes.len() / 2;
        let across = self.split.cross();
        let mut result = area.partition(first, &w.panes[..half], across, w.weights());
        result.extend(area.partition(second, &w.panes[half..], across, w.weights()));
        result
    }
    fn resize(&self, w: &mut Workspace, index: usize, axis: Axis, forward: bool) {
        if axis == self.split {
            return resize_main(w, forward, false);
        }
        let half = w.panes.len() / 2;
        let group = if index < half {
            0..half
        } else {
            half..w.panes.len()
        };
        resize_within(w, index, group, forward);
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
    fn rectangles(&self, w: &Workspace, area: &Area) -> Vec<(u64, Rect)> {
        let (main, stack) = area.split(Axis::X, w.main_ratio);
        vec![(w.panes[0], main), (w.panes[1], stack)]
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
    /// Only the outer split resizes; the stack shows one pane.
    fn resize(&self, w: &mut Workspace, _index: usize, axis: Axis, forward: bool) {
        if axis == Axis::X {
            resize_main(w, forward, false);
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
    let before = w.weights().get(&a).copied().unwrap_or(1.0);
    let after = w.weights().get(&b).copied().unwrap_or(1.0);
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
