use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateOptions {
    pub command: Option<String>,
    pub cwd: Option<String>,
}
impl CreateOptions {
    pub fn validate(&self) -> Result<(), String> {
        self.spawn().validate()
    }
    pub fn spawn(&self) -> super::pane::SpawnOptions {
        super::pane::SpawnOptions {
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            ..Default::default()
        }
    }
}
/// Floating pane requests act on the current workspace's floats; `Get` and
/// `List` can inspect another workspace.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", content = "args")]
pub(crate) enum Request {
    Create(CreateOptions),
    Toggle,
    Restore(u64),
    Get {
        target: Option<u64>,
        workspace: Option<u64>,
    },
    List {
        workspace: Option<u64>,
    },
    Close(Option<u64>),
    /// Turns a tiled pane into a float in its workspace.
    Float(u64),
    /// Turns a float into a tiled pane after the focused pane.
    Tile(Option<u64>),
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Description {
    pub id: u64,
    pub workspace: u64,
    pub visible: bool,
    pub focused: bool,
}
pub(crate) struct Entry<T> {
    pub id: u64,
    pub value: T,
}
impl<T> std::ops::Deref for Entry<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> std::ops::DerefMut for Entry<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}
/// One workspace's floating panes: IDs in creation order, the one shown
/// while the workspace is current, focus history for toggle and close, and
/// the size the mouse gave them.
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct Floats {
    #[serde(rename = "floats")]
    ids: Vec<u64>,
    #[serde(rename = "visible_float")]
    visible: Option<u64>,
    #[serde(skip)]
    recent: Vec<u64>,
    /// Set by resizing with the mouse, as percentages of the workspace like
    /// the configured padding; `None` uses the configured or default size.
    #[serde(skip)]
    size: Option<crate::config::FloatingPadding>,
}
impl Floats {
    /// This workspace's floating size, if the mouse has set one.
    pub fn size(&self) -> Option<crate::config::FloatingPadding> {
        self.size
    }
    pub fn set_size(&mut self, size: Option<crate::config::FloatingPadding>) {
        self.size = size;
    }
    pub fn ids(&self) -> &[u64] {
        &self.ids
    }
    pub fn contains(&self, id: u64) -> bool {
        self.ids.contains(&id)
    }
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
    pub fn visible(&self) -> Option<u64> {
        self.visible
    }
    pub fn last(&self) -> Option<u64> {
        self.recent.last().copied()
    }
    pub fn add(&mut self, id: u64) {
        self.ids.push(id);
        self.show(id);
    }
    pub fn show(&mut self, id: u64) {
        debug_assert!(self.contains(id));
        self.visible = Some(id);
        self.recent.retain(|i| *i != id);
        self.recent.push(id);
    }
    pub fn hide(&mut self) {
        self.visible = None;
    }
    pub fn cycle(&mut self, next: bool) {
        if self.is_empty() {
            return;
        }
        let current = self.visible.or_else(|| self.last());
        let at = self
            .ids
            .iter()
            .position(|id| Some(*id) == current)
            .unwrap_or(0);
        let len = self.ids.len();
        let at = if next {
            (at + 1) % len
        } else {
            (at + len - 1) % len
        };
        self.show(self.ids[at]);
    }
    /// Closing the visible float shows the most recently focused remaining one.
    pub fn remove(&mut self, id: u64) -> bool {
        let Some(at) = self.ids.iter().position(|i| *i == id) else {
            return false;
        };
        self.ids.remove(at);
        self.recent.retain(|i| *i != id);
        if self.visible == Some(id) {
            self.visible = self.last();
        }
        true
    }
}
/// The window's floating terminals. Which workspace owns each one, and
/// which is visible, is recorded in that workspace's `Floats`.
pub(crate) struct Panes<T> {
    pub entries: Vec<Entry<T>>,
    next_id: u64,
}
impl<T> Default for Panes<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 1,
        }
    }
}
impl<T> Panes<T> {
    pub fn get(&self, id: u64) -> Option<&Entry<T>> {
        self.entries.iter().find(|e| e.id == id)
    }
    pub fn get_mut(&mut self, id: u64) -> Option<&mut Entry<T>> {
        self.entries.iter_mut().find(|e| e.id == id)
    }
    /// A float by its ID, among one workspace's floats.
    pub fn resolve(&self, target: u64, floats: &Floats) -> Option<u64> {
        floats.contains(target).then_some(target)
    }
    pub fn validate_create(&self, options: &CreateOptions) -> Result<(), String> {
        options.validate()?;
        if self.next_id > i64::MAX as u64 {
            return Err("floating pane IDs exhausted".into());
        }
        Ok(())
    }
    pub fn insert(&mut self, value: T) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(Entry { id, value });
        id
    }
    pub fn remove(&mut self, id: u64) -> Option<T> {
        let at = self.entries.iter().position(|e| e.id == id)?;
        Some(self.entries.remove(at).value)
    }
    /// `current` says whether the workspace is shown, so its visible float
    /// is also the window's input target.
    pub fn descriptions(&self, workspace: u64, floats: &Floats, current: bool) -> Vec<Description> {
        floats
            .ids()
            .iter()
            .filter_map(|id| self.description(*id, workspace, floats, current))
            .collect()
    }
    pub fn description(
        &self,
        id: u64,
        workspace: u64,
        floats: &Floats,
        current: bool,
    ) -> Option<Description> {
        if !floats.contains(id) {
            return None;
        }
        self.get(id)?;
        let visible = floats.visible() == Some(id);
        Some(Description {
            id,
            workspace,
            visible,
            focused: visible && current,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_workspace_keeps_its_own_floats_and_visibility() {
        let mut panes = Panes::default();
        let (mut first, mut second) = (Floats::default(), Floats::default());
        let a = panes.insert(());
        first.add(a);
        let b = panes.insert(());
        second.add(b);
        second.hide();
        assert_eq!(first.visible(), Some(a));
        assert_eq!(second.visible(), None);
        assert_eq!(panes.resolve(a, &first), Some(a));
        // A float is found only among its own workspace's.
        assert_eq!(panes.resolve(b, &first), None);
        let described = panes.descriptions(2, &second, false);
        assert_eq!(described.len(), 1);
        assert_eq!((described[0].id, described[0].workspace), (b, 2));
        assert!(!described[0].visible && !described[0].focused);
        assert!(
            panes
                .description(a, 1, &first, false)
                .is_some_and(|d| d.visible && !d.focused)
        );
    }
    #[test]
    fn cycling_stays_within_the_workspace_and_restores_recent() {
        let mut panes = Panes::default();
        let mut floats = Floats::default();
        let mut other = Floats::default();
        let [a, b, c] = [(); 3].map(|_| panes.insert(()));
        floats.add(a);
        other.add(b);
        floats.add(c);
        floats.show(a);
        floats.hide();
        assert_eq!(floats.last(), Some(a));
        floats.cycle(true);
        assert_eq!(floats.visible(), Some(c));
        floats.cycle(true);
        assert_eq!(floats.visible(), Some(a));
        floats.cycle(false);
        assert_eq!(floats.visible(), Some(c));
        assert!(floats.remove(c));
        assert_eq!(floats.visible(), Some(a));
        assert!(!floats.remove(b));
        assert_eq!(other.visible(), Some(b));
    }
    #[test]
    fn hidden_close_does_not_restore_and_empty_cycle_is_safe() {
        let mut floats = Floats::default();
        floats.cycle(true);
        floats.add(1);
        floats.add(2);
        floats.hide();
        floats.remove(2);
        assert_eq!(floats.visible(), None);
        assert_eq!(floats.last(), Some(1));
        floats.remove(1);
        assert!(floats.is_empty());
    }
    #[test]
    fn removing_visible_float_shows_next_until_none_remain() {
        let mut floats = Floats::default();
        floats.add(1);
        floats.add(2);
        assert!(floats.remove(2));
        assert_eq!(floats.visible(), Some(1));
        assert!(floats.remove(1));
        assert_eq!(floats.visible(), None);
    }
    #[test]
    fn ids_stay_unique_across_removal() {
        let mut panes = Panes::default();
        let a = panes.insert(());
        assert!(panes.remove(a).is_some());
        assert!(panes.remove(a).is_none());
        assert_eq!(panes.insert(()), a + 1);
    }
}
