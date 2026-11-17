//! Sessions: a workspace's shells and its layout, kept in the session daemon.
//!
//! Shells that run in the daemon outlive the window that showed them. Each
//! workspace is one session: it describes itself in terms of the daemon's
//! panes, and the description is kept in the daemon whenever it changes.
//! Closing a window, or detaching a workspace, leaves its session waiting;
//! any window can take a waiting session back as a workspace. A window is
//! only a view and nothing about it is kept.
//!
//! Everything here runs on the UI thread.

use super::{
    FloatingTerminal, TerminalView,
    workspace::{self, Appearance, Layout},
};
use crate::{actions::AppAction, graphics as glow};
use nterm_session::protocol::PaneInfo;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Bumped when a description stops meaning what it did.
const VERSION: u32 = 1;

/// One workspace, by the daemon's pane numbers.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    version: u32,
    layout: Layout,
    main_ratio: Option<f32>,
    panes: Vec<u32>,
    focused: Option<u32>,
    weights: BTreeMap<Layout, BTreeMap<u32, f32>>,
    appearance: Option<Appearance>,
    floats: Vec<Float>,
    visible_float: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Float {
    pane: u32,
    name: Option<String>,
}

/// A session nothing is showing.
pub(super) struct Detached {
    /// Empty for the shells no session describes.
    id: String,
    saved: Saved,
    label: String,
    detail: String,
}

impl Saved {
    /// Every shell the workspace holds, tiled and floating.
    fn members(&self) -> Vec<u32> {
        self.panes.iter().copied().chain(self.floats.iter().map(|float| float.pane)).collect()
    }

    /// Cut down to the shells `keep` accepts. Floats belong to a workspace,
    /// which needs a tiled pane: left without one, they are tiled.
    fn keep_only(&mut self, mut keep: impl FnMut(u32) -> bool) {
        self.panes.retain(|pane| keep(*pane));
        self.floats.retain(|float| keep(float.pane));
        if self.panes.is_empty() {
            self.panes = self.floats.drain(..).map(|float| float.pane).collect();
            self.visible_float = None;
        }
    }
}

/// A new session's name in the daemon. It only has to differ from the
/// others there.
fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{time:x}-{:x}-{:x}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

/// The sessions nothing is showing, among those in `listing`. The waiting
/// shells that none of them lays out come last, as one more: those in no
/// session, and those whose session this build cannot read.
pub(super) fn detached(listing: &crate::session::Listing) -> Vec<Detached> {
    let waiting: HashMap<u32, &PaneInfo> = listing
        .sessions
        .iter()
        .flat_map(|session| &session.panes)
        .filter(|pane| crate::session::waiting(pane))
        .map(|pane| (pane.pane, pane))
        .collect();
    let mut placed = HashSet::new();
    let mut sessions = Vec::new();
    for session in &listing.sessions {
        // The daemon says which shells are the session's; the layout says
        // where they go.
        let members: HashSet<u32> = session.panes.iter().map(|pane| pane.pane).collect();
        let saved = serde_json::from_str::<Saved>(&session.layout).ok();
        let Some(mut saved) = saved.filter(|saved| saved.version == VERSION) else { continue };
        saved.keep_only(|pane| {
            members.contains(&pane) && waiting.contains_key(&pane) && placed.insert(pane)
        });
        if !saved.panes.is_empty() {
            sessions.push(describe(session.id.clone(), saved, &waiting));
        }
    }
    let mut strays: Vec<u32> =
        waiting.keys().copied().filter(|pane| !placed.contains(pane)).collect();
    if !strays.is_empty() {
        strays.sort_unstable();
        let saved = Saved { version: VERSION, panes: strays, ..Saved::default() };
        sessions.push(describe(String::new(), saved, &waiting));
    }
    sessions
}

/// What the picker says of a session: what runs in it, how many panes and
/// where.
fn describe(id: String, saved: Saved, panes: &HashMap<u32, &PaneInfo>) -> Detached {
    let all: Vec<&PaneInfo> = saved
        .panes
        .iter()
        .chain(saved.floats.iter().map(|float| &float.pane))
        .filter_map(|pane| panes.get(pane).copied())
        .collect();
    let name = |pane: &PaneInfo| {
        let title = pane.title.as_deref().filter(|title| !title.trim().is_empty());
        let name = title.unwrap_or(&pane.command);
        // A path says less than the program it ends in.
        let short = name.rsplit(['\\', '/']).next().filter(|short| !short.is_empty());
        short.unwrap_or(name).to_owned()
    };
    let mut names: Vec<String> = all.iter().take(3).map(|pane| name(pane)).collect();
    if all.len() > names.len() {
        names.push(format!("+{}", all.len() - names.len()));
    }
    let count = match all.len() {
        1 => "1 pane".to_owned(),
        count => format!("{count} panes"),
    };
    let place = all.iter().find_map(|pane| pane.pwd.clone());
    Detached {
        label: format!(
            "{}{}",
            if id.is_empty() { "Shells without a layout: " } else { "Session: " },
            names.join(", ")
        ),
        detail: match place {
            Some(place) => format!("{} / {count} / {place}", saved.layout.tag()),
            None => format!("{} / {count}", saved.layout.tag()),
        },
        id,
        saved,
    }
}

/// The picker rows that take a waiting session.
fn entries(sessions: &[Detached]) -> Vec<crate::actions::Entry> {
    sessions
        .iter()
        .map(|session| crate::actions::Entry {
            id: format!("session.{}", session.id),
            label: session.label.clone(),
            shortcut: Some(session.detail.clone()),
            action: AppAction::AttachSession(session.id.clone()),
        })
        .collect()
}

/// What a picker that offers the waiting sessions was made from: the list,
/// and the rows it gave. The daemon gives a new list for changes that the
/// rows do not show, and the picker is made again only when they differ.
pub(super) struct Offered {
    stamp: (u64, u64),
    rows: Vec<(String, String, Option<String>)>,
}

/// What differs between two sets of rows that a picker would show.
fn shown(rows: &[crate::actions::Entry]) -> Vec<(String, String, Option<String>)> {
    rows.iter().map(|row| (row.id.clone(), row.label.clone(), row.shortcut.clone())).collect()
}

impl TerminalView {
    /// A picker's rows for the sessions that are waiting now. They are
    /// remembered, for `sessions_changed` to tell when the picker is out of
    /// date.
    pub(super) fn offer_waiting(&mut self) -> Vec<crate::actions::Entry> {
        let Some(listing) = crate::session::sessions() else {
            self.offered = None;
            return Vec::new();
        };
        let rows = entries(&detached(&listing));
        self.offered = Some(Offered { stamp: listing.stamp(), rows: shown(&rows) });
        rows
    }

    /// Whether the picker that `offer_waiting` last gave rows to would show
    /// other rows now.
    pub(super) fn sessions_changed(&mut self) -> bool {
        let Some(offered) = &mut self.offered else { return false };
        let Some(listing) = crate::session::sessions() else { return false };
        if listing.stamp() == offered.stamp {
            return false;
        }
        offered.stamp = listing.stamp();
        shown(&entries(&detached(&listing))) != offered.rows
    }

    /// Every frame: the workspace picker is made again, with what was typed
    /// into it, when the sessions it offers have changed.
    pub(super) fn follow_sessions(&mut self) {
        if self.offered.is_none() || self.empty.is_some() {
            return;
        }
        let query = self
            .active_pane()
            .and_then(|pane| self.pane_mut(pane))
            .and_then(|pane| pane.workspace_picker_query());
        let Some(query) = query else {
            // The picker has closed.
            self.offered = None;
            return;
        };
        if self.sessions_changed() {
            let ctx = self.context.clone();
            self.show_workspace_picker(&ctx, false, Some(query));
        }
    }

    /// Show a waiting session as a new workspace of this window, and go to
    /// it. In a window that had no terminal it is the only workspace.
    pub(super) fn attach_session(&mut self, id: &str, gl: &glow::Context) -> Result<(), String> {
        // The sessions as they are now, which the rows chosen from followed.
        let listing = crate::session::sessions().ok_or("That session is no longer waiting.")?;
        let session = detached(&listing)
            .into_iter()
            .find(|session| session.id == id)
            .ok_or("That session is no longer waiting.")?;
        let saved = session.saved;
        let index = self.free_index();
        let workspace_id = self.push_workspace(index, Some(saved.layout)).id;
        // Daemon pane to this window's tiled and floating panes.
        let mut tiled: HashMap<u32, u64> = HashMap::new();
        let mut floating: HashMap<u32, u64> = HashMap::new();
        for number in &saved.panes {
            let Some(pane) = self.adopt(*number, &listing, gl) else { continue };
            let id = self.next_pane;
            self.next_pane += 1;
            self.tiled.push((id, pane));
            tiled.insert(*number, id);
            self.workspace_by_id(workspace_id).panes.push(id);
        }
        if tiled.is_empty() {
            self.workspaces.retain(|workspace| workspace.id != workspace_id);
            return Err("None of the session's shells could be shown.".into());
        }
        for float in &saved.floats {
            let Some(pane) = self.adopt(float.pane, &listing, gl) else { continue };
            let id = self.floating.insert(float.name.clone(), FloatingTerminal { pane });
            floating.insert(float.pane, id);
            self.workspace_by_id(workspace_id).floats.add(id);
        }
        let visible = saved.visible_float.and_then(|pane| floating.get(&pane)).copied();
        let focused = saved.focused.and_then(|pane| tiled.get(&pane)).copied();
        let weights = saved
            .weights
            .iter()
            .map(|(layout, weights)| {
                let weights = weights
                    .iter()
                    .filter_map(|(pane, weight)| Some((*tiled.get(pane)?, *weight)))
                    .collect();
                (*layout, weights)
            })
            .collect();
        let workspace = self.workspace_by_id(workspace_id);
        workspace.layout = saved.layout;
        if let Some(ratio) = saved.main_ratio.filter(|ratio| ratio.is_finite()) {
            workspace.main_ratio = ratio.clamp(0.1, 0.9);
        }
        workspace.pane_weights = weights;
        workspace.appearance =
            saved.appearance.clone().filter(|appearance| appearance.validate().is_ok());
        match visible {
            Some(id) => workspace.floats.show(id),
            None => workspace.floats.hide(),
        }
        if let Some(id) = focused.or(workspace.panes.first().copied()) {
            workspace.focus(id);
        }
        if !id.is_empty() {
            self.sessions.insert(workspace_id, id.to_owned());
        }
        self.select_workspace(workspace_id)?;
        // A window that had no terminal had a workspace with nothing in it.
        self.prune_workspaces();
        workspace::renumber(&mut self.workspaces);
        self.context.request_repaint();
        Ok(())
    }

    /// A pane on a shell the daemon is running. `None`, with the error
    /// shown, when the terminal could not be made.
    fn adopt(
        &mut self,
        number: u32,
        listing: &crate::session::Listing,
        gl: &glow::Context,
    ) -> Option<super::TerminalPane> {
        let Some(adopt) = listing.adopt(number) else {
            self.window_error(format!("Shell {number} is no longer waiting."));
            return None;
        };
        let options = super::pane::SpawnOptions {
            start: crate::session::Start::Adopt(Box::new(adopt)),
            ..Default::default()
        };
        let pane = self.create_pane(gl, &options);
        match pane {
            Ok(pane) => Some(pane),
            Err(error) => {
                self.window_error(format!("A running shell could not be shown: {error}"));
                None
            }
        }
    }

    fn workspace_by_id(&mut self, id: u64) -> &mut workspace::Workspace {
        self.workspaces
            .iter_mut()
            .find(|workspace| workspace.id == id)
            .expect("the workspace was just made or found")
    }

    /// A workspace by the daemon's pane numbers. Panes with their own
    /// shells, and ones the daemon has not numbered yet, are left out.
    /// `None` when that leaves nothing.
    fn describe(&self, workspace: &workspace::Workspace) -> Option<Saved> {
        let number = |id: &u64| self.tiled_ref(*id).and_then(|pane| pane.session_pane());
        let panes: Vec<u32> = workspace.panes.iter().filter_map(number).collect();
        if panes.is_empty() {
            return None;
        }
        let float = |id: &u64| {
            let entry = self.floating.get(*id)?;
            Some((entry.value.pane.session_pane()?, entry.name.clone()))
        };
        let weights = workspace
            .pane_weights
            .iter()
            .map(|(layout, weights)| {
                let weights = weights
                    .iter()
                    .filter_map(|(id, weight)| Some((number(id)?, *weight)))
                    .collect();
                (*layout, weights)
            })
            .collect();
        Some(Saved {
            version: VERSION,
            layout: workspace.layout,
            main_ratio: Some(workspace.main_ratio),
            focused: workspace.focused.as_ref().and_then(number),
            weights,
            appearance: workspace.appearance.clone(),
            floats: workspace
                .floats
                .ids()
                .iter()
                .filter_map(float)
                .map(|(pane, name)| Float { pane, name })
                .collect(),
            visible_float: workspace.floats.visible().and_then(|id| float(&id)).map(|(pane, _)| pane),
            panes,
        })
    }

    /// Keep the daemon's copy of each workspace up to date, and drop the
    /// sessions whose workspaces have gone because their panes were closed.
    /// Called every frame; the daemon hears only of changes.
    pub(super) fn save_sessions(&mut self) {
        let mut current = HashSet::new();
        for workspace in &self.workspaces {
            let described = self.describe(workspace).and_then(|saved| {
                Some((saved.members(), serde_json::to_string(&saved).ok()?))
            });
            let Some((members, text)) = described
            else {
                // Nothing the daemon has numbered yet; what was sent stands.
                if let Some(id) = self.sessions.get(&workspace.id) {
                    current.insert(id.clone());
                }
                continue;
            };
            let id = self.sessions.entry(workspace.id).or_insert_with(new_id).clone();
            if self.sessions_sent.get(&id) != Some(&text) {
                crate::session::set_session(id.clone(), members, text.clone());
                self.sessions_sent.insert(id.clone(), text);
            }
            current.insert(id);
        }
        self.sessions.retain(|workspace, _| self.workspaces.iter().any(|w| w.id == *workspace));
        self.sessions_sent.retain(|id, _| {
            let kept = current.contains(id);
            if !kept {
                crate::session::remove_session(id.clone());
            }
            kept
        });
    }

    /// Take the current workspace out of this window and leave its session
    /// waiting in the daemon. The last workspace takes the window with it.
    pub(super) fn detach_workspace(&mut self) -> Result<(), String> {
        self.save_sessions();
        let id = self.current;
        let workspace = self.workspace();
        let kept = |pane: Option<&super::TerminalPane>| {
            pane.is_some_and(|pane| pane.session_pane().is_some())
        };
        let all_kept = workspace.panes.iter().all(|pane| kept(self.tiled_ref(*pane)))
            && workspace
                .floats
                .ids()
                .iter()
                .all(|float| kept(self.floating.get(*float).map(|entry| &entry.value.pane)));
        let session = self.sessions.get(&id).filter(|id| self.sessions_sent.contains_key(*id));
        let Some(session) = session.cloned().filter(|_| all_kept) else {
            return Err("This workspace's shells are not in the session daemon, \
                        so they cannot be left running."
                .into());
        };
        if self.workspaces.len() == 1 {
            // Closing a window detaches what it shows.
            self.close_requested = true;
            return Ok(());
        }
        // Forgotten here without being removed from the daemon.
        self.sessions.remove(&id);
        self.sessions_sent.remove(&session);
        let at = self.workspaces.iter().position(|w| w.id == id).expect("current workspace");
        let workspace = self.workspaces.remove(at);
        for pane in &workspace.panes {
            if let Some(at) = self.tiled.iter().position(|(id, _)| id == pane) {
                let (_, mut pane) = self.tiled.remove(at);
                pane.keep_shell();
                self.closed_panes.push(pane);
            }
        }
        for float in workspace.floats.ids() {
            if let Some(mut float) = self.floating.remove(*float) {
                float.pane.keep_shell();
                self.closed_panes.push(float.pane);
            }
        }
        workspace::renumber(&mut self.workspaces);
        let exists = |id: u64| self.workspaces.iter().any(|w| w.id == id);
        self.current =
            if exists(self.previous) { self.previous } else { self.workspaces[0].id };
        self.previous = self.current;
        self.context.request_repaint();
        Ok(())
    }

    /// End every shell in the current workspace, and its session with them.
    pub(super) fn end_workspace(&mut self) {
        let workspace = self.workspace();
        let floats = workspace.floats.ids().to_vec();
        let panes = workspace.panes.clone();
        self.close_floats(floats);
        for pane in panes {
            let _ = self.close_tiled(pane);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nterm_session::protocol::{SessionInfo, Size};

    fn pane(number: u32, attached: u32, title: Option<&str>) -> PaneInfo {
        PaneInfo {
            pane: number,
            pid: 1,
            creation_time: 1,
            command: r"C:\Windows\system32\cmd.exe".into(),
            size: Size { cols: 80, rows: 24, cell_width: 8, cell_height: 16 },
            exited: None,
            attached,
            title: title.map(str::to_owned),
            pwd: None,
        }
    }

    fn layout(panes: &[u32], floats: &[u32]) -> String {
        let saved = Saved {
            version: VERSION,
            panes: panes.to_vec(),
            floats: floats.iter().map(|pane| Float { pane: *pane, name: None }).collect(),
            ..Saved::default()
        };
        serde_json::to_string(&saved).unwrap()
    }

    fn session(id: &str, layout: String, panes: &[PaneInfo]) -> SessionInfo {
        SessionInfo { id: id.into(), panes: panes.to_vec(), layout }
    }

    fn sort(sessions: Vec<SessionInfo>) -> Vec<Detached> {
        detached(&crate::session::Listing::of(sessions))
    }

    #[test]
    fn waiting_sessions_are_offered_and_shown_ones_are_not() {
        let sessions = sort(vec![
            session("a", layout(&[1, 2], &[]), &[pane(1, 0, Some("build")), pane(2, 0, None)]),
            // Another window is showing it.
            session("b", layout(&[3], &[]), &[pane(3, 1, None)]),
            // Only its float is running, and is tiled.
            session("d", layout(&[7], &[4]), &[pane(4, 0, None)]),
            // In no session.
            session("", String::new(), &[pane(5, 0, None)]),
        ]);
        let ids: Vec<&str> = sessions.iter().map(|session| session.id.as_str()).collect();
        // The shell in no session comes last, without a name.
        assert_eq!(ids, ["a", "d", ""]);
        assert_eq!(sessions[0].saved.panes, [1, 2]);
        assert_eq!(sessions[0].label, "Session: build, cmd.exe");
        assert!(sessions[0].detail.ends_with("/ 2 panes"));
        assert_eq!(sessions[1].saved.panes, [4]);
        assert!(sessions[1].saved.floats.is_empty());
        assert_eq!(sessions[2].saved.panes, [5]);
    }

    #[test]
    fn a_layout_places_only_the_shells_the_daemon_says_are_the_sessions() {
        // The daemon moved pane 2 to the session that named it last. The
        // first one's layout has not caught up.
        let sessions = sort(vec![
            session("a", layout(&[1, 2], &[]), &[pane(1, 0, None)]),
            session("b", layout(&[2], &[]), &[pane(2, 0, None)]),
        ]);
        assert_eq!(sessions.len(), 2);
        assert_eq!((sessions[0].saved.panes.as_slice(), sessions[1].saved.panes.as_slice()), (&[1][..], &[2][..]));
        // A shell of the session that its layout leaves out is not lost.
        let sessions = sort(vec![session("a", layout(&[1], &[]), &[pane(1, 0, None), pane(3, 0, None)])]);
        assert_eq!(sessions[1].id, "");
        assert_eq!(sessions[1].saved.panes, [3]);
    }

    #[test]
    fn a_description_tolerates_a_newer_one_and_refuses_another_version() {
        let panes = [pane(4, 0, None)];
        let newer = format!(r#"{{"version":{VERSION},"later":true,"panes":[4]}}"#);
        assert_eq!(sort(vec![session("a", newer, &panes)])[0].id, "a");
        let other = format!(r#"{{"version":{},"panes":[4]}}"#, VERSION + 1);
        // Not understood, so its shell is offered without a layout.
        assert_eq!(sort(vec![session("a", other, &panes)])[0].id, "");
    }
}
