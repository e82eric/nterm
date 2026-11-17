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
use nterm_session::protocol::{PaneInfo, SessionInfo};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

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
    /// What the user called the workspace.
    name: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Float {
    pane: u32,
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

/// The shells of a session that can be taken: all of them, when nothing
/// shows any. A session is taken whole, so one a window shows part of is
/// not offered. The shells in no session, which the daemon gives under no
/// name, are not one thing, and those of them that wait are offered.
fn takeable(session: &SessionInfo) -> Vec<&PaneInfo> {
    let waiting: Vec<&PaneInfo> =
        session.panes.iter().filter(|pane| crate::session::waiting(pane)).collect();
    if session.id.is_empty() || waiting.len() == session.panes.len() {
        waiting
    } else {
        Vec::new()
    }
}

/// What the user called a session's workspace. It is kept in the layout,
/// which is all this reads of it: a layout this build cannot lay out may
/// still say the name. The shells in no session have none.
fn session_name(session: &SessionInfo) -> Option<String> {
    #[derive(Deserialize)]
    struct Named {
        name: Option<String>,
    }
    if session.id.is_empty() {
        return None;
    }
    let named: Named = serde_json::from_str(&session.layout).ok()?;
    named.name.as_deref().and_then(workspace::named)
}

/// What the rows `session_rows` gives are of: which sessions there are to
/// take, what the user called each, and which shells each has. Rows are
/// given to an open picker again when this changes. It leaves out what a
/// shell is called and where it is, which change on their own: those are
/// shown as they were when the rows were last given, so that a picker's
/// selection is not taken back to its first row for a new title.
pub(super) fn session_rows_key(listing: &crate::session::Listing) -> String {
    let key = |session: &SessionInfo| {
        let panes: Vec<u32> = takeable(session).iter().map(|pane| pane.pane).collect();
        (!panes.is_empty()).then(|| format!("{:?}{:?}{panes:?}", session.id, session_name(session)))
    };
    listing.sessions.iter().filter_map(key).collect()
}

/// How many panes a row says there are: the tiled ones, then each floating
/// one, as `3 + 1 + 1`.
pub(super) fn counted(tiled: usize, floats: usize) -> String {
    let counts = std::iter::once(tiled).chain(std::iter::repeat_n(1, floats));
    counts.map(|count| count.to_string()).collect::<Vec<_>>().join(" + ")
}

/// What a row calls a workspace or a session, and what it says runs there.
/// One the user named is called by the name, and what its shells call
/// themselves is said beside where the first of them is. One without a
/// name is called by what its shells call themselves.
pub(super) fn called(
    name: Option<String>,
    titles: &[String],
    place: Option<&String>,
) -> (String, String) {
    let short = |title: &String| {
        // A path says less than the program it ends in.
        let short = title.rsplit(['\\', '/']).next().filter(|short| !short.is_empty());
        let short = short.unwrap_or(title);
        short.chars().filter(|c| !c.is_control()).collect::<String>()
    };
    // A shell that has not said what it is called says nothing here.
    let titles: Vec<&String> = titles.iter().filter(|title| !title.trim().is_empty()).collect();
    let mut names: Vec<String> = titles.iter().take(3).map(|title| short(title)).collect();
    if titles.len() > names.len() {
        names.push(format!("+{}", titles.len() - names.len()));
    }
    let names = names.join(", ");
    let place = place.map(|pwd| self::place(pwd)).filter(|place| !place.is_empty());
    let (name, running) = match name {
        Some(name) => (name, [Some(names), place]),
        None => (names, [None, place]),
    };
    (name, running.into_iter().flatten().collect::<Vec<_>>().join(" / "))
}

/// A picker's rows for the sessions the daemon holds that nothing is
/// showing. Their cells are those of a workspace's row after its number,
/// which a session has none of: its name or what runs in it, that it is
/// detached, its layout, how many panes, and what runs and where.
pub(super) fn session_rows(listing: &crate::session::Listing) -> Vec<crate::actions::Entry> {
    let row = |session: &SessionInfo| {
        let panes = takeable(session);
        let saved = arrangement(session)?;
        let titles: Vec<String> = panes
            .iter()
            .map(|pane| {
                let title = pane.title.as_deref().filter(|title| !title.trim().is_empty());
                title.unwrap_or(&pane.command).to_owned()
            })
            .collect();
        let place = panes.iter().find_map(|pane| pane.pwd.clone());
        let (name, running) = called(session_name(session), &titles, place.as_ref());
        let kind = if session.id.is_empty() { "Shells in no session: " } else { "" };
        Some(crate::actions::Entry {
            id: format!("session.{}", session.id),
            label: String::new(),
            shortcut: None,
            cells: vec![
                format!("{kind}{name}"),
                "detached".into(),
                saved.layout.tag().to_owned(),
                counted(saved.panes.len(), saved.floats.len()),
                running,
            ],
            action: AppAction::AttachSession(session.id.clone()),
        })
    };
    listing.sessions.iter().filter_map(row).collect()
}

/// The directory in what a shell reported, which is a URL such as
/// `kitty-shell-cwd://host/C:/src`: the path alone, and a drive without the
/// slash before it.
fn place(pwd: &str) -> String {
    let path = match pwd.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("", |at| &rest[at..]),
        None => pwd,
    };
    let drive = path.strip_prefix('/').filter(|rest| rest.as_bytes().get(1) == Some(&b':'));
    drive.unwrap_or(path).to_owned()
}

/// How a session being taken is laid out: as its window left it, cut down
/// to the shells that are there to take. Shells its layout does not place,
/// which is all of them when it cannot be read, are tiled after the rest.
/// `None` when the session cannot be taken.
fn arrangement(session: &SessionInfo) -> Option<Saved> {
    let panes: Vec<u32> = takeable(session).iter().map(|pane| pane.pane).collect();
    if panes.is_empty() {
        return None;
    }
    let read = serde_json::from_str::<Saved>(&session.layout).ok();
    let mut saved = read.filter(|saved| saved.version == VERSION).unwrap_or_default();
    saved.version = VERSION;
    saved.keep_only(|pane| panes.contains(&pane));
    let placed = saved.members();
    saved.panes.extend(panes.iter().filter(|pane| !placed.contains(pane)));
    Some(saved)
}

/// A waiting session, for drawing it small.
pub(super) struct Waiting {
    /// Its tiled panes as it lays them out. Their ids are the daemon's pane
    /// numbers.
    pub(super) workspace: workspace::Workspace,
    /// The shells to look at: the tiled ones, and the floating one that
    /// was showing.
    pub(super) shells: Vec<crate::session::Adopt>,
    /// The floating pane that was showing, by the daemon's pane number.
    pub(super) float: Option<u32>,
}

/// A session waiting in the daemon, as the daemon last described it. `None`
/// when it is not waiting.
pub(super) fn waiting_workspace(id: &str) -> Option<Waiting> {
    let listing = crate::session::sessions()?;
    let saved = listing.sessions.iter().find(|session| session.id == id).and_then(arrangement)?;
    let mut shown = workspace::Workspace::new(0, 0);
    shown.layout = saved.layout;
    if let Some(ratio) = saved.main_ratio.filter(|ratio| ratio.is_finite()) {
        shown.main_ratio = ratio.clamp(0.1, 0.9);
    }
    shown.panes = saved.panes.iter().map(|pane| u64::from(*pane)).collect();
    shown.pane_weights = saved
        .weights
        .iter()
        .map(|(layout, weights)| {
            let weights = weights.iter().map(|(pane, weight)| (u64::from(*pane), *weight));
            (*layout, weights.collect())
        })
        .collect();
    shown.appearance = saved.appearance.clone().filter(|appearance| appearance.validate().is_ok());
    let focused = saved.focused.filter(|pane| saved.panes.contains(pane));
    if let Some(pane) = focused.or(saved.panes.first().copied()) {
        shown.focus(u64::from(pane));
    }
    let float = saved.visible_float.filter(|pane| saved.floats.iter().any(|f| f.pane == *pane));
    let shells = saved.panes.iter().chain(&float).filter_map(|pane| listing.adopt(*pane)).collect();
    Some(Waiting { workspace: shown, shells, float })
}

impl TerminalView {
    /// Show a waiting session as a new workspace of this window, and go to
    /// it. In a window that had no terminal it is the only workspace.
    pub(super) fn attach_session(&mut self, id: &str, gl: &glow::Context) -> Result<(), String> {
        // Nothing looks at the session once it is shown.
        self.drop_peek();
        // The sessions as they are now, which the rows chosen from followed.
        let listing = crate::session::sessions().ok_or("That session is no longer waiting.")?;
        let session = listing.sessions.iter().find(|session| session.id == id);
        let saved = session.and_then(arrangement).ok_or("That session is no longer waiting.")?;
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
            let id = self.floating.insert(FloatingTerminal { pane });
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
        workspace.name = saved.name.as_deref().and_then(workspace::named);
        match visible {
            Some(id) => workspace.floats.show(id),
            None => workspace.floats.hide(),
        }
        if let Some(id) = focused.or(workspace.panes.first().copied()) {
            workspace.focus(id);
        }
        if !id.is_empty() {
            let session = workspace::Session { id: id.to_owned(), sent: String::new() };
            workspace.session = Some(session);
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
            self.floating.get(*id)?.value.pane.session_pane()
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
                .map(|pane| Float { pane })
                .collect(),
            visible_float: workspace.floats.visible().and_then(|id| float(&id)),
            name: workspace.name.clone(),
            panes,
        })
    }

    /// Keep the daemon's copy of each workspace up to date. Called every
    /// frame; the daemon hears only of changes. A workspace that goes is
    /// not taken from the daemon: when its shells end, or another
    /// workspace takes them, the daemon forgets it, and one that was
    /// detached is meant to stay.
    pub(super) fn save_sessions(&mut self) {
        for at in 0..self.workspaces.len() {
            // `None` while the daemon has numbered none of its panes.
            let Some(saved) = self.describe(&self.workspaces[at]) else { continue };
            let Ok(text) = serde_json::to_string(&saved) else { continue };
            let session = self.workspaces[at]
                .session
                .get_or_insert_with(|| workspace::Session { id: new_id(), sent: String::new() });
            if session.sent != text {
                crate::session::set_session(session.id.clone(), saved.members(), text.clone());
                session.sent = text;
            }
        }
    }

    /// Take a workspace out of this window and leave its session
    /// waiting in the daemon. The last workspace takes the window with it.
    pub(super) fn detach_workspace(&mut self, id: u64) -> Result<(), String> {
        self.save_sessions();
        let Some(workspace) = self.workspaces.iter().find(|w| w.id == id) else {
            return Err("workspace does not exist".into());
        };
        let kept = |pane: Option<&super::TerminalPane>| {
            pane.is_some_and(|pane| pane.session_pane().is_some())
        };
        let all_kept = workspace.panes.iter().all(|pane| kept(self.tiled_ref(*pane)))
            && workspace
                .floats
                .ids()
                .iter()
                .all(|float| kept(self.floating.get(*float).map(|entry| &entry.value.pane)));
        // One the daemon has not been told of could not be shown again.
        let told = workspace.session.as_ref().is_some_and(|session| !session.sent.is_empty());
        if !(told && all_kept) {
            return Err("This workspace's shells are not in the session daemon, \
                        so they cannot be left running."
                .into());
        }
        if self.workspaces.len() == 1 {
            // Closing a window detaches what it shows.
            self.close_requested = true;
            return Ok(());
        }
        // Gone from here and left in the daemon.
        let at = self.workspaces.iter().position(|w| w.id == id).expect("the workspace");
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
        if self.current == id {
            self.current =
                if exists(self.previous) { self.previous } else { self.workspaces[0].id };
        }
        if !exists(self.previous) {
            self.previous = self.current;
        }
        self.context.request_repaint();
        Ok(())
    }

    /// End every shell in a workspace, and its session with them. A window
    /// this leaves without a terminal asks what to show next.
    pub(super) fn end_workspace(&mut self, id: u64) {
        self.stay_open = true;
        let Some(workspace) = self.workspaces.iter().find(|w| w.id == id) else { return };
        let floats = workspace.floats.ids().to_vec();
        let panes = workspace.panes.clone();
        self.close_floats(floats);
        for pane in panes {
            let _ = self.close_tiled(pane);
        }
    }

    /// End every shell of a session nothing shows. The daemon forgets a
    /// session once none of its shells runs, and says so, which takes its
    /// row out of the picker.
    pub(super) fn end_session(&self, id: &str) {
        let Some(listing) = crate::session::sessions() else { return };
        let Some(session) = listing.sessions.iter().find(|session| session.id == id) else {
            return;
        };
        for pane in takeable(session) {
            crate::session::close_pane(pane.pane);
        }
    }

    /// A window whose last terminal has just gone stays when sessions wait
    /// in the daemon, or when a workspace was closed by its command: it
    /// asks what to show next, as a window just opened does. Otherwise it
    /// closes, as `should_close` then says.
    pub(super) fn stay_for_sessions(&mut self) {
        let stay = std::mem::take(&mut self.stay_open);
        if self.empty.is_some() || self.close_requested || self.has_panes() {
            return;
        }
        if crate::session::status() != crate::session::Status::Connected {
            return;
        }
        let waiting = crate::session::sessions()
            .is_some_and(|listing| !session_rows(&listing).is_empty());
        if !(stay || waiting) {
            return;
        }
        // The workspace kept for the window to stand on is no session's
        // any more, and its floats go as those of any emptied workspace.
        let floats: Vec<u64> =
            self.workspaces.iter().flat_map(|w| w.floats.ids().to_vec()).collect();
        self.close_floats(floats);
        for workspace in &mut self.workspaces {
            workspace.session = None;
            workspace.name = None;
        }
        self.close_window_picker();
        self.key_table = None;
        self.empty = Some(super::empty::Empty::Connecting);
        self.context.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nterm_session::protocol::{SessionInfo, Size};

    fn pane(number: u32, attached: bool, title: Option<&str>) -> PaneInfo {
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
            floats: floats.iter().map(|pane| Float { pane: *pane }).collect(),
            ..Saved::default()
        };
        serde_json::to_string(&saved).unwrap()
    }

    fn session(id: &str, layout: String, panes: &[PaneInfo]) -> SessionInfo {
        SessionInfo { id: id.into(), panes: panes.to_vec(), layout }
    }

    fn labels(sessions: Vec<SessionInfo>) -> Vec<String> {
        let rows = session_rows(&crate::session::Listing::of(sessions));
        rows.into_iter().map(|row| row.cells[0].clone()).collect()
    }

    #[test]
    fn a_reported_directory_is_shown_as_a_path() {
        assert_eq!(place("kitty-shell-cwd://localhost/C:/src/nterm"), "C:/src/nterm");
        // A new pane is started in the path, as the shell would spell it.
        let path = crate::session::url_path;
        assert_eq!(path("file://host/C:/my%20src/caf%C3%A9"), "/C:/my src/café");
        assert_eq!(path("C:\\src\\100%"), "C:\\src\\100%");
        assert_eq!(place("file://host/home/me"), "/home/me");
        assert_eq!(place("C:/src"), "C:/src");
        assert_eq!(place("file://host"), "");
    }

    #[test]
    fn waiting_sessions_are_offered_and_shown_ones_are_not() {
        let sessions = vec![
            session("a", layout(&[1, 2], &[]), &[pane(1, false, Some("build")), pane(2, false, None)]),
            // Another window is showing it.
            session("b", layout(&[3], &[]), &[pane(3, true, None)]),
            // A window shows one of its shells: it is taken whole or not.
            session("c", layout(&[6, 7], &[]), &[pane(6, true, None), pane(7, false, None)]),
            // In no session: the one that waits is offered.
            session("", String::new(), &[pane(5, false, Some("stray")), pane(8, true, None)]),
        ];
        let rows = session_rows(&crate::session::Listing::of(sessions));
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["session.a", "session."]);
        assert_eq!(rows[0].cells[..2], ["build, cmd.exe", "detached"]);
        assert_eq!(rows[0].cells[3..], ["2", ""]);
        assert_eq!(rows[1].cells[0], "Shells in no session: stray");
    }

    #[test]
    fn a_session_is_listed_whatever_its_layout_says() {
        // Listing reads no layout: one this build cannot read is a session
        // all the same.
        let other = format!(r#"{{"version":{},"panes":[4]}}"#, VERSION + 1);
        assert_eq!(labels(vec![session("a", other, &[pane(4, false, None)])]), ["cmd.exe"]);
        assert_eq!(labels(vec![session("a", "nonsense".into(), &[pane(4, false, None)])]).len(), 1);
    }

    #[test]
    fn a_named_session_is_called_by_its_name_and_says_what_runs_beside_it() {
        let named = format!(r#"{{"version":{VERSION},"panes":[1],"name":"build"}}"#);
        let listing = crate::session::Listing::of(vec![session("a", named, &[pane(1, false, None)])]);
        let rows = session_rows(&listing);
        // Its name, that nothing shows it, its layout, one tiled pane, and
        // what runs there.
        assert_eq!(rows[0].cells, ["build", "detached", "T", "1", "cmd.exe"]);
        // A name is read from a layout this build cannot lay out.
        let other = format!(r#"{{"version":{},"name":"later"}}"#, VERSION + 1);
        let listing = crate::session::Listing::of(vec![session("a", other, &[pane(1, false, None)])]);
        assert_eq!(session_rows(&listing)[0].cells[0], "later");
        // What is typed is trimmed and kept to a line; nothing is no name.
        assert_eq!(workspace::named("  api\tserver\n "), Some("apiserver".into()));
        assert_eq!(workspace::named("   "), None);
    }

    #[test]
    fn a_row_says_what_runs_where_and_how_many_panes() {
        let titles = [r"C:\tools\cargo.exe".to_owned(), String::new(), "vim".to_owned()];
        let place = "file://host/C:/src".to_owned();
        // Without a name it is called by what runs, and one with no title
        // is passed over.
        assert_eq!(called(None, &titles, Some(&place)), ("cargo.exe, vim".into(), "C:/src".into()));
        let named = called(Some("api".into()), &titles, Some(&place));
        assert_eq!(named, ("api".into(), "cargo.exe, vim / C:/src".into()));
        assert_eq!(called(None, &[], None), (String::new(), String::new()));
        // Three tiled and two floating.
        assert_eq!((counted(3, 2), counted(1, 0)), ("3 + 1 + 1".into(), "1".into()));
    }

    #[test]
    fn rows_are_of_the_sessions_and_their_shells_not_of_what_the_shells_say() {
        let key = |sessions| session_rows_key(&crate::session::Listing::of(sessions));
        let named = |name: &str| format!(r#"{{"version":{VERSION},"panes":[1],"name":"{name}"}}"#);
        let was = key(vec![session("a", named("build"), &[pane(1, false, Some("idle"))])]);
        // A shell's new title, or directory, is not a reason for new rows.
        let mut moved = pane(1, false, Some("compiling"));
        moved.pwd = Some("file://host/C:/src".into());
        assert_eq!(key(vec![session("a", named("build"), &[moved])]), was);
        // Another name, another shell, or another session is.
        assert_ne!(key(vec![session("a", named("test"), &[pane(1, false, None)])]), was);
        let two = [pane(1, false, None), pane(2, false, None)];
        assert_ne!(key(vec![session("a", named("build"), &two)]), was);
        let other = session("b", String::new(), &[pane(3, false, None)]);
        let both = vec![session("a", named("build"), &[pane(1, false, None)]), other];
        assert_ne!(key(both), was);
        // One a window has taken is no row, and neither is its key.
        assert_eq!(key(vec![session("a", named("build"), &[pane(1, true, None)])]), "");
    }

    #[test]
    fn a_session_is_taken_as_it_was_laid_out_with_every_shell_it_has() {
        let panes = [pane(1, false, None), pane(2, false, None), pane(3, false, None)];
        // Its float is still a float; a shell the layout leaves out is tiled.
        let saved = arrangement(&session("a", layout(&[2], &[1]), &panes)).unwrap();
        assert_eq!(saved.panes, [2, 3]);
        assert_eq!(saved.floats.len(), 1);
        // Shells that have ended are gone from it; a float left alone is tiled.
        let saved = arrangement(&session("a", layout(&[9], &[1]), &panes[..1])).unwrap();
        assert_eq!(saved.panes, [1]);
        assert!(saved.floats.is_empty());
        // A newer description is read; another version is not, and its
        // shells are tiled in the daemon's order.
        let newer = format!(r#"{{"version":{VERSION},"later":true,"panes":[2,1]}}"#);
        assert_eq!(arrangement(&session("a", newer, &panes[..2])).unwrap().panes, [2, 1]);
        let other = format!(r#"{{"version":{},"panes":[2,1]}}"#, VERSION + 1);
        assert_eq!(arrangement(&session("a", other, &panes[..2])).unwrap().panes, [1, 2]);
        // One a window is showing cannot be taken.
        assert!(arrangement(&session("a", layout(&[1], &[]), &[pane(1, true, None)])).is_none());
    }
}
