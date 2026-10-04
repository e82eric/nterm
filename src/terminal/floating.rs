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
        for (label, value) in [
            ("name", &self.name),
            ("command", &self.command),
            ("cwd", &self.cwd),
        ] {
            if value
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.contains('\0'))
            {
                return Err(format!(
                    "floating pane {label} must be nonempty and contain no NUL"
                ));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", content = "args")]
pub(crate) enum Request {
    Create(CreateOptions),
    Toggle,
    Restore(Target),
    Get(Option<Target>),
    List,
    Next,
    Previous,
    Close(Option<Target>),
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Description {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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
pub(crate) struct Panes<T> {
    pub entries: Vec<Entry<T>>,
    visible: Option<u64>,
    recent: Vec<u64>,
    next_id: u64,
}
impl<T> Default for Panes<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            visible: None,
            recent: Vec::new(),
            next_id: 1,
        }
    }
}
impl<T> Panes<T> {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn visible_id(&self) -> Option<u64> {
        self.visible
    }
    pub fn last_id(&self) -> Option<u64> {
        self.recent.last().copied()
    }
    pub fn visible(&self) -> Option<&Entry<T>> {
        let id = self.visible?;
        self.entries.iter().find(|e| e.id == id)
    }
    pub fn visible_mut(&mut self) -> Option<&mut Entry<T>> {
        let id = self.visible?;
        self.entries.iter_mut().find(|e| e.id == id)
    }
    pub fn resolve(&self, target: &Target) -> Option<u64> {
        self.entries
            .iter()
            .find(|e| match target {
                Target::Id(id) => e.id == *id,
                Target::Name(name) => e.name.as_ref() == Some(name),
            })
            .map(|e| e.id)
    }
    pub fn validate_create(&self, options: &CreateOptions) -> Result<(), String> {
        options.validate()?;
        if options
            .name
            .as_ref()
            .is_some_and(|n| self.resolve(&Target::Name(n.clone())).is_some())
        {
            return Err("floating pane name already exists".into());
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
        self.restore(id);
        id
    }
    pub fn restore(&mut self, id: u64) {
        debug_assert!(self.entries.iter().any(|e| e.id == id));
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
        let current = self.visible.or_else(|| self.last_id());
        let at = self
            .entries
            .iter()
            .position(|e| Some(e.id) == current)
            .unwrap_or(0);
        let len = self.entries.len();
        let at = if next {
            (at + 1) % len
        } else {
            (at + len - 1) % len
        };
        self.restore(self.entries[at].id);
    }
    pub fn remove(&mut self, id: u64) -> Option<T> {
        let at = self.entries.iter().position(|e| e.id == id)?;
        let entry = self.entries.remove(at);
        self.recent.retain(|i| *i != id);
        if self.visible == Some(id) {
            self.visible = self.last_id();
        }
        Some(entry.value)
    }
    pub fn descriptions(&self) -> Vec<Description> {
        self.entries
            .iter()
            .map(|e| Description {
                id: e.id,
                name: e.name.clone(),
                visible: self.visible == Some(e.id),
                focused: self.visible == Some(e.id),
            })
            .collect()
    }
    pub fn description(&self, id: u64) -> Option<Description> {
        self.descriptions().into_iter().find(|p| p.id == id)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_keeps_ids_and_creation_order_and_restores_recent() {
        let mut p = Panes::default();
        let a = p.insert(Some("tools".into()), ());
        let b = p.insert(None, ());
        let c = p.insert(None, ());
        p.restore(a);
        p.hide();
        assert_eq!(p.last_id(), Some(a));
        p.cycle(true);
        assert_eq!(p.visible_id(), Some(b));
        p.cycle(false);
        assert_eq!(p.visible_id(), Some(a));
        p.cycle(false);
        assert_eq!(p.visible_id(), Some(c));
        p.remove(c);
        assert_eq!(p.visible_id(), Some(a));
        p.remove(b);
        assert_eq!(p.visible_id(), Some(a));
        assert_eq!(p.insert(None, ()), 4);
        assert_eq!(p.resolve(&Target::Name("tools".into())), Some(a));
        assert_eq!(
            p.descriptions().iter().map(|d| d.id).collect::<Vec<_>>(),
            vec![a, 4]
        );
        assert!(
            p.validate_create(&CreateOptions {
                name: Some("tools".into()),
                ..Default::default()
            })
            .is_err()
        );
    }
    #[test]
    fn hidden_close_does_not_restore_and_empty_cycle_is_safe() {
        let mut p = Panes::default();
        p.cycle(true);
        let a = p.insert(None, ());
        let b = p.insert(None, ());
        p.hide();
        p.remove(b);
        assert_eq!(p.visible_id(), None);
        assert_eq!(p.last_id(), Some(a));
        p.remove(a);
        assert!(p.is_empty());
    }
}
