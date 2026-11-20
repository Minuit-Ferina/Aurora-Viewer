//! Bounded desktop-file staging and session-owned inventory transactions.
mod files;
mod native;
mod view;

use super::*;
use crate::{
    creation_permissions::Defaults,
    world::inventory::{Inventory, actions as rules},
};
use aurora_net::inventory::{
    operations::NewItem,
    text_import::{Content, Import, Outcome},
};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::mpsc,
};
use uuid::Uuid;

const MAX_UNFINISHED: usize = 100;
const MAX_PAYLOAD: usize = 32 * 1024 * 1024;
const MAX_READERS: usize = 2;
const MAX_VERIFICATIONS: usize = 4;
const BUSY_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Note,
    Script,
}
impl Kind {
    fn item(self) -> NewItem {
        match self {
            Self::Note => NewItem::Note,
            Self::Script => NewItem::Script,
        }
    }
    fn allowed(self, inv: &Inventory) -> bool {
        inv.can_preview(match self {
            Self::Note => 7,
            Self::Script => 10,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Error {
    InvalidName,
    Extension,
    Read,
    TooLarge,
    Utf8,
    Nul,
    Marker,
    Payload,
    Destination,
    Restricted,
    Unavailable,
    Upload,
    Unknown,
    Cancelled,
}

enum State {
    Waiting(PathBuf),
    Reading,
    Ready(Content),
    Creating,
    Uploading,
    Saved {
        script: Option<aurora_net::scripts::ScriptSaved>,
        verified: Option<bool>,
    },
    Failed(Error),
}

impl State {
    fn pending(&self) -> bool {
        matches!(self, Self::Waiting(_) | Self::Reading | Self::Ready(_))
    }

    fn unfinished(&self) -> bool {
        self.pending() || matches!(self, Self::Creating | Self::Uploading)
    }
}

struct Row {
    id: Uuid,
    filename: String,
    name: String,
    kind: Option<Kind>,
    created: Option<Uuid>,
    state: State,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Session {
    // Viewer generation only, independent of the simulator's credentials.
    generation: Uuid,
    owner: Uuid,
    root: Uuid,
}

#[derive(Clone)]
struct Destination {
    session: Session,
    folder: Uuid,
    defaults: Defaults,
}

struct Batch {
    id: Uuid,
    to: Destination,
    rows: Vec<Row>,
}

struct Active {
    row: Uuid,
    request: Uuid,
    session: Session,
    folder: Uuid,
    bytes: usize,
    cancel_if_busy: bool,
}

struct Verification {
    row: Uuid,
    session: Session,
    asset: Uuid,
    saved_item: Option<aurora_net::inventory::InvItem>,
}

struct DropFiles {
    id: Uuid,
    session: Session,
    paths: Vec<PathBuf>,
}

type ReadResult = mpsc::Receiver<Result<Content, Error>>;
type PickerResult = mpsc::Receiver<Option<Vec<PathBuf>>>;

enum DemoIntake {
    Choose,
    RemoveFolder,
}

pub(super) struct Controller {
    generation: Uuid,
    batches: Vec<Batch>,
    reads: Vec<(Uuid, ReadResult)>,
    active: Option<Active>,
    verifications: HashMap<Uuid, Verification>,
    retry_after: Option<Instant>,
    native: native::DropState,
    awaiting_folder: VecDeque<DropFiles>,
    picker: Option<(Destination, PickerResult)>,
    demo_started: bool,
    demo_intake: Option<DemoIntake>,
    exiting: bool,
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            generation: Uuid::new_v4(),
            batches: Vec::new(),
            reads: Vec::new(),
            active: None,
            verifications: HashMap::new(),
            retry_after: None,
            native: Default::default(),
            awaiting_folder: VecDeque::new(),
            picker: None,
            demo_started: false,
            demo_intake: None,
            exiting: false,
        }
    }
}

impl Controller {
    pub(super) fn clear_session(&mut self) {
        // A cancelled worker still counts against the limit until it returns.
        let reads = std::mem::take(&mut self.reads);
        *self = Self::default();
        self.reads = reads;
    }

    fn rows(&self) -> impl Iterator<Item = &Row> {
        self.batches.iter().flat_map(|batch| &batch.rows)
    }

    fn row_mut(&mut self, id: Uuid) -> Option<&mut Row> {
        self.batches.iter_mut().flat_map(|batch| &mut batch.rows).find(|row| row.id == id)
    }

    fn unfinished(&self) -> usize {
        self.rows().filter(|row| row.state.unfinished()).count() + self.awaiting_folder.iter().map(|drop| drop.paths.len()).sum::<usize>()
    }

    fn payload(&self) -> usize {
        self.rows()
            .filter_map(|row| match &row.state {
                State::Ready(content) => Some(content.len()),
                _ => None,
            })
            .sum::<usize>()
            + self.active.as_ref().map_or(0, |active| active.bytes)
    }

    fn await_folder(&mut self, session: Session, paths: Vec<PathBuf>) -> bool {
        if paths.len() > MAX_UNFINISHED.saturating_sub(self.unfinished()) {
            return false;
        }
        if !paths.is_empty() {
            self.awaiting_folder.push_back(DropFiles {
                id: Uuid::new_v4(),
                session,
                paths,
            });
        }
        true
    }

    fn enqueue(&mut self, id: Uuid, to: Destination, paths: Vec<PathBuf>) -> bool {
        if paths.is_empty() || self.batches.iter().any(|batch| batch.id == id) {
            return true;
        }
        if paths.len() > MAX_UNFINISHED.saturating_sub(self.unfinished()) {
            return false;
        }
        let rows = paths.into_iter().map(files::row).collect();
        self.batches.push(Batch { id, to, rows });
        true
    }

    fn poll_reads(&mut self, jobs: &crate::scene::jobs::Jobs) {
        let mut index = 0;
        while index < self.reads.len() {
            let (id, rx) = &self.reads[index];
            let result = match rx.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Disconnected) => Err(Error::Read),
                Err(mpsc::TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
            };
            let id = *id;
            self.reads.swap_remove(index);
            let retained = self.payload();
            if let Some(row) = self.row_mut(id).filter(|row| matches!(row.state, State::Reading)) {
                row.state = match result {
                    Ok(content) if retained + content.len() <= MAX_PAYLOAD => State::Ready(content),
                    Ok(_) => State::Failed(Error::Payload),
                    Err(reason) => State::Failed(reason),
                };
            }
        }

        for row in self.batches.iter_mut().flat_map(|batch| &mut batch.rows) {
            if self.reads.len() >= MAX_READERS {
                break;
            }
            if let (State::Waiting(_), Some(kind)) = (&row.state, row.kind)
                && let State::Waiting(path) = std::mem::replace(&mut row.state, State::Reading)
            {
                let (tx, rx) = mpsc::channel();
                jobs.execute(move || {
                    let _ = tx.send(files::read(&path, kind));
                });
                self.reads.push((row.id, rx));
            }
        }
    }

    fn cancel(&mut self, batch: Uuid) {
        if let Some(batch) = self.batches.iter_mut().find(|b| b.id == batch) {
            for row in &mut batch.rows {
                if row.state.pending() {
                    row.state = State::Failed(Error::Cancelled);
                } else if let Some(active) = self.active.as_mut().filter(|active| active.row == row.id) {
                    // The command can be refused as Busy before any packet is sent.
                    active.cancel_if_busy = true;
                }
            }
        }
    }

    fn next(&mut self, inv: &Inventory, session: Session, visible: bool, ready: bool, now: Instant) -> Option<Import> {
        if self.active.is_some() || self.verifications.len() >= MAX_VERIFICATIONS || self.retry_after.is_some_and(|time| time > now) {
            return None;
        }
        for batch in &mut self.batches {
            let folder = batch.to.folder;
            let allowed = batch.to.session == session && rules::text_import_destination(inv, folder);
            for row in batch.rows.iter_mut().filter(|row| row.state.pending()) {
                if !allowed {
                    row.state = State::Failed(Error::Destination);
                } else if !visible || row.kind.is_some_and(|kind| !kind.allowed(inv)) {
                    row.state = State::Failed(Error::Restricted);
                }
            }
            if !allowed || !ready {
                continue;
            }
            let Some(row) = batch.rows.iter_mut().find(|row| matches!(row.state, State::Ready(_))) else {
                continue;
            };
            let Some(permissions) = row.kind.and_then(|kind| batch.to.defaults.for_item(kind.item())) else {
                continue;
            };
            let State::Ready(content) = std::mem::replace(&mut row.state, State::Creating) else {
                continue;
            };
            let request = Uuid::new_v4();
            self.active = Some(Active {
                row: row.id,
                request,
                session,
                folder,
                bytes: content.len(),
                cancel_if_busy: false,
            });
            return Some(Import {
                request,
                owner: session.owner,
                root: session.root,
                folder,
                name: row.name.clone(),
                content,
                permissions,
            });
        }
        None
    }

    fn reply(
        &mut self,
        inv: &Inventory,
        session: Session,
        request: Uuid,
        outcome: Outcome,
        now: Instant,
    ) -> Option<aurora_net::inventory::InvItem> {
        if let Outcome::Verification {
            asset,
            verified_item,
            permissions_verified,
        } = outcome
        {
            let check = self
                .verifications
                .get(&request)
                .filter(|check| check.session == session && check.asset == asset)?;
            let row_id = check.row;
            let saved_item = self.verifications.remove(&request)?.saved_item;
            let row = self.row_mut(row_id)?;
            let State::Saved { verified, .. } = &mut row.state else {
                return None;
            };
            let confirmed = verified_item.filter(|item| Some(item.id) == row.created && item.asset_id == asset);
            *verified = Some(confirmed.is_some() && permissions_verified);
            // A new edit or inventory update may have landed while the check was in flight.
            // Only reconcile over the version we published when the upload was acknowledged.
            return confirmed.filter(|item| saved_item.as_ref() == inv.items.get(&item.id) && saved_item.is_some());
        }
        let active = self
            .active
            .as_ref()
            .filter(|active| active.request == request && active.session == session)?;
        let cancel_if_busy = active.cancel_if_busy;
        let folder = active.folder;
        let row = self.row_mut(active.row)?;
        if let Outcome::Created(item) = outcome {
            if matches!(row.state, State::Creating) {
                row.created = Some(item.id);
                row.name.clone_from(&item.name);
                row.state = State::Uploading;
                return Some(item);
            }
            return None;
        }
        let mut verification = None;
        let busy = matches!(outcome, Outcome::Busy(_)) && matches!(row.state, State::Creating);
        row.state = match outcome {
            Outcome::Busy(_) if busy && cancel_if_busy => State::Failed(Error::Cancelled),
            Outcome::Busy(content) if busy => State::Ready(content),
            Outcome::Unavailable => State::Failed(Error::Unavailable),
            Outcome::UploadFailed => State::Failed(Error::Upload),
            Outcome::Saved { script, asset } if matches!(row.state, State::Uploading) => {
                let saved_item = row
                    .created
                    .and_then(|id| inv.items.get(&id))
                    .filter(|item| item.owner == session.owner && item.parent == folder)
                    .cloned()
                    .map(|mut item| {
                        item.asset_id = asset;
                        item
                    });
                verification = Some(Verification {
                    row: row.id,
                    session,
                    asset,
                    saved_item,
                });
                State::Saved { script, verified: None }
            }
            _ => State::Failed(Error::Unknown),
        };
        self.active = None;
        self.retry_after = (busy && !cancel_if_busy).then_some(now + BUSY_DELAY);
        verification.and_then(|verification| {
            let item = verification.saved_item.clone();
            self.verifications.insert(request, verification);
            item
        })
    }
}

impl Controller {
    pub(super) fn stop_accepting(&mut self) {
        self.exiting = true;
        self.native = Default::default();
        self.awaiting_folder.clear();
        self.picker = None;
        for batch in &mut self.batches {
            for row in batch.rows.iter_mut().filter(|row| row.state.pending()) {
                row.state = State::Failed(Error::Cancelled);
            }
        }
    }
}

impl App {
    fn text_import_session(&self) -> Session {
        Session {
            generation: self.desktop_text_import.generation,
            owner: self.world.agent_id,
            root: self.world.inventory.root,
        }
    }

    fn text_import_destination(&self, folder: Uuid) -> Option<Destination> {
        let inv = &self.world.inventory;
        let session = self.text_import_session();
        (!self.desktop_text_import.exiting
            && matches!(self.screen, Screen::World)
            && !session.owner.is_nil()
            && inv.owner == session.owner
            && !self.world.rlva.has("showinv")
            && rules::text_import_destination(inv, folder))
        .then_some(Destination {
            session,
            folder,
            defaults: self.settings.inventory.creation_permissions,
        })
    }

    pub(super) fn pick_desktop_text(&mut self, folder: Uuid) {
        if self.desktop_text_import.picker.is_some() {
            return;
        }
        let Some(to) = self.text_import_destination(folder) else {
            self.inventory_ui.message = aurora_i18n::tr!("desktop-text-import-error-destination").into();
            return;
        };
        let (tx, rx) = mpsc::channel();
        let title = aurora_i18n::tr!("desktop-text-import-title");
        let filter = aurora_i18n::tr!("desktop-text-import-filter");
        let ctx = self.egui_ctx.clone();
        if std::thread::Builder::new()
            .name("aurora-text-import-picker".into())
            .spawn(move || {
                let paths = rfd::FileDialog::new()
                    .set_title(title)
                    .add_filter(
                        filter,
                        &[
                            "txt", "txT", "tXt", "tXT", "Txt", "TxT", "TXt", "TXT", "lsl", "lsL", "lSl", "lSL", "Lsl", "LsL", "LSl", "LSL",
                        ],
                    )
                    .pick_files();
                let _ = tx.send(paths);
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.desktop_text_import.picker = Some((to, rx));
        } else {
            self.inventory_ui.message = aurora_i18n::tr!("desktop-text-import-error-unavailable").into();
        }
    }

    pub(super) fn publish_desktop_text_status(&self, ctx: &egui::Context) {
        let visible = !self.desktop_text_import.exiting && matches!(self.screen, Screen::World) && !self.world.rlva.has("showinv");
        let feedback = if visible {
            view::feedback(&self.world.inventory, &self.desktop_text_import)
        } else {
            Default::default()
        };
        ui::inventory::text_import::publish(ctx, feedback);
    }

    pub(super) fn desktop_text_reply(&mut self, request: Uuid, outcome: Outcome) {
        let session = self.text_import_session();
        let refresh = matches!(outcome, Outcome::Unknown)
            .then_some(self.desktop_text_import.active.as_ref())
            .flatten()
            .filter(|active| active.session == session && active.request == request)
            .map(|active| active.folder);
        if let Some(item) = self
            .desktop_text_import
            .reply(&self.world.inventory, session, request, outcome, Instant::now())
        {
            self.world.inventory.add_items(vec![item]);
        }
        if let Some(folder) = refresh.filter(|folder| rules::text_import_destination(&self.world.inventory, *folder))
            && !self.demo
            && !self.desktop_text_import.exiting
            && self.world.inventory_writable()
        {
            // The protocol has no client item UUID to resolve after a create
            // timeout. Refresh the actual folder without guessing by name.
            self.send(NetCommand::FetchInventory {
                folders: vec![folder],
                owner: session.owner,
                library: false,
            });
        }
    }

    pub(super) fn tick_desktop_text_import(&mut self) {
        let now = Instant::now();
        let session = self.text_import_session();
        let visible = !self.world.rlva.has("showinv");
        let ready =
            !self.desktop_text_import.exiting && matches!(self.screen, Screen::World) && (self.demo || self.world.inventory_writable());
        let ctl = &mut self.desktop_text_import;
        if ctl.native.expire(now) && visible {
            self.inventory_ui.message = aurora_i18n::tr!("desktop-text-import-error-transfer").into();
        }
        if !visible {
            ctl.native = Default::default();
            ctl.awaiting_folder.clear();
        }
        if let Some((to, rx)) = ctl.picker.as_ref() {
            match rx.try_recv() {
                Ok(paths) => {
                    let to = to.clone();
                    ctl.picker = None;
                    if to.session == session
                        && let Some(paths) = paths
                    {
                        let accepted = match ctl.demo_intake.take() {
                            Some(DemoIntake::Choose) => ctl.await_folder(session, paths),
                            Some(DemoIntake::RemoveFolder) => {
                                let folder = to.folder;
                                let accepted = ctl.enqueue(Uuid::new_v4(), to, paths);
                                self.world.inventory.folders.remove(&folder);
                                self.world.inventory.generation += 1;
                                accepted
                            }
                            None => ctl.enqueue(Uuid::new_v4(), to, paths),
                        };
                        if !accepted {
                            self.inventory_ui.message = aurora_i18n::tr!("desktop-text-import-error-full").into();
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => ctl.picker = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        ctl.poll_reads(&self.scene.jobs);
        if let Some(import) = ctl.next(&self.world.inventory, session, visible, ready, now) {
            if self.demo {
                self.demo_desktop_text(import);
            } else {
                self.send(NetCommand::ImportDesktopText(import));
            }
        }
        self.seed_desktop_text_demo();
    }

    pub(super) fn choose_desktop_text(&mut self, id: Uuid, folder: Uuid) {
        let Some(to) = self.text_import_destination(folder) else {
            self.inventory_ui.message = aurora_i18n::tr!("desktop-text-import-error-destination").into();
            return;
        };
        if self
            .desktop_text_import
            .awaiting_folder
            .front()
            .is_some_and(|drop| drop.id == id && drop.session == to.session)
            && let Some(drop) = self.desktop_text_import.awaiting_folder.pop_front()
            && !self.desktop_text_import.enqueue(id, to, drop.paths)
        {
            self.inventory_ui.message = aurora_i18n::tr!("desktop-text-import-error-full").into();
        }
    }

    pub(super) fn discard_desktop_text(&mut self, id: Uuid) {
        if self.desktop_text_import.awaiting_folder.front().is_some_and(|drop| drop.id == id) {
            self.desktop_text_import.awaiting_folder.pop_front();
        }
    }

    pub(super) fn cancel_desktop_text(&mut self, id: Uuid) {
        self.desktop_text_import.cancel(id);
    }

    pub(super) fn dismiss_desktop_text(&mut self, id: Uuid) {
        self.desktop_text_import
            .batches
            .retain(|b| b.id != id || b.rows.iter().any(|row| row.state.unfinished()));
    }

    fn seed_desktop_text_demo(&mut self) {
        if !self.demo || self.desktop_text_import.demo_started || !matches!(self.screen, Screen::World) {
            return;
        }
        self.desktop_text_import.demo_started = true;
        let Ok(mode) = std::env::var("AURORA_DEMO_DESKTOP_TEXT_IMPORT") else {
            return;
        };
        self.settings.classic_inventory = mode.starts_with("legacy-");
        let mode = mode.strip_prefix("legacy-").unwrap_or(&mode).to_owned();
        let inv = &mut self.world.inventory;
        let folder = if mode == "destination-changed" {
            let id = Uuid::new_v4();
            inv.folders.insert(
                id,
                crate::world::inventory::Folder {
                    info: aurora_net::inventory::InvFolder {
                        id,
                        parent: inv.root,
                        type_default: -1,
                        name: "Text import demo".into(),
                        ..Default::default()
                    },
                    children: Vec::new(),
                    items: Vec::new(),
                    state: crate::world::inventory::FetchState::Fetched,
                    library: false,
                },
            );
            self.desktop_text_import.demo_intake = Some(DemoIntake::RemoveFolder);
            id
        } else {
            if mode == "choose" {
                self.desktop_text_import.demo_intake = Some(DemoIntake::Choose);
            }
            rules::system(inv, 7).unwrap_or(inv.root)
        };
        let Some(to) = self.text_import_destination(folder) else { return };
        let Ok(dir) = std::env::current_dir() else { return };
        let dir = dir.join(".scratch");
        if !dir.is_dir() {
            return;
        }
        let dir = dir.join(format!("text-import-{}", Uuid::new_v4()));
        let (tx, rx) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        self.scene.jobs.execute(move || {
            let result = (|| -> std::io::Result<_> {
                std::fs::create_dir(&dir)?;
                let fixtures = [
                    ("Welcome.txt", "Linden text version 2\r\n\tCafé, 世界 🌏\r\n"),
                    ("Door.lsl", "default { state_entry() { } }\n"),
                ];
                let mut paths = Vec::new();
                for (name, text) in fixtures {
                    let path = dir.join(name);
                    std::fs::write(&path, text)?;
                    paths.push(path);
                }
                if mode == "ten" {
                    for index in 1..=4 {
                        for (name, text) in fixtures {
                            let path = dir.join(format!("{index}-{name}"));
                            std::fs::write(&path, text)?;
                            paths.push(path);
                        }
                    }
                }
                if mode == "mixed" || mode == "invalid" {
                    let path = dir.join("Invalid.txt");
                    std::fs::write(&path, [0xff])?;
                    paths.push(path);
                    paths.push(dir.join("Unsupported.pdf"));
                }
                if mode == "duplicates" {
                    let sub = dir.join("another");
                    std::fs::create_dir(&sub)?;
                    let path = sub.join("Welcome.txt");
                    std::fs::write(&path, "Second notecard")?;
                    paths.push(path);
                }
                Ok(paths)
            })();
            let _ = tx.send(result.ok());
            ctx.request_repaint();
        });
        self.desktop_text_import.picker = Some((to, rx));
        ui::widgets::open_floater(&self.egui_ctx, "inventory", &mut self.panels.inventory);
    }

    fn demo_desktop_text(&mut self, import: Import) {
        let mode = std::env::var("AURORA_DEMO_DESKTOP_TEXT_IMPORT").unwrap_or_default();
        let mode = mode.strip_prefix("legacy-").unwrap_or(&mode);
        if mode == "unknown-outcome" {
            self.desktop_text_reply(import.request, Outcome::Unknown);
            return;
        }
        let kind = match &import.content {
            Content::Notecard(_) => 7,
            Content::Script(_) => 10,
        };
        let mut item = aurora_net::inventory::InvItem {
            id: Uuid::new_v4(),
            parent: import.folder,
            name: import.name,
            asset_type: kind,
            inv_type: kind,
            owner: import.owner,
            creator: import.owner,
            base_mask: 0x7fff_ffff,
            owner_mask: 0x7fff_ffff,
            next_owner_mask: import.permissions.next_owner,
            group_mask: import.permissions.group,
            everyone_mask: import.permissions.everyone,
            ..Default::default()
        };
        self.desktop_text_reply(import.request, Outcome::Created(item.clone()));
        if self
            .desktop_text_import
            .batches
            .last()
            .is_some_and(|batch| batch.rows.iter().filter(|row| row.created.is_some()).count() == 1)
        {
            self.inventory_ui.show_original(&self.world.inventory, item.id);
        }
        if mode == "cancel"
            && let Some(id) = self.desktop_text_import.batches.last().map(|batch| batch.id)
        {
            self.desktop_text_import.cancel(id);
        }
        if mode == "upload-error" {
            self.desktop_text_reply(import.request, Outcome::UploadFailed);
            return;
        }
        item.asset_id = Uuid::new_v4();
        let script = match import.content {
            Content::Notecard(data) => {
                self.demo_documents.insert(item.id, data);
                None
            }
            Content::Script(source) => {
                self.demo_script_sources.insert(
                    aurora_net::scripts::ScriptTarget {
                        object: Uuid::nil(),
                        item: item.id,
                    },
                    source,
                );
                let compiled = mode != "compile-error";
                Some(aurora_net::scripts::ScriptSaved {
                    asset: item.asset_id,
                    compiled,
                    checked: true,
                    errors: if compiled { Vec::new() } else { vec!["(1, 8) Syntax error".into()] },
                })
            }
        };
        self.desktop_text_reply(
            import.request,
            Outcome::Saved {
                asset: item.asset_id,
                script,
            },
        );
        let verified = mode != "verification-failed";
        self.desktop_text_reply(
            import.request,
            Outcome::Verification {
                asset: item.asset_id,
                verified_item: verified.then_some(item),
                permissions_verified: verified,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::inventory::{FetchState, Folder};
    use aurora_net::inventory::{InvFolder, InvItem};

    fn folder(inv: &mut Inventory, id: u128, parent: u128, kind: i32, library: bool) -> Uuid {
        let id = Uuid::from_u128(id);
        inv.folders.insert(
            id,
            Folder {
                info: InvFolder {
                    id,
                    parent: Uuid::from_u128(parent),
                    type_default: kind,
                    name: format!("Folder {id}"),
                    ..Default::default()
                },
                children: Vec::new(),
                items: Vec::new(),
                state: FetchState::Fetched,
                library,
            },
        );
        id
    }

    fn fixture() -> (Controller, Inventory, Destination) {
        let controller = Controller::default();
        let mut inv = Inventory {
            owner: Uuid::from_u128(1),
            root: Uuid::from_u128(2),
            ..Default::default()
        };
        folder(&mut inv, 2, 0, 8, false);
        let target = folder(&mut inv, 3, 2, -1, false);
        let to = Destination {
            session: Session {
                generation: controller.generation,
                owner: inv.owner,
                root: inv.root,
            },
            folder: target,
            defaults: Defaults::default(),
        };
        (controller, inv, to)
    }

    fn stage(ctl: &mut Controller, to: &Destination) -> Uuid {
        let batch = Uuid::new_v4();
        ctl.enqueue(
            batch,
            to.clone(),
            vec!["a/Door.lsl".into(), "b/Door.lsl".into(), "wrong.pdf".into()],
        );
        for row in &mut ctl.batches.last_mut().expect("batch").rows[..2] {
            row.state = State::Ready(Content::Script(String::new()));
        }
        batch
    }

    fn created(req: &Import, id: u128) -> InvItem {
        InvItem {
            id: Uuid::from_u128(id),
            parent: req.folder,
            owner: req.owner,
            asset_type: 10,
            inv_type: 10,
            name: req.name.clone(),
            ..Default::default()
        }
    }

    #[test]
    fn desktop_drop_waits_for_explicit_selection_and_later_gestures_are_independent() {
        let (mut ctl, inv, to) = fixture();
        let now = Instant::now();
        assert!(ctl.await_folder(to.session, vec!["a/Door.lsl".into(), "b/Door.lsl".into()]));
        assert!(ctl.await_folder(to.session, vec!["a/Door.lsl".into()]));
        assert_eq!(ctl.awaiting_folder[0].paths.len(), 2);
        assert_eq!(ctl.awaiting_folder[1].paths.len(), 1);
        assert!(ctl.next(&inv, to.session, true, true, now).is_none());
        let dropped = ctl.awaiting_folder.pop_front().expect("first drop");
        ctl.enqueue(dropped.id, to.clone(), dropped.paths);
        ctl.enqueue(dropped.id, to, vec!["a/Door.lsl".into()]);
        assert_eq!(ctl.batches.len(), 1);
        assert_eq!(ctl.batches[0].rows.len(), 2);
    }

    #[test]
    fn creates_distinct_items_and_accepts_replies_only_for_the_active_request_and_session() {
        let (mut ctl, mut inv, mut to) = fixture();
        to.defaults.scripts.share_group = true;
        to.defaults.scripts.everyone_copy = true;
        stage(&mut ctl, &to);
        let now = Instant::now();
        let a = ctl.next(&inv, to.session, true, true, now).expect("first create");
        assert_eq!(a.folder, to.folder);
        assert_eq!(a.permissions, to.defaults.scripts.permissions());
        assert!(ctl.next(&inv, to.session, true, true, now).is_none());
        let a_item = created(&a, 5);
        assert!(
            ctl.reply(&inv, to.session, Uuid::nil(), Outcome::Created(a_item.clone()), now)
                .is_none()
        );
        assert!(
            ctl.reply(
                &inv,
                Session {
                    generation: Uuid::nil(),
                    ..to.session
                },
                a.request,
                Outcome::Created(a_item.clone()),
                now
            )
            .is_none()
        );
        assert_eq!(
            ctl.reply(&inv, to.session, a.request, Outcome::Created(a_item.clone()), now),
            Some(a_item.clone())
        );
        assert!(
            ctl.reply(&inv, to.session, a.request, Outcome::Created(created(&a, 13)), now)
                .is_none()
        );
        inv.add_items(vec![a_item.clone()]);
        let saved_item = ctl.reply(
            &inv,
            to.session,
            a.request,
            Outcome::Saved {
                asset: Uuid::from_u128(8),
                script: Some(aurora_net::scripts::ScriptSaved {
                    asset: Uuid::from_u128(8),
                    compiled: true,
                    checked: true,
                    errors: Vec::new(),
                }),
            },
            now,
        );
        let saved_item = saved_item.expect("upload acknowledgement updates the asset immediately");
        assert_eq!(saved_item.id, a_item.id);
        assert_eq!(saved_item.asset_id, Uuid::from_u128(8));
        inv.add_items(vec![saved_item]);
        let row = &ctl.batches[0].rows[0];
        assert!(matches!(row.state, State::Saved { verified: None, .. }));
        assert_eq!(view::row_status(row).label, aurora_i18n::tr!("desktop-text-import-compiled"));
        assert!(view::row_status(row).detail.is_empty());
        let b = ctl.next(&inv, to.session, true, true, now).expect("second create");
        let verification = Outcome::Verification {
            asset: Uuid::from_u128(8),
            verified_item: None,
            permissions_verified: false,
        };
        let previous = Session {
            generation: Uuid::nil(),
            ..to.session
        };
        assert!(ctl.reply(&inv, previous, a.request, verification.clone(), now).is_none());
        assert!(matches!(ctl.batches[0].rows[0].state, State::Saved { verified: None, .. }));
        assert!(ctl.reply(&inv, to.session, a.request, verification, now).is_none());
        let row = &ctl.batches[0].rows[0];
        assert!(matches!(row.state, State::Saved { verified: Some(false), .. }));
        let status = view::row_status(row);
        assert_eq!(status.label, aurora_i18n::tr!("desktop-text-import-compiled"));
        assert_eq!(status.detail, aurora_i18n::tr!("desktop-text-import-unverified"));
        assert_eq!(a.name, b.name);
        let b_item = created(&b, 6);
        assert_ne!(a_item.id, b_item.id);
        assert!(ctl.reply(&inv, to.session, a.request, Outcome::Created(a_item), now).is_none());
        assert_eq!(
            ctl.reply(&inv, to.session, b.request, Outcome::Created(b_item.clone()), now),
            Some(b_item.clone())
        );
        ctl.reply(&inv, to.session, b.request, Outcome::UploadFailed, now);
        assert_eq!(ctl.batches[0].rows[1].created, Some(b_item.id));
        assert!(matches!(ctl.batches[0].rows[1].state, State::Failed(Error::Upload)));
        assert!(matches!(ctl.batches[0].rows[2].state, State::Failed(Error::Extension)));
        assert!(ctl.next(&inv, to.session, true, true, now).is_none());
    }

    #[test]
    fn cancel_preserves_in_flight_result_and_unknown_creates_are_not_replayed() {
        let (mut ctl, inv, to) = fixture();
        let id = stage(&mut ctl, &to);
        let now = Instant::now();
        assert!(ctl.next(&inv, to.session, true, false, now).is_none());
        let request = ctl.next(&inv, to.session, true, true, now).expect("create");
        ctl.cancel(id);
        assert!(matches!(ctl.batches[0].rows[0].state, State::Creating));
        assert!(matches!(ctl.batches[0].rows[1].state, State::Failed(Error::Cancelled)));
        ctl.reply(&inv, to.session, request.request, Outcome::Unknown, now);
        assert!(matches!(ctl.batches[0].rows[0].state, State::Failed(Error::Unknown)));
        assert!(ctl.next(&inv, to.session, true, true, now + BUSY_DELAY).is_none());
    }

    #[test]
    fn capacity_backpressure_retries_only_pre_send_and_revalidates_the_captured_folder() {
        let (mut ctl, mut inv, to) = fixture();
        stage(&mut ctl, &to);
        let now = Instant::now();
        let request = ctl.next(&inv, to.session, true, true, now).expect("first attempt");
        ctl.reply(&inv, to.session, request.request, Outcome::Busy(request.content), now);
        assert!(ctl.next(&inv, to.session, true, true, now).is_none());
        inv.folders.remove(&to.folder);
        assert!(ctl.next(&inv, to.session, true, true, now + BUSY_DELAY).is_none());
        assert!(matches!(ctl.batches[0].rows[0].state, State::Failed(Error::Destination)));
        assert!(matches!(ctl.batches[0].rows[1].state, State::Failed(Error::Destination)));
    }

    #[test]
    fn applies_folder_and_document_restrictions_and_rejects_an_overlarge_batch_as_a_unit() {
        let (mut ctl, mut inv, to) = fixture();
        for kind in [14, 47, 48, 46, 50] {
            let invalid = folder(&mut inv, kind as u128, 2, kind, false);
            inv.generation += 1;
            assert!(!rules::text_import_destination(&inv, invalid), "folder kind {kind}");
        }
        let library = folder(&mut inv, 16, 0, 8, true);
        assert!(!rules::text_import_destination(&inv, library));
        ctl.enqueue(Uuid::new_v4(), to.clone(), vec!["item.txt".into(); MAX_UNFINISHED + 1]);
        assert!(ctl.batches.is_empty());
        stage(&mut ctl, &to);
        inv.rlva_preview_mask = 4;
        assert!(ctl.next(&inv, to.session, true, true, Instant::now()).is_none());
        assert!(matches!(ctl.batches[0].rows[0].state, State::Failed(Error::Restricted)));
    }
}
