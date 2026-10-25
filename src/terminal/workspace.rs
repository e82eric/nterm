//! Window-local workspaces. Layout geometry and behavior live in `layout`.
use super::layout::{Area, Axis, Motion};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Layout {
    #[default]
    Tile,
    Monocle,
    Deck,
    Grid,
    TileReversed,
    #[serde(alias = "horizontal_deck")]
    HorizontalGrid,
}
impl Layout {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Tile => "T",
            Self::TileReversed => "RT",
            Self::Monocle => "M",
            Self::Deck => "D",
            Self::HorizontalGrid => "HQ",
            Self::Grid => "Q",
        }
    }
}
/// The config's `workspaces` table: window appearance defaults plus the layout
/// new workspaces start in.
#[derive(Clone, Debug, Default)]
pub(crate) struct Config {
    pub default_layout: Layout,
    pub appearance: Appearance,
}
// Not `#[serde(flatten)]`: it disables `deny_unknown_fields`, and typos in
// appearance fields must stay errors.
impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let mut fields = serde_json::Map::deserialize(deserializer)?;
        let default_layout = match fields.remove("default_layout") {
            Some(layout) => serde_json::from_value(layout).map_err(D::Error::custom)?,
            None => Layout::default(),
        };
        let appearance = serde_json::from_value(fields.into()).map_err(D::Error::custom)?;
        Ok(Self {
            default_layout,
            appearance,
        })
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Appearance {
    pub inner_gap: f32,
    pub outer_gap: f32,
    pub padding: f32,
    pub corner_radius: f32,
    pub border_width: f32,
    pub shadow_size: f32,
    pub shadow_opacity: f32,
    pub shadow_offset: [f32; 2],
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            inner_gap: 8.0,
            outer_gap: 8.0,
            padding: 4.0,
            corner_radius: 8.0,
            // Thin curved lines look jagged at 1 px.
            border_width: 1.5,
            shadow_size: 8.0,
            shadow_opacity: 0.3,
            shadow_offset: [0.0, 2.0],
        }
    }
}
impl Appearance {
    pub fn content_rect(&self, rect: egui::Rect) -> egui::Rect {
        rect.shrink(
            self.padding
                .min((rect.width().min(rect.height()) - 1.0).max(0.0) / 2.0),
        )
    }
    pub fn paint_frame(&self, painter: &egui::Painter, rect: egui::Rect, active: bool) {
        let palette = crate::colors::palette();
        if self.shadow_size > 0.0 {
            let shadow = egui::epaint::Shadow {
                offset: [
                    self.shadow_offset[0].clamp(-128.0, 127.0) as i8,
                    self.shadow_offset[1].clamp(-128.0, 127.0) as i8,
                ],
                blur: self.shadow_size.min(255.0) as u8,
                spread: 0,
                color: egui::Color32::from_black_alpha((self.shadow_opacity * 255.0) as u8),
            };
            painter.add(shadow.as_shape(
                rect,
                egui::CornerRadius::same(self.corner_radius.min(255.0) as u8),
            ));
        }
        painter.rect_stroke(
            rect,
            self.corner_radius,
            egui::Stroke::new(
                self.border_width,
                if active {
                    palette.accent
                } else {
                    palette.border
                },
            ),
            egui::StrokeKind::Outside,
        );
    }
    pub fn validate(&self) -> Result<(), String> {
        for v in [
            self.inner_gap,
            self.outer_gap,
            self.padding,
            self.corner_radius,
            self.border_width,
            self.shadow_size,
            self.shadow_opacity,
            self.shadow_offset[0],
            self.shadow_offset[1],
        ] {
            if !v.is_finite() {
                return Err("workspace appearance values must be finite".into());
            }
        }
        if [
            self.inner_gap,
            self.outer_gap,
            self.padding,
            self.corner_radius,
            self.border_width,
            self.shadow_size,
        ]
        .iter()
        .any(|v| *v < 0.0 || *v > 256.0)
            || !(0.0..=1.0).contains(&self.shadow_opacity)
        {
            return Err("workspace dimensions must be 0..256 and shadow opacity 0..1".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Workspace {
    pub id: u64,
    pub index: usize,
    pub panes: Vec<u64>,
    pub focused: Option<u64>,
    pub layout: Layout,
    pub main_ratio: f32,
    /// Each layout's pane sizes within its groups, by pane ID; a missing pane
    /// weighs 1.0. Kept per layout because layouts group panes differently.
    pub pane_weights: BTreeMap<Layout, BTreeMap<u64, f32>>,
    pub appearance: Option<Appearance>,
    #[serde(flatten)]
    pub floats: super::floating::Floats,
}
impl Workspace {
    pub fn new(id: u64, index: usize) -> Self {
        Self {
            id,
            index,
            panes: Vec::new(),
            focused: None,
            layout: Layout::Tile,
            main_ratio: 0.5,
            pane_weights: Default::default(),
            appearance: None,
            floats: Default::default(),
        }
    }
    pub fn focus(&mut self, id: u64) {
        self.focused = Some(id);
        self.reveal();
    }
    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
        self.reveal();
    }
    /// Lets the layout reorder panes so the focused one is on screen, after
    /// focus, layout, or pane order changes.
    fn reveal(&mut self) {
        if self.panes.len() >= 2 {
            self.layout.arrange().reveal(self);
        }
    }
    pub fn remove(&mut self, id: u64) {
        let position = self.panes.iter().position(|p| *p == id).unwrap_or(0);
        self.panes.retain(|p| *p != id);
        for weights in self.pane_weights.values_mut() {
            weights.remove(&id);
        }
        if self.focused == Some(id) {
            self.focused = self
                .panes
                .get(position.min(self.panes.len().saturating_sub(1)))
                .copied();
        }
        self.reveal();
    }
    /// The current layout's pane weights.
    pub(super) fn weights(&self) -> &BTreeMap<u64, f32> {
        static EMPTY: BTreeMap<u64, f32> = BTreeMap::new();
        self.pane_weights.get(&self.layout).unwrap_or(&EMPTY)
    }
    pub(super) fn weights_mut(&mut self) -> &mut BTreeMap<u64, f32> {
        self.pane_weights.entry(self.layout).or_default()
    }
    fn focused_index(&self) -> Option<usize> {
        self.panes.iter().position(|id| Some(*id) == self.focused)
    }
    /// Where each visible pane goes, as decided by the layout. A single pane
    /// fills the area in every layout.
    pub fn rectangles(&self, bounds: egui::Rect, style: &Appearance) -> Vec<(u64, egui::Rect)> {
        if self.panes.is_empty() {
            return Vec::new();
        }
        let area = Area::new(bounds, style, self.panes.len());
        if let [only] = self.panes[..] {
            return vec![(only, area.rect)];
        }
        self.layout.arrange().rectangles(self, &area)
    }
    /// Move the focused pane's nearest shared divider in the requested direction.
    pub fn resize(&mut self, horizontal: bool, forward: bool) {
        let Some(index) = self.focused_index().filter(|_| self.panes.len() >= 2) else {
            return;
        };
        let axis = if horizontal { Axis::X } else { Axis::Y };
        self.layout.arrange().resize(self, index, axis, forward);
    }
    /// Moves focus, or with `moving` moves the focused pane. `rects` are this
    /// workspace's current rectangles. Returns whether anything changed.
    pub(super) fn navigate(
        &mut self,
        motion: Motion,
        moving: bool,
        rects: &[(u64, egui::Rect)],
    ) -> bool {
        let Some(index) = self.focused_index().filter(|_| self.panes.len() >= 2) else {
            return false;
        };
        let arrange = self.layout.arrange();
        if !moving {
            return arrange.focus(self, index, motion, rects);
        }
        let moved = arrange.move_pane(self, index, motion, rects);
        self.reveal();
        moved
    }
    /// The focused pane's number for the status bar, or 0 without focus.
    pub fn pane_number(&self) -> usize {
        self.focused_index()
            .map_or(0, |index| self.layout.arrange().pane_number(self, index))
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CreateOptions {
    pub index: Option<usize>,
    /// `None` uses the configured `workspaces.default_layout`.
    pub layout: Option<Layout>,
}
/// `app:create_pane` options: the destination workspace (current by default)
/// and how to start the shell. Tiled panes have no names.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CreatePaneOptions {
    pub workspace: Option<u64>,
    pub command: Option<String>,
    pub cwd: Option<String>,
}
impl CreatePaneOptions {
    pub fn spawn(&self) -> super::pane::SpawnOptions {
        super::pane::SpawnOptions {
            command: self.command.clone(),
            cwd: self.cwd.clone(),
        }
    }
}

/// Removes workspaces without tiled panes and returns the floats they owned,
/// which close with them. Keep the final empty workspace as a teardown
/// sentinel until the host closes the native window; callbacks in the current
/// frame may still inspect it.
#[must_use = "floats of removed workspaces must be closed"]
pub(super) fn prune_empty(
    workspaces: &mut Vec<Workspace>,
    current: &mut u64,
    previous: &mut u64,
) -> Vec<u64> {
    let sentinel = workspaces.iter().all(|w| w.panes.is_empty());
    let orphaned = workspaces
        .iter()
        .filter(|w| w.panes.is_empty() && !(sentinel && w.id == *current))
        .flat_map(|w| w.floats.ids().to_vec())
        .collect();
    if sentinel {
        workspaces.retain(|w| w.id == *current);
        *previous = *current;
        return orphaned;
    }
    let old_index = workspaces
        .iter()
        .find(|w| w.id == *current)
        .map_or(1, |w| w.index);
    workspaces.retain(|w| !w.panes.is_empty());
    if !workspaces.iter().any(|w| w.id == *current) {
        *current = workspaces
            .iter()
            .find(|w| w.id == *previous)
            .or_else(|| workspaces.iter().find(|w| w.index >= old_index))
            .unwrap_or_else(|| workspaces.last().unwrap())
            .id;
    }
    if !workspaces.iter().any(|w| w.id == *previous) {
        *previous = *current;
    }
    orphaned
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", content = "args")]
pub(crate) enum Request {
    Create(CreateOptions),
    Ensure(CreateOptions),
    List,
    Get(Option<u64>),
    Select(u64),
    Remove(u64),
    SetLayout(u64, Layout),
    CreatePane(CreatePaneOptions),
    MovePane(u64, u64),
    /// Moves a float to another workspace and makes it that workspace's
    /// visible float.
    MoveFloat(u64, u64),
    ClosePane(u64),
    FocusPane(u64),
    ListPanes,
    Appearance(Appearance),
    SetAppearance(u64, Appearance),
    MainRatio(u64, f32),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::layout::Deck;
    #[test]
    fn resizing_moves_the_main_divider_in_each_layout() {
        let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
        for layout in [
            Layout::Tile,
            Layout::TileReversed,
            Layout::Deck,
            Layout::Grid,
            Layout::HorizontalGrid,
        ] {
            let mut w = Workspace::new(1, 1);
            w.layout = layout;
            w.panes = vec![1, 2, 3, 4];
            w.focus(2);
            let style = Appearance::default();
            let before = w.rectangles(bounds, &style);
            let horizontal = layout != Layout::HorizontalGrid;
            let edge = |rows: &Vec<(u64, egui::Rect)>| {
                if horizontal {
                    rows.iter()
                        .map(|(_, r)| r.right())
                        .filter(|x| *x < bounds.right() - style.outer_gap - 1.0)
                        .fold(f32::INFINITY, f32::min)
                } else {
                    rows[0].1.bottom()
                }
            };
            w.resize(horizontal, true);
            assert!(
                edge(&w.rectangles(bounds, &style)) > edge(&before),
                "{layout:?}"
            );
            w.resize(horizontal, false);
            assert!((edge(&w.rectangles(bounds, &style)) - edge(&before)).abs() < 0.01);
            assert_eq!(w.focused, Some(2));
        }
    }

    #[test]
    fn individual_resize_is_local_and_keeps_gaps_and_minimum_sizes() {
        let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
        let style = Appearance::default();
        for layout in [
            Layout::Tile,
            Layout::TileReversed,
            Layout::Grid,
            Layout::HorizontalGrid,
        ] {
            let mut w = Workspace::new(1, 1);
            w.layout = layout;
            w.panes = vec![1, 2, 3, 4, 5, 6];
            w.focus(5);
            let before = w.rectangles(bounds, &style);
            let horizontal = layout == Layout::HorizontalGrid;
            w.resize(horizontal, true);
            let after = w.rectangles(bounds, &style);
            assert_eq!(before[0], after[0], "unrelated pane changed: {layout:?}");
            assert_ne!(
                before[4].1, after[4].1,
                "focused pane unchanged: {layout:?}"
            );
            let saved = w.pane_weights.clone();
            w.focus(1);
            w.focus(5);
            assert_eq!(saved, w.pane_weights);
            for _ in 0..100 {
                w.resize(horizontal, true);
            }
            for _ in 0..100 {
                w.resize(!horizontal, false);
            }
            let rects = w.rectangles(bounds, &style);
            for (i, (_, r)) in rects.iter().enumerate() {
                assert!(
                    r.width() >= 47.99 && r.height() >= 47.99,
                    "{layout:?}: {r:?}"
                );
                assert!(bounds.contains_rect(*r));
                for (_, other) in &rects[i + 1..] {
                    assert!(!r.intersects(*other));
                }
            }
            w.remove(5);
            assert!(!w.weights().contains_key(&5));
        }
    }

    #[test]
    fn pane_sizes_are_kept_per_layout() {
        let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
        let style = Appearance::default();
        let mut w = Workspace::new(1, 1);
        w.panes = vec![1, 2, 3, 4];
        w.focus(2);
        w.resize(false, true);
        let tile = w.rectangles(bounds, &style);
        // Grid starts at equal sizes, and its own resizing leaves tile alone.
        w.set_layout(Layout::Grid);
        assert!(w.weights().is_empty());
        w.resize(false, false);
        w.set_layout(Layout::Monocle);
        w.set_layout(Layout::Tile);
        assert_eq!(w.rectangles(bounds, &style), tile);
        let json = serde_json::to_value(&w).unwrap();
        assert!(json["pane_weights"]["tile"].is_object());
        w.remove(2);
        assert!(w.pane_weights.values().all(|m| !m.contains_key(&2)));
    }
    #[test]
    fn singleton_and_monocle_resize_are_noops() {
        let mut w = Workspace::new(1, 1);
        w.panes = vec![1];
        w.focus(1);
        w.resize(true, true);
        w.resize(false, true);
        w.layout = Layout::Monocle;
        w.panes.push(2);
        w.resize(true, true);
        w.resize(false, true);
        assert_eq!(w.main_ratio, 0.5);
        assert!(w.pane_weights.is_empty());
    }
    #[test]
    fn horizontal_grid_displays_every_pane_in_two_rows() {
        let mut w = Workspace::new(1, 1);
        w.layout = Layout::HorizontalGrid;
        w.panes = vec![1, 2, 3, 4, 5];
        w.focus(5);
        let bounds = egui::Rect::from_min_size(egui::pos2(20.0, 30.0), egui::vec2(800.0, 600.0));
        let rects = w.rectangles(bounds, &Appearance::default());
        assert_eq!(rects.len(), 5);
        assert_eq!(rects[0].1.top(), rects[1].1.top());
        assert_eq!(rects[2].1.top(), rects[4].1.top());
        assert!(rects[0].1.bottom() < rects[2].1.top());
        assert!(rects[0].1.right() < rects[1].1.left());
        assert!(rects[2].1.right() < rects[3].1.left());
        assert!(rects[3].1.right() < rects[4].1.left());
        assert!(rects[0].1.width() > rects[2].1.width());
    }
    #[test]
    fn deck_cycles_stack_in_both_directions_without_visiting_main() {
        let mut w = Workspace::new(1, 1);
        w.layout = Layout::Deck;
        w.panes = vec![1, 2, 3, 4];
        w.focus(2);
        for expected in [3, 4, 2, 3, 4, 2] {
            Deck::cycle(&mut w, true);
            assert_eq!(w.focused, Some(expected));
            assert_eq!(w.panes[0], 1);
        }
        for expected in [4, 3, 2, 4, 3, 2] {
            Deck::cycle(&mut w, false);
            assert_eq!(w.focused, Some(expected));
            assert_eq!(w.panes[0], 1);
        }
        w.focus(4);
        Deck::cycle(&mut w, true);
        assert_eq!(w.focused, Some(2));
    }
    #[test]
    fn deck_rotation_from_main_matches_swm() {
        for (forward, expected) in [(true, vec![3, 2, 4, 1]), (false, vec![4, 2, 3, 1])] {
            let mut w = Workspace::new(1, 1);
            w.layout = Layout::Deck;
            w.panes = vec![1, 2, 3, 4];
            w.focus(1);
            Deck::cycle(&mut w, forward);
            assert_eq!(w.panes, expected);
            assert_eq!(w.focused, Some(expected[0]));
        }
        let mut w = Workspace::new(1, 1);
        w.panes = vec![1, 2];
        w.focus(2);
        Deck::cycle(&mut w, true);
        Deck::cycle(&mut w, false);
        assert_eq!(w.panes, vec![1, 2]);
        assert_eq!(w.focused, Some(2));
    }
    #[test]
    fn closing_last_pane_removes_workspace_and_restores_last_used() {
        let mut first = Workspace::new(1, 1);
        first.panes = vec![10];
        first.focus(10);
        let mut second = Workspace::new(2, 2);
        second.panes = vec![20, 21];
        second.focus(20);
        let mut workspaces = vec![first, second];
        let mut current = 2;
        let mut previous = 1;
        workspaces[1].remove(20);
        assert!(prune_empty(&mut workspaces, &mut current, &mut previous).is_empty());
        assert_eq!(workspaces.len(), 2);
        assert_eq!(current, 2);
        workspaces[1].remove(21);
        assert!(prune_empty(&mut workspaces, &mut current, &mut previous).is_empty());
        assert_eq!(workspaces.len(), 1);
        assert_eq!(current, 1);
        assert_eq!(previous, 1);
        workspaces[0].remove(10);
        assert!(prune_empty(&mut workspaces, &mut current, &mut previous).is_empty());
        assert!(workspaces.iter().all(|w| w.panes.is_empty()));
        assert_eq!(workspaces[0].id, current);
    }
    #[test]
    fn pruned_workspaces_return_their_floats_except_the_sentinel() {
        let mut first = Workspace::new(1, 1);
        first.panes = vec![10];
        first.floats.add(100);
        let mut second = Workspace::new(2, 2);
        second.floats.add(200);
        second.floats.add(201);
        let mut workspaces = vec![first, second];
        let (mut current, mut previous) = (1, 2);
        assert_eq!(
            prune_empty(&mut workspaces, &mut current, &mut previous),
            vec![200, 201]
        );
        workspaces[0].remove(10);
        // The window is closing; its last workspace keeps its floats for teardown.
        assert!(prune_empty(&mut workspaces, &mut current, &mut previous).is_empty());
        assert_eq!(workspaces[0].floats.ids(), &[100]);
    }
    #[test]
    fn moving_visible_float_shows_next_and_shows_it_in_destination() {
        let mut source = Workspace::new(1, 1);
        source.floats.add(100);
        source.floats.add(101);
        let mut shown = Workspace::new(2, 2);
        shown.floats.add(200);
        let mut hidden = Workspace::new(3, 3);
        hidden.floats.add(300);
        hidden.floats.hide();
        // The same steps as `Request::MoveFloat`.
        source.floats.remove(101);
        shown.floats.add(101);
        assert_eq!(source.floats.visible(), Some(100));
        assert_eq!(shown.floats.visible(), Some(101));
        source.floats.remove(100);
        hidden.floats.add(100);
        assert_eq!(source.floats.visible(), None);
        assert_eq!(hidden.floats.visible(), Some(100));
    }
    #[test]
    fn removing_background_workspace_keeps_focus_and_indexes() {
        let mut first = Workspace::new(1, 1);
        first.panes = vec![10];
        let second = Workspace::new(2, 7);
        let mut workspaces = vec![first, second];
        let mut current = 1;
        let mut previous = 2;
        assert!(prune_empty(&mut workspaces, &mut current, &mut previous).is_empty());
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].index, 1);
        assert_eq!((current, previous), (1, 1));
    }
    #[test]
    fn layouts_keep_rectangles_inside_tiny_and_large_windows() {
        for layout in [
            Layout::Tile,
            Layout::TileReversed,
            Layout::Deck,
            Layout::HorizontalGrid,
            Layout::Monocle,
            Layout::Grid,
        ] {
            for count in 0..20 {
                for size in [1.0, 80.0, 1200.0] {
                    let mut w = Workspace::new(1, 1);
                    w.layout = layout;
                    w.panes = (0..count).collect();
                    w.focused = w.panes.last().copied();
                    let b = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size, size));
                    for (_, r) in w.rectangles(b, &Appearance::default()) {
                        assert!(r.is_finite() && r.width() >= 0.0 && r.height() >= 0.0);
                        assert!(b.contains_rect(r));
                    }
                }
            }
        }
    }
    #[test]
    fn removing_focus_selects_a_neighbor_without_reordering() {
        let mut w = Workspace::new(1, 1);
        w.panes = vec![1, 2, 3];
        w.focused = Some(2);
        w.remove(2);
        assert_eq!(w.panes, vec![1, 3]);
        assert_eq!(w.focused, Some(3));
    }
    #[test]
    fn deck_exposes_focused_stack_and_monocle_only_focus() {
        let mut w = Workspace::new(1, 1);
        w.panes = vec![1, 2, 3];
        w.focus(3);
        w.set_layout(Layout::Deck);
        assert_eq!(w.panes, vec![1, 3, 2]);
        let b = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        assert_eq!(
            w.rectangles(b, &Appearance::default())
                .iter()
                .map(|v| v.0)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        w.layout = Layout::Monocle;
        assert_eq!(w.rectangles(b, &Appearance::default())[0].0, 3);
    }
}
