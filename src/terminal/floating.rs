use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum Target {
    Id(u64),
    Name(String),
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateOptions {
    pub name: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
}
impl CreateOptions {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .name
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.contains('\0'))
        {
            return Err("floating pane name must be nonempty and contain no NUL".into());
        }
        self.spawn().validate()
    }
    pub fn spawn(&self) -> super::pane::SpawnOptions {
        super::pane::SpawnOptions {
            command: self.command.clone(),
            cwd: self.cwd.clone(),
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
    Restore(Target),
    Get {
        target: Option<Target>,
        workspace: Option<u64>,
    },
    List {
        workspace: Option<u64>,
    },
    Next,
    Previous,
    Close(Option<Target>),
    /// Turns a tiled pane into a float in its workspace.
    Float(u64),
    /// Turns a float into a tiled pane after the focused pane.
    Tile(Option<Target>),
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Description {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub workspace: u64,
    pub visible: bool,
    pub focused: bool,
}
pub(crate) struct Entry<T> {
    pub id: u64,
    pub name: Option<String>,
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
/// while the workspace is current, and focus history for toggle and close.
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct Floats {
    #[serde(rename = "floats")]
    ids: Vec<u64>,
    #[serde(rename = "visible_float")]
    visible: Option<u64>,
    #[serde(skip)]
    recent: Vec<u64>,
}
impl Floats {
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
    /// Finds a float by ID or name among one workspace's floats.
    pub fn resolve(&self, target: &Target, floats: &Floats) -> Option<u64> {
        floats.ids().iter().copied().find(|id| match target {
            Target::Id(target) => id == target,
            Target::Name(name) => self.get(*id).and_then(|e| e.name.as_ref()) == Some(name),
        })
    }
    /// Names are unique within a workspace.
    pub fn validate_create(&self, options: &CreateOptions, floats: &Floats) -> Result<(), String> {
        options.validate()?;
        if options
            .name
            .as_ref()
            .is_some_and(|n| self.resolve(&Target::Name(n.clone()), floats).is_some())
        {
            return Err("floating pane name already exists in this workspace".into());
        }
        if self.next_id > i64::MAX as u64 {
            return Err("floating pane IDs exhausted".into());
        }
        Ok(())
    }
    pub fn insert(&mut self, name: Option<String>, value: T) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(Entry { id, name, value });
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
        let entry = self.get(id)?;
        let visible = floats.visible() == Some(id);
        Some(Description {
            id,
            name: entry.name.clone(),
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
    fn each_workspace_keeps_its_own_floats_visibility_and_names() {
        let mut panes = Panes::default();
        let (mut first, mut second) = (Floats::default(), Floats::default());
        let a = panes.insert(Some("tools".into()), ());
        first.add(a);
        let b = panes.insert(Some("tools".into()), ());
        second.add(b);
        second.hide();
        assert_eq!(first.visible(), Some(a));
        assert_eq!(second.visible(), None);
        assert_eq!(
            panes.resolve(&Target::Name("tools".into()), &first),
            Some(a)
        );
        assert_eq!(
            panes.resolve(&Target::Name("tools".into()), &second),
            Some(b)
        );
        assert_eq!(panes.resolve(&Target::Id(b), &first), None);
        let tools = CreateOptions {
            name: Some("tools".into()),
            ..Default::default()
        };
        assert!(panes.validate_create(&tools, &first).is_err());
        assert!(panes.validate_create(&tools, &Floats::default()).is_ok());
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
        let [a, b, c] = [(); 3].map(|_| panes.insert(None, ()));
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
        let a = panes.insert(None, ());
        assert!(panes.remove(a).is_some());
        assert!(panes.remove(a).is_none());
        assert_eq!(panes.insert(None, ()), a + 1);
    }
}
