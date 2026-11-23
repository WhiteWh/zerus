//! What the Teams views follow, which read is due, and the updates a reply
//! gives. Pure and clock-driven, so tests replay real replies without `hgs`.
//!
//! A follow names the team list, the Waiting list and at most one watched
//! team. The watched team's status, inbox and newest feed page are read every
//! [`POLL`], as are the list and Waiting when followed (Qt's 3 s
//! `TeamController` poll); older feed pages, spans and the profile are read on
//! request. A reply becomes an update only when the view it projects differs
//! from the last one sent for that key, so an unchanged team costs a read and
//! nothing more. Keys: `list`, `waiting`, and `team:`, `graph:`, `new:`,
//! `feed:`, `inbox:`, `spans:`, `profile:` followed by the team id.
use super::read::{TeamRead, FEED_PAGE};
use super::view::{
    self, CatalogView, FeedEvent, FeedView, InboxView, NewTeamView, PlanGraph, ProfileView,
    SpansView, TeamList, TeamView, WaitingView,
};
use crate::reads::ReadError;
use serde::Serialize;
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often a followed read runs, as Qt's `TeamController` polls.
pub const POLL: Duration = Duration::from_secs(3);
/// Task 8: with the stream's team heads, a followed read is due when its
/// team's revision moved, and otherwise only this often (what a revision
/// does not show, such as a member's live session).
pub const STREAM_POLL: Duration = Duration::from_secs(60);
/// Task #6 of team c536f0e5 (P1.4, owner 2026-11-23): heads that differ only
/// in revisions (and `updated`) make the list and Waiting due at most this
/// long after their last reply; any other head change makes them due at once.
pub const REVISION_ONLY: Duration = Duration::from_secs(3);
/// Feed events one watched team keeps; Load older stops there.
pub const FEED_KEPT: usize = 500;
/// Task #10: while the window is hidden or minimized only Waiting is read,
/// at the always-on watcher's pace (src-tauri `team_watch::INTERVAL`), so its
/// notices still arrive from this reply and nothing else is read.
pub const HIDDEN_WAITING: Duration = Duration::from_secs(30);
/// On-demand reads that may wait at once (older pages, spans, profile).
const QUEUED: usize = 8;
/// How long a read in flight keeps its slot from running again.
const IN_FLIGHT: Duration = Duration::from_secs(3600);
/// Task #14 (team ede724c4): the teams whose last views are held, so a tab
/// opened again shows its team at once while the fresh read runs.
const HELD_TEAMS: usize = 8;
/// Task #14: the views of a team that are held: its status, plan graph,
/// draft and inbox (the feed has its own paging state and is read again).
const HELD_VIEWS: [&str; 4] = ["team", "graph", "new", "inbox"];

/// What the Teams views follow on one computer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeamsFollow {
    /// The team list (the Teams page, a session's team).
    pub list: bool,
    /// What waits for the owner across every team.
    pub waiting: bool,
    /// The team whose tab, plan or New team form is open.
    pub team: Option<String>,
    /// The team catalog and the designer's draft.
    pub catalog: bool,
}

/// One change of a Teams view, keyed as the module says.
#[derive(Clone, Debug, PartialEq)]
pub enum TeamsUpdate {
    List(Arc<TeamList>),
    Team(Arc<TeamView>),
    Graph(Arc<PlanGraph>),
    NewTeam(Arc<NewTeamView>),
    Feed(Arc<FeedView>),
    Inbox(Arc<InboxView>),
    Waiting(Arc<WaitingView>),
    Spans(Arc<SpansView>),
    Profile(Arc<ProfileView>),
    Catalog(Arc<CatalogView>),
    /// The read failed; the view keeps what it showed.
    Failed {
        code: String,
        message: String,
    },
    /// The view is no longer followed; its data is gone.
    Ended,
}

impl TeamsUpdate {
    /// The batch's `kind`: the view's name, `failed` or `ended`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::List(_) => "list",
            Self::Team(_) => "team",
            Self::Graph(_) => "graph",
            Self::NewTeam(_) => "new_team",
            Self::Feed(_) => "feed",
            Self::Inbox(_) => "inbox",
            Self::Waiting(_) => "waiting",
            Self::Spans(_) => "spans",
            Self::Profile(_) => "profile",
            Self::Catalog(_) => "catalog",
            Self::Failed { .. } => "failed",
            Self::Ended => "ended",
        }
    }

    /// The batch's `payload`: the view model, `{code, message}` or null.
    pub fn payload(&self) -> Value {
        fn value(view: &impl Serialize) -> Value {
            serde_json::to_value(view).unwrap_or(Value::Null)
        }
        match self {
            Self::List(view) => value(&**view),
            Self::Team(view) => value(&**view),
            Self::Graph(view) => value(&**view),
            Self::NewTeam(view) => value(&**view),
            Self::Feed(view) => value(&**view),
            Self::Inbox(view) => value(&**view),
            Self::Waiting(view) => value(&**view),
            Self::Spans(view) => value(&**view),
            Self::Profile(view) => value(&**view),
            Self::Catalog(view) => value(&**view),
            Self::Failed { code, message } => serde_json::json!({"code": code, "message": message}),
            Self::Ended => Value::Null,
        }
    }

    /// Whether it carries a view (not a failure or an end).
    pub fn is_view(&self) -> bool {
        !matches!(self, Self::Failed { .. } | Self::Ended)
    }
}

/// The key of a view about one team.
pub fn team_key(kind: &str, team: &str) -> String {
    format!("{kind}:{team}")
}

/// The views of one team, ended together when it is no longer watched.
const TEAM_VIEWS: [&str; 7] = ["team", "graph", "new", "feed", "inbox", "spans", "profile"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    List,
    Waiting,
    Status,
    Inbox,
    Feed,
    Catalog,
}

#[derive(Debug, Default)]
struct Feed {
    events: BTreeMap<i64, FeedEvent>,
    /// The cursor of the page before the oldest held; None at the start.
    older: Option<i64>,
    capped: bool,
}

/// The Teams read model of one computer.
#[derive(Debug, Default)]
pub struct TeamsModel {
    follow: TeamsFollow,
    due: BTreeMap<Slot, Instant>,
    queued: VecDeque<TeamRead>,
    feed: Feed,
    /// The Waiting pages loaded from the newest; older pages append here.
    waiting: WaitingView,
    /// The digest of the last update sent per key.
    shown: BTreeMap<String, u64>,
    /// Task 8: the team revisions the computer's watch stream gives; None
    /// while it gives none (no `team` topic, a gap, a lost stream): poll.
    heads: Option<std::sync::Arc<super::heads::Heads>>,
    /// Slots whose head moved while their read ran: due again at its reply.
    again: std::collections::BTreeSet<Slot>,
    /// Task #6 (P1.4): slots whose heads moved only in revisions while their
    /// read ran, with the time by which they are read again.
    again_by: BTreeMap<Slot, Instant>,
    /// Task #6 (P1.4): when each slot's last reply came.
    replied: BTreeMap<Slot, Instant>,
    /// Task #10: the window is hidden or minimized: only Waiting is read.
    hidden: bool,
    /// Task #14: the last held views of recently watched teams, newest last,
    /// at most [`HELD_TEAMS`] teams.
    held: VecDeque<(String, BTreeMap<String, TeamsUpdate>)>,
    /// Task #16 of team c536f0e5 (P2.5): the followed reads whose replies the
    /// stream's `team.views` rows bring; they are not polled. Empty while the
    /// stream gives none (no topic, a gap, a lost stream).
    streamed: std::collections::BTreeSet<Slot>,
}

/// An update as the model sends it (task #9): its payload is computed once
/// per reply and serves both the digest and the client's batch.
#[derive(Clone, Debug, PartialEq)]
pub struct Sent {
    pub key: String,
    pub update: TeamsUpdate,
    pub payload: Arc<Value>,
}

impl From<(String, TeamsUpdate)> for Sent {
    fn from((key, update): (String, TeamsUpdate)) -> Self {
        Self::new(key, update)
    }
}

impl Sent {
    /// An update whose payload is computed here (ends and follow changes).
    pub fn new(key: String, update: TeamsUpdate) -> Self {
        let payload = Arc::new(update.payload());
        Self {
            key,
            update,
            payload,
        }
    }
}

/// Feeds the serialized payload straight into the hasher, without a string.
struct HashWriter<'a>(&'a mut DefaultHasher);

impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn digest(kind: &str, payload: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    kind.hash(&mut hasher);
    let _ = serde_json::to_writer(HashWriter(&mut hasher), payload);
    hasher.finish()
}

fn code(error: &ReadError) -> String {
    serde_json::to_value(error)
        .ok()
        .and_then(|value| value["error"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "failed".into())
}

impl TeamsModel {
    pub fn following(&self) -> &TeamsFollow {
        &self.follow
    }

    /// Follows `next` from now on: newly followed reads are due at once, and
    /// views no longer followed end (only the ones that were sent).
    pub fn follow(&mut self, next: TeamsFollow, now: Instant) -> Vec<(String, TeamsUpdate)> {
        let mut out = Vec::new();
        let previous = std::mem::replace(&mut self.follow, next);
        if previous.team != self.follow.team {
            if let Some(old) = &previous.team {
                for kind in TEAM_VIEWS {
                    self.end(&team_key(kind, old), &mut out);
                }
            }
            self.feed = Feed::default();
            self.queued.retain(|read| read.team().is_none());
            for slot in [Slot::Status, Slot::Inbox, Slot::Feed] {
                self.due.remove(&slot);
                // Task #16: the new team's rows have not come yet.
                self.streamed.remove(&slot);
            }
            // Task #14: the tab shows the views held for its team at once;
            // the fresh reads (due now) replace them, and an unchanged one
            // sends nothing.
            if let Some(team) = self.follow.team.clone() {
                if let Some(at) = self.held.iter().position(|(held, _)| *held == team) {
                    let (team, views) = self.held.remove(at).expect("found");
                    for (key, update) in &views {
                        let digest = digest(update.kind(), &update.payload());
                        self.shown.insert(key.clone(), digest);
                        out.push((key.clone(), update.clone()));
                    }
                    self.held.push_back((team, views));
                }
            }
        }
        let wanted = [
            (Slot::List, self.follow.list),
            (Slot::Waiting, self.follow.waiting),
            (Slot::Status, self.follow.team.is_some()),
            (Slot::Inbox, self.follow.team.is_some()),
            (Slot::Feed, self.follow.team.is_some()),
            (Slot::Catalog, self.follow.catalog),
        ];
        for (slot, on) in wanted {
            if on {
                self.due.entry(slot).or_insert(now);
            } else {
                self.due.remove(&slot);
            }
        }
        if !self.follow.list {
            self.end("list", &mut out);
        }
        if !self.follow.waiting {
            self.waiting = WaitingView::default();
            self.end("waiting", &mut out);
        }
        if !self.follow.catalog {
            self.end("catalog", &mut out);
        }
        out
    }

    /// Task #14: keeps a watched team's view to show at once when its tab
    /// opens again; a draft that ended is dropped.
    fn hold(&mut self, key: &str, update: &TeamsUpdate) {
        let Some((kind, team)) = key.split_once(':') else { return };
        if !HELD_VIEWS.contains(&kind) || self.watched() != Some(team) {
            return;
        }
        let at = match self.held.iter().position(|(held, _)| held == team) {
            Some(at) => at,
            None => {
                if self.held.len() >= HELD_TEAMS {
                    self.held.pop_front();
                }
                self.held.push_back((team.to_owned(), BTreeMap::new()));
                self.held.len() - 1
            }
        };
        let views = &mut self.held[at].1;
        if update.is_view() {
            views.insert(key.to_owned(), update.clone());
        } else if matches!(update, TeamsUpdate::Ended) {
            views.remove(key);
        }
    }

    fn end<T: From<(String, TeamsUpdate)>>(&mut self, key: &str, out: &mut Vec<T>) {
        if self.shown.remove(key).is_some() {
            out.push((key.to_owned(), TeamsUpdate::Ended).into());
        }
    }

    fn watched(&self) -> Option<&str> {
        self.follow.team.as_deref()
    }

    fn queue(&mut self, read: TeamRead) -> bool {
        if self.queued.len() >= QUEUED || self.queued.contains(&read) {
            return false;
        }
        self.queued.push_back(read);
        true
    }

    /// Asks for the feed page before the oldest event held. False when there
    /// is none, the feed is at [`FEED_KEPT`], or one is already asked for.
    pub fn request_older(&mut self) -> bool {
        let Some(team) = self.watched().map(str::to_owned) else {
            return false;
        };
        let Some(before) = self.feed.older else {
            return false;
        };
        if self.feed.capped || self.feed.events.len() >= FEED_KEPT || self.feed.events.is_empty() {
            return false;
        }
        self.queue(TeamRead::Events {
            team,
            before: Some(before),
        })
    }

    /// Asks for the next older Waiting page. False when the loaded list has
    /// no cursor or that page is already queued.
    pub fn request_waiting_older(&mut self) -> bool {
        let Some(before) = self.waiting.next.clone() else {
            return false;
        };
        self.queue(TeamRead::Waiting { before: Some(before) })
    }

    /// Asks for the watched team's spans after task `after`.
    pub fn request_spans(&mut self, after: u64) -> bool {
        let Some(team) = self.watched().map(str::to_owned) else {
            return false;
        };
        self.queue(TeamRead::Spans { team, after })
    }

    /// Asks for the watched team's profile.
    pub fn request_profile(&mut self) -> bool {
        let Some(team) = self.watched().map(str::to_owned) else {
            return false;
        };
        self.queue(TeamRead::Profile { team })
    }

    /// Reads every followed view again at once (after an action): each slot
    /// not in flight is due now.
    pub fn refresh(&mut self, now: Instant) {
        for due in self.due.values_mut() {
            if *due <= now + POLL {
                *due = now;
            }
        }
    }

    /// Task #10: the window is hidden or minimized (`true`) or shown again.
    /// Hidden, only Waiting is read, within [`HIDDEN_WAITING`]; shown again,
    /// every followed read is due at once, so the views are current.
    pub fn set_hidden(&mut self, hidden: bool, now: Instant) {
        if self.hidden == hidden {
            return;
        }
        self.hidden = hidden;
        if hidden {
            if let Some(due) = self.due.get_mut(&Slot::Waiting) {
                // A read in flight keeps its slot; its reply sets the pace.
                if *due <= now + STREAM_POLL {
                    *due = (*due).min(now + HIDDEN_WAITING);
                }
            }
        } else {
            // Waiting leaves the hidden pace: due now, unless a read is in flight.
            if let Some(due) = self.due.get_mut(&Slot::Waiting) {
                if *due <= now + STREAM_POLL {
                    *due = now;
                }
            }
            self.refresh(now);
        }
    }

    /// Whether the window is hidden (task #10).
    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// Whether a followed slot may be read now: hidden, only Waiting; never
    /// one the stream's team views bring (task #16).
    fn eligible(&self, slot: Slot) -> bool {
        (!self.hidden || slot == Slot::Waiting) && !self.streamed.contains(&slot)
    }

    /// The next read to run now, if any: an asked-for read first, then the
    /// followed read due longest. Its slot waits until its reply is applied.
    pub fn next(&mut self, now: Instant) -> Option<TeamRead> {
        // Task #14: the watched team's status goes first, ahead of the list,
        // Waiting and asked-for reads: it is what an opened tab waits for.
        if let Some(read) = self.next_status(now) {
            return Some(read);
        }
        if let Some(read) = self.queued.pop_front() {
            return Some(read);
        }
        // Owner, 2026-11-10 ("вход в team очень долгий"): reads run one at a
        // time, so among the due ones the opened team comes first (its status,
        // then inbox and feed), before the list, Waiting and the catalog.
        let rank = |slot: &Slot| match slot {
            Slot::Status => 0,
            Slot::Inbox => 1,
            Slot::Feed => 2,
            Slot::List => 3,
            Slot::Waiting => 4,
            _ => 5,
        };
        let (slot, _) = self
            .due
            .iter()
            .filter(|(slot, due)| **due <= now && self.eligible(**slot))
            .min_by_key(|(slot, due)| (rank(slot), **due))?;
        let slot = *slot;
        let team = self.watched().map(str::to_owned);
        let read = match (slot, team) {
            (Slot::List, _) => TeamRead::List,
            (Slot::Waiting, _) => TeamRead::Waiting { before: None },
            (Slot::Status, Some(team)) => TeamRead::Status { team },
            (Slot::Inbox, Some(team)) => TeamRead::Inbox { team },
            (Slot::Feed, Some(team)) => TeamRead::Events { team, before: None },
            (Slot::Catalog, _) => TeamRead::Catalog,
            // A team slot without a watched team: it cannot be due again.
            _ => {
                self.due.remove(&slot);
                return None;
            }
        };
        self.due.insert(slot, now + IN_FLIGHT);
        Some(read)
    }

    /// Task #14: the watched team's status read when it is due now, else
    /// None. The poller also runs it beside a read still in flight for a team
    /// no longer watched, so a newly opened tab never waits behind that.
    pub fn next_status(&mut self, now: Instant) -> Option<TeamRead> {
        if self.streamed.contains(&Slot::Status) {
            return None;
        }
        let team = self.watched()?.to_owned();
        let due = *self.due.get(&Slot::Status)?;
        if due > now || !self.eligible(Slot::Status) {
            return None;
        }
        self.due.insert(Slot::Status, now + IN_FLIGHT);
        Some(TeamRead::Status { team })
    }

    /// Task 8: the team heads of the computer's stream, or None. A changed
    /// head makes the list and Waiting due at once, and the watched team's
    /// reads when its own revision moved (or it appeared or left). Task #6
    /// (P1.4): heads that moved only in revisions make the list and Waiting
    /// due within [`REVISION_ONLY`] of their last reply instead.
    /// Losing the heads makes every followed read due within [`POLL`] again.
    pub fn heads(&mut self, heads: Option<std::sync::Arc<super::heads::Heads>>, now: Instant) {
        let previous = std::mem::replace(&mut self.heads, heads);
        let mut moved: Vec<Slot> = Vec::new();
        let mut revisions: Vec<Slot> = Vec::new();
        match (&previous, &self.heads) {
            (Some(old), Some(new)) => {
                if super::heads::changed_beyond_revisions(old, new) {
                    moved.extend([Slot::List, Slot::Waiting]);
                } else if old != new {
                    revisions.extend([Slot::List, Slot::Waiting]);
                }
                if let Some(team) = self.watched() {
                    if old.get(team).map(|head| head.revision)
                        != new.get(team).map(|head| head.revision)
                    {
                        moved.extend([Slot::Status, Slot::Inbox, Slot::Feed]);
                    }
                }
            }
            // Still none (a stream's hello before its snapshot): as before.
            (None, None) => {}
            // Heads came or went: everything followed is read now, once.
            _ => moved.extend([
                Slot::List,
                Slot::Waiting,
                Slot::Status,
                Slot::Inbox,
                Slot::Feed,
            ]),
        }
        for slot in moved {
            if let Some(due) = self.due.get_mut(&slot) {
                // A read in flight holds its slot far beyond any interval:
                // it is read again at its reply.
                if *due > now + STREAM_POLL {
                    self.again.insert(slot);
                } else {
                    *due = (*due).min(now);
                }
            }
        }
        for slot in revisions {
            let by = self
                .replied
                .get(&slot)
                .map_or(now, |replied| (*replied + REVISION_ONLY).max(now));
            if let Some(due) = self.due.get_mut(&slot) {
                if *due > now + STREAM_POLL {
                    // In flight: read again by then, or at its reply if later.
                    let deadline = self.again_by.entry(slot).or_insert(now + REVISION_ONLY);
                    *deadline = (*deadline).min(now + REVISION_ONLY);
                } else {
                    *due = (*due).min(by);
                }
            }
        }
    }

    /// Whether the stream's team heads drive the reads (task 8).
    pub fn streamed(&self) -> bool {
        self.heads.is_some()
    }

    /// Task #16 (P2.5): whether Waiting's first page comes from the stream.
    pub fn waiting_streamed(&self) -> bool {
        self.streamed.contains(&Slot::Waiting)
    }

    /// Task #16 of team c536f0e5 (P2.5): rows of the stream's `team.views`
    /// topic, each the reply of one followed read (`list`, `waiting`,
    /// `status:<id>`, `inbox:<id>`, `feed:<id>`), applied exactly as that
    /// read's reply; that read is no longer polled. A row the server could
    /// not send (`{"error":{"code":"too_large"|"failed"}}`) leaves its read
    /// to the poller, due now. None: the stream gives no views any more, so
    /// every followed read is polled again, due now.
    pub fn views(&mut self, rows: Option<&[(String, Value)]>, now: Instant) -> Vec<Sent> {
        let mut out = Vec::new();
        let Some(rows) = rows else {
            for slot in std::mem::take(&mut self.streamed) {
                if let Some(due) = self.due.get_mut(&slot) {
                    *due = (*due).min(now);
                }
            }
            return out;
        };
        for (key, row) in rows {
            let read = match key.split_once(':') {
                None if key == "list" => TeamRead::List,
                None if key == "waiting" => TeamRead::Waiting { before: None },
                Some(("status", team)) => TeamRead::Status { team: team.to_owned() },
                Some(("inbox", team)) => TeamRead::Inbox { team: team.to_owned() },
                Some(("feed", team)) => TeamRead::Events { team: team.to_owned(), before: None },
                _ => continue,
            };
            // A row of a team no longer watched is not this tab's.
            if read.team().is_some_and(|team| Some(team) != self.watched()) {
                continue;
            }
            let Some(slot) = Self::slot(&read) else { continue };
            if row.get("error").is_some_and(|error| error.get("code").is_some()) && row.as_object().is_some_and(|row| row.len() == 1) {
                // Read with its command, now.
                self.streamed.remove(&slot);
                if let Some(due) = self.due.get_mut(&slot) {
                    if *due <= now + STREAM_POLL {
                        *due = now;
                    }
                }
                continue;
            }
            // The row is the read's reply: Waiting's first page, the newest
            // feed page, as the polled reads are.
            out.extend(self.apply_sent(&read, Ok(row.clone()), now));
            self.streamed.insert(slot);
        }
        out
    }

    /// How long until a read is due, at most [`POLL`].
    pub fn wait(&self, now: Instant) -> Duration {
        if !self.queued.is_empty() {
            return Duration::ZERO;
        }
        self.due
            .iter()
            .filter(|(slot, _)| self.eligible(**slot))
            .map(|(_, due)| due.saturating_duration_since(now))
            .min()
            .unwrap_or(POLL)
            .min(POLL)
    }

    fn slot(read: &TeamRead) -> Option<Slot> {
        match read {
            TeamRead::List => Some(Slot::List),
            TeamRead::Waiting { before: None } => Some(Slot::Waiting),
            TeamRead::Status { .. } => Some(Slot::Status),
            TeamRead::Inbox { .. } => Some(Slot::Inbox),
            TeamRead::Events { before: None, .. } => Some(Slot::Feed),
            TeamRead::Catalog => Some(Slot::Catalog),
            _ => None,
        }
    }

    fn key(read: &TeamRead) -> String {
        match read {
            TeamRead::List => "list".into(),
            TeamRead::Waiting { .. } => "waiting".into(),
            TeamRead::Status { team } => team_key("team", team),
            TeamRead::Task { team, task } => team_key(&format!("task:{task}"), team),
            TeamRead::Inbox { team } => team_key("inbox", team),
            TeamRead::Events { team, .. } => team_key("feed", team),
            TeamRead::Spans { team, .. } => team_key("spans", team),
            TeamRead::Profile { team } => team_key("profile", team),
            TeamRead::Catalog => "catalog".into(),
            TeamRead::CatalogRoute { .. } => "route".into(),
            TeamRead::TestQueue { team } => team_key("tests-queue", team),
            TeamRead::TestResults { team, .. } => team_key("tests", team),
            TeamRead::ScopedProfile { team, .. } => team_key("timeline-profile", team),
            TeamRead::ScopedSpans { team, .. } => team_key("timeline-spans", team),
            TeamRead::TestWindow { team, .. } => team_key("timeline-tests", team),
            TeamRead::Builds { team } => team_key("builds", team),
        }
    }

    fn send(&mut self, key: String, update: TeamsUpdate, out: &mut Vec<Sent>) {
        let payload = update.payload();
        let digest = digest(update.kind(), &payload);
        if self.shown.get(&key) != Some(&digest) {
            self.shown.insert(key.clone(), digest);
            self.hold(&key, &update);
            out.push(Sent {
                key,
                update,
                payload: Arc::new(payload),
            });
        }
    }

    /// Applies a read's reply: the slot is due again after [`POLL`], and the
    /// views it changed are returned. A reply about a team no longer watched
    /// gives nothing.
    pub fn apply(
        &mut self,
        read: &TeamRead,
        reply: Result<Value, ReadError>,
        now: Instant,
    ) -> Vec<(String, TeamsUpdate)> {
        self.apply_sent(read, reply, now)
            .into_iter()
            .map(|sent| (sent.key, sent.update))
            .collect()
    }

    /// [`Self::apply`] with each update's payload, computed once (task #9).
    pub fn apply_sent(
        &mut self,
        read: &TeamRead,
        reply: Result<Value, ReadError>,
        now: Instant,
    ) -> Vec<Sent> {
        let mut out = Vec::new();
        // Task #14: a reply about a team no longer watched leaves the slots
        // alone: they belong to the newly watched team now, whose read may
        // not have run yet (its tab waited a whole interval behind it).
        let stale = read.team().is_some_and(|team| Some(team) != self.watched());
        if let Some(slot) = Self::slot(read).filter(|_| !stale) {
            // With the stream's heads a slot waits for its revision to move,
            // and at most STREAM_POLL: the inbox and feed too, whose messages
            // and events are written without a revision bump (task #10,
            // attempt 2). Hidden, Waiting is read at the watcher's pace.
            let every = if self.hidden && slot == Slot::Waiting {
                HIDDEN_WAITING
            } else if self.heads.is_some() {
                STREAM_POLL
            } else {
                POLL
            };
            let again = self.again.remove(&slot);
            let again_by = self.again_by.remove(&slot);
            self.replied.insert(slot, now);
            if let Some(due) = self.due.get_mut(&slot) {
                *due = if again {
                    now
                } else if let Some(by) = again_by {
                    by.max(now).min(now + every)
                } else {
                    now + every
                };
            }
        }
        if stale {
            return out;
        }
        let value = match reply {
            Ok(value) => value,
            Err(error) => {
                let failed = TeamsUpdate::Failed {
                    code: code(&error),
                    message: error.to_string(),
                };
                self.send(Self::key(read), failed, &mut out);
                return out;
            }
        };
        match read {
            TeamRead::List => self.send(
                "list".into(),
                TeamsUpdate::List(Arc::new(view::list(&value))),
                &mut out,
            ),
            TeamRead::Waiting { before } => {
                let page = view::waiting(&value);
                if before.is_none() {
                    self.waiting = page;
                } else {
                    let seen: std::collections::HashSet<String> =
                        self.waiting.items.iter().map(|item| item.id.clone()).collect();
                    self.waiting
                        .items
                        .extend(page.items.into_iter().filter(|item| !seen.contains(&item.id)));
                    self.waiting.next = page.next;
                    self.waiting.total = page.total;
                    self.waiting.more = page.more;
                }
                self.send(
                    "waiting".into(),
                    TeamsUpdate::Waiting(Arc::new(self.waiting.clone())),
                    &mut out,
                );
            }
            TeamRead::Status { team } => {
                let shown = view::team(&value);
                let graph = view::graph(&value, &shown);
                let new_team = view::new_team(&value, &shown);
                self.send(
                    team_key("team", team),
                    TeamsUpdate::Team(Arc::new(shown)),
                    &mut out,
                );
                self.send(
                    team_key("graph", team),
                    TeamsUpdate::Graph(Arc::new(graph)),
                    &mut out,
                );
                match new_team {
                    Some(new_team) => self.send(
                        team_key("new", team),
                        TeamsUpdate::NewTeam(Arc::new(new_team)),
                        &mut out,
                    ),
                    // Created or discarded: the form's draft view ends.
                    None => {
                        self.hold(&team_key("new", team), &TeamsUpdate::Ended);
                        self.end(&team_key("new", team), &mut out)
                    }
                }
            }
            TeamRead::Inbox { team } => self.send(
                team_key("inbox", team),
                TeamsUpdate::Inbox(Arc::new(view::inbox(team, &value))),
                &mut out,
            ),
            TeamRead::Events { team, before } => {
                let (events, cursor) = view::feed_page(&value);
                match before {
                    None => self.head(events, cursor),
                    Some(_) => self.older(events, cursor),
                }
                let feed = self.feed_view(team);
                self.send(
                    team_key("feed", team),
                    TeamsUpdate::Feed(Arc::new(feed)),
                    &mut out,
                );
            }
            TeamRead::Spans { team, .. } => self.send(
                team_key("spans", team),
                TeamsUpdate::Spans(Arc::new(view::spans(team, &value))),
                &mut out,
            ),
            TeamRead::Profile { team } => self.send(
                team_key("profile", team),
                TeamsUpdate::Profile(Arc::new(view::profile(team, &value))),
                &mut out,
            ),
            TeamRead::Catalog => self.send(
                "catalog".into(),
                TeamsUpdate::Catalog(Arc::new(view::catalog(&value))),
                &mut out,
            ),
            // Asked once by the editor's Try it, never polled.
            TeamRead::CatalogRoute { .. } | TeamRead::Task { .. } => {}
            // Test pages are read on request by the Tests mode, not polled.
            TeamRead::TestQueue { .. } | TeamRead::TestResults { .. } => {}
            // Task 92: the Time view reads these on request, never polled.
            TeamRead::ScopedProfile { .. }
            | TeamRead::ScopedSpans { .. }
            | TeamRead::TestWindow { .. } => {}
            // Task 199: the Tests tab's Builds are read on request too.
            TeamRead::Builds { .. } => {}
        }
        out
    }

    /// The newest page. A full page that does not reach the newest event held
    /// left a gap (more than a page arrived since): the feed starts over from
    /// it. Event numbers are the store's, shared by every team, so only an
    /// overlap proves the pages meet.
    fn head(&mut self, events: Vec<FeedEvent>, cursor: Option<i64>) {
        let newest = self.feed.events.keys().next_back().copied();
        let oldest = events.iter().map(|event| event.seq).min();
        let gap = match (newest, oldest) {
            (Some(newest), Some(oldest)) => {
                events.len() as u64 >= FEED_PAGE && oldest > newest && cursor.is_some()
            }
            _ => false,
        };
        if gap {
            self.feed = Feed::default();
        }
        if self.feed.events.is_empty() {
            self.feed.older = cursor;
        }
        for event in events {
            self.feed.events.insert(event.seq, event);
        }
        self.trim();
    }

    fn older(&mut self, events: Vec<FeedEvent>, cursor: Option<i64>) {
        for event in events {
            self.feed.events.insert(event.seq, event);
        }
        self.feed.older = cursor;
        self.trim();
    }

    /// Keeps the newest [`FEED_KEPT`] events; once older ones went, Load older
    /// stops.
    fn trim(&mut self) {
        while self.feed.events.len() > FEED_KEPT {
            self.feed.events.pop_first();
            self.feed.capped = true;
        }
    }

    fn feed_view(&self, team: &str) -> FeedView {
        FeedView {
            team: team.to_owned(),
            events: self.feed.events.values().rev().cloned().collect(),
            more: self.feed.older.is_some() && !self.feed.capped,
            capped: self.feed.capped,
        }
    }

    /// Events the feed holds (for tests and diagnostics).
    pub fn feed_len(&self) -> usize {
        self.feed.events.len()
    }
}
