//! Materials use CreateInventoryItem followed by UpdateMaterialAgentInventory,
//! as LLMaterialEditor::createInventoryItem (Firestorm, originally LGPL 2.1).
//! The callback owns its request; unrelated inventory creations cannot finish it.

use super::*;
use crate::inventory::{
    InvItem,
    operations::{Mutation, Operation},
    text_import::{Content, Import, Outcome, SaveError},
    upload::{Error, Kind, Receipt, Rejection, Request},
};

const MAX_DOCUMENT_CREATES: usize = 8;

#[derive(Default)]
pub(super) struct Imports {
    creation_permissions: HashMap<u32, CreationPermissions>,
    creates: HashMap<u32, Pending>,
    /// New environment settings items waiting for their inventory callback.
    settings: HashMap<u32, SettingsCreate>,
    desktop_text: HashMap<u32, TextCreate>,
    meshes: Arc<Mutex<HashMap<Uuid, MeshOffer>>>,
    pub balance: Option<i32>,
}

struct TextCreate {
    started: Instant,
    import: Import,
    region: std::net::SocketAddr,
    upload_cap: String,
    fetch_cap: Option<String>,
}

impl Imports {
    fn documents_full(&self) -> bool {
        self.settings.len() + self.desktop_text.len() >= MAX_DOCUMENT_CREATES
    }
}

struct CreationPermissions {
    started: Instant,
    parent: Uuid,
    asset_type: i32,
    rights: crate::inventory::upload::Permissions,
}

impl CreationPermissions {
    fn update(&self, item: &InvItem, owner: Uuid) -> Option<update_inventory_item::InventoryData> {
        if item.id.is_nil() || item.parent != self.parent || item.asset_type != self.asset_type || item.owner != owner {
            return None;
        }
        let update = permission_update(item, self.rights, 0);
        ((item.next_owner_mask, item.group_mask, item.everyone_mask) != (update.next_owner_mask, update.group_mask, update.everyone_mask))
            .then_some(update)
    }
}
struct MeshOffer {
    started: Instant,
    upload: crate::inventory::mesh_upload::Request,
    offer: Option<crate::inventory::mesh_upload::Offer>,
}

impl MeshOffer {
    fn expired(&self) -> bool {
        let seconds = if self.offer.is_some() {
            300
        } else {
            u64::from(self.upload.timeout_secs) + 40
        };
        self.started.elapsed() > Duration::from_secs(seconds)
    }

    fn quoted(&mut self, offer: crate::inventory::mesh_upload::Offer) {
        // A slow fee transfer must not consume the user's confirmation time.
        self.started = Instant::now();
        self.offer = Some(offer);
    }
}
/// LLSettingsVOBase::createInventoryItem: the item first, its asset once the
/// callback names it (onInventoryItemCreated → updateInventoryItem).
struct SettingsCreate {
    started: Instant,
    request: Uuid,
    folder: Uuid,
    data: Vec<u8>,
    /// UpdateSettingsAgentInventory and FetchInventory2.
    caps: (String, String),
    asset_type: i32,
}

fn material_save_error(reason: String) -> Error {
    let rejection = Rejection::from_message(&reason);
    log::warn!("material inventory save did not confirm: {rejection:?}");
    Error::Uncertain(rejection)
}

/// PERM_COPY (llpermissionsflags.h).
const PERM_COPY: u32 = 0x8000;
/// LLFloaterPerms::getNextOwnerPerms("Settings") with Firestorm's defaults:
/// PERM_MOVE, plus SettingsNextOwnerModify and SettingsNextOwnerTransfer
/// (on); SettingsNextOwnerCopy is not defined, so it reads as off.
const SETTINGS_NEXT_OWNER: u32 = 0x0008_0000 | 0x4000 | 0x2000;
/// PERM_MOVE | PERM_TRANSFER, LLScriptRecoverQueue::recoverNext's next
/// owner rights for a recovered script.
const SCRIPT_RECOVERY_NEXT_OWNER: u32 = 0x0008_0000 | 0x2000;

struct Pending {
    started: Instant,
    request: Uuid,
    upload: Request,
    caps: (String, String, String, String),
    legacy_asset: Option<Uuid>,
    permissions_applied: bool,
}

/// LLViewerInventoryItem::updateServer(false), including the legacy checksum.
pub(super) fn permission_update(
    item: &InvItem,
    rights: crate::inventory::upload::Permissions,
    callback: u32,
) -> update_inventory_item::InventoryData {
    let uuid_sum = |id: Uuid| {
        id.as_bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .fold(0u32, |sum, b| sum.wrapping_add(u32::from_le_bytes([b[0], b[1], b[2], b[3]])))
    };
    let group = rights.group & item.owner_mask;
    let everyone = rights.everyone & item.owner_mask;
    let crc = [
        item.id,
        item.parent,
        item.creator,
        item.owner,
        item.last_owner,
        item.group_id,
        item.asset_id,
        item.thumbnail,
    ]
    .into_iter()
    .map(uuid_sum)
    .chain([
        item.base_mask,
        item.owner_mask,
        group,
        everyone,
        item.asset_type as u32,
        item.inv_type as u32,
        item.flags,
        item.sale_price as u32,
        u32::from(item.sale_type).wrapping_mul(0x07073096),
        item.created_at as u32,
    ])
    .fold(0u32, u32::wrapping_add);
    update_inventory_item::InventoryData {
        item_id: item.id,
        folder_id: item.parent,
        callback_id: callback,
        creator_id: item.creator,
        owner_id: item.owner,
        group_id: item.group_id,
        base_mask: item.base_mask,
        owner_mask: item.owner_mask,
        group_mask: group,
        everyone_mask: everyone,
        next_owner_mask: rights.next_owner & item.owner_mask,
        group_owned: item.group_owned,
        transaction_id: Uuid::nil(),
        type_: item.asset_type as i8,
        inv_type: item.inv_type as i8,
        flags: item.flags,
        sale_type: item.sale_type,
        sale_price: item.sale_price,
        name: str_field(&item.name),
        description: str_field(&item.desc),
        creation_date: item.created_at as i32,
        crc,
    }
}

impl Session<'_> {
    /// LLInventoryCallback creation callbacks apply group/everyone rights only
    /// after the server has named the new, owned inventory item.
    pub(super) fn remember_creation_permissions(
        &mut self,
        callback: u32,
        parent: Uuid,
        asset_type: i32,
        rights: crate::inventory::upload::Permissions,
    ) {
        self.asset_imports.creation_permissions.insert(
            callback,
            CreationPermissions {
                started: Instant::now(),
                parent,
                asset_type,
                rights,
            },
        );
    }

    pub(super) fn legacy_asset_ready(&mut self, request: Uuid, upload: Request, transaction: Uuid, asset: Uuid) {
        if upload.expected_cost > 0 && self.asset_imports.balance.is_none_or(|balance| balance < upload.expected_cost) {
            emit(
                self.sh,
                NetEvent::AssetUploaded {
                    request,
                    result: Err(Error::Rejected),
                },
            );
            return;
        }
        // upload_done_callback: the charge precedes inventory creation and
        // carries the price the user confirmed, so the simulator can reject
        // a discrepancy. The transfer itself has already completed.
        let mut charge = MoneyTransferRequest::default();
        charge.agent_data.agent_id = self.agent_id();
        charge.agent_data.session_id = self.session_id();
        charge.money_data.source_id = self.agent_id();
        charge.money_data.amount = upload.expected_cost;
        charge.money_data.transaction_type = 1101;
        self.send_main(&charge, true);
        if let Some(balance) = &mut self.asset_imports.balance {
            *balance -= upload.expected_cost;
        }
        let callback = self.inventory_callback(false);
        let mut message = CreateInventoryItem::default();
        message.agent_data.agent_id = self.agent_id();
        message.agent_data.session_id = self.session_id();
        message.inventory_block.folder_id = upload.folder;
        message.inventory_block.callback_id = callback;
        message.inventory_block.transaction_id = transaction;
        message.inventory_block.next_owner_mask = upload.permissions.next_owner;
        message.inventory_block.type_ = upload.kind.asset_type() as i8;
        message.inventory_block.inv_type = match upload.kind {
            Kind::Texture => 0,
            Kind::Sound => 1,
            Kind::Animation => 19,
            // IT_SNAPSHOT
            Kind::Snapshot => 15,
            _ => return,
        };
        message.inventory_block.wearable_type = 255;
        message.inventory_block.name = str_field(&upload.name);
        message.inventory_block.description = str_field(&upload.description);
        self.asset_imports.creates.insert(
            callback,
            Pending {
                started: Instant::now(),
                request,
                upload,
                caps: Default::default(),
                legacy_asset: Some(asset),
                permissions_applied: false,
            },
        );
        self.send_main(&message, true);
    }
    pub(super) fn quote_mesh(&mut self, request: Uuid, upload: crate::inventory::mesh_upload::Request) {
        let caps = self.main_cap("NewFileAgentInventory").zip(self.main_cap("MeshUploadFlag"));
        let Some((cap, permission)) = caps else {
            emit(
                self.sh,
                NetEvent::MeshQuoted {
                    request,
                    result: Err(Error::Unavailable),
                },
            );
            return;
        };
        if upload.validate().is_err() || self.asset_imports.meshes.lock().len() >= 8 {
            emit(
                self.sh,
                NetEvent::MeshQuoted {
                    request,
                    result: Err(Error::Invalid),
                },
            );
            return;
        }
        let token = Uuid::new_v4();
        self.asset_imports.meshes.lock().insert(
            token,
            MeshOffer {
                started: Instant::now(),
                upload: upload.clone(),
                offer: None,
            },
        );
        let offers = self.asset_imports.meshes.clone();
        let http = self.sh.caps_http.clone();
        let events = self.sh.events.clone();
        tokio::spawn(async move {
            let result = crate::inventory::mesh_upload::quote(&http, &cap, &permission, &upload).await;
            let result = match result {
                Ok(offer) => {
                    let quote = crate::inventory::mesh_upload::Quote {
                        token,
                        cost: offer.cost,
                        data: offer.data.clone(),
                    };
                    if let Some(pending) = offers.lock().get_mut(&token) {
                        pending.quoted(offer);
                        Ok(quote)
                    } else {
                        Err(Error::PriceChanged(None))
                    }
                }
                Err(error) => {
                    offers.lock().remove(&token);
                    Err(error)
                }
            };
            let _ = events.send(NetEvent::MeshQuoted { request, result });
        });
    }

    pub(super) fn discard_mesh_quote(&mut self, token: Uuid) {
        self.asset_imports.meshes.lock().remove(&token);
    }

    pub(super) fn upload_mesh(&mut self, request: Uuid, token: Uuid) {
        // Consume before spawning. Repeating the command cannot spend twice.
        let pending = self.asset_imports.meshes.lock().remove(&token);
        let http = self.sh.caps_http.clone();
        let events = self.sh.events.clone();
        let owner = self.agent_id();
        tokio::spawn(async move {
            let result = if let Some(pending) = pending {
                if pending.expired() {
                    Err(Error::PriceChanged(None))
                } else if let Some(offer) = pending.offer {
                    crate::inventory::mesh_upload::upload(&http, offer.uploader, owner, offer.cost, pending.upload).await
                } else {
                    Err(Error::Invalid)
                }
            } else {
                Err(Error::PriceChanged(None))
            };
            let _ = events.send(NetEvent::AssetUploaded { request, result });
        });
    }

    pub(super) fn import_asset(&mut self, request: Uuid, upload: Request) {
        if upload.kind == Kind::Mesh {
            emit(
                self.sh,
                NetEvent::AssetUploaded {
                    request,
                    result: Err(Error::Invalid),
                },
            );
            return;
        }
        let invalid = upload.body().is_err();
        if invalid {
            emit(
                self.sh,
                NetEvent::AssetUploaded {
                    request,
                    result: Err(Error::Invalid),
                },
            );
            return;
        }
        let http = self.sh.caps_http.clone();
        let events = self.sh.events.clone();
        let owner = self.agent_id();
        if upload.kind != Kind::Material {
            let caps = self.main_cap("NewFileAgentInventory").zip(self.main_cap("FetchInventory2"));
            if self.main_cap("NewFileAgentInventory").is_none() {
                self.legacy_import(request, upload);
                return;
            }
            tokio::spawn(async move {
                let result = if let Some((cap, fetch)) = caps {
                    crate::inventory::upload::upload(&http, &cap, &fetch, owner, upload).await
                } else {
                    Err(Error::Unavailable)
                };
                let _ = events.send(NetEvent::AssetUploaded { request, result });
            });
            return;
        }
        let caps = self
            .main_cap("UpdateMaterialAgentInventory")
            .zip(self.main_cap("FetchInventory2"))
            .zip(self.main_cap("InventoryAPIv3"))
            .zip(self.main_cap("FetchInventoryDescendents2"));
        let Some((((cap, fetch), ais), descendants)) = caps else {
            emit(
                self.sh,
                NetEvent::AssetUploaded {
                    request,
                    result: Err(Error::Unavailable),
                },
            );
            return;
        };
        // Refused before CreateInventoryItem, so no empty material is left behind
        // and the reason is certain (nothing was sent).
        if let Err(reason) = crate::inventory::operations::material_content_check(&upload.data) {
            log::warn!("material {} not uploaded: {reason}", upload.name);
            emit(
                self.sh,
                NetEvent::AssetUploaded {
                    request,
                    result: Err(Error::Refused(Rejection::from_message(&reason))),
                },
            );
            return;
        }
        if self.asset_imports.creates.len() >= 8 || upload.expected_cost != 0 {
            emit(
                self.sh,
                NetEvent::AssetUploaded {
                    request,
                    result: Err(Error::Invalid),
                },
            );
            return;
        }
        let callback = self.inventory_callback(false);
        let mut message = CreateInventoryItem::default();
        message.agent_data.agent_id = owner;
        message.agent_data.session_id = self.session_id();
        message.inventory_block.folder_id = upload.folder;
        message.inventory_block.callback_id = callback;
        message.inventory_block.next_owner_mask = upload.permissions.next_owner;
        message.inventory_block.type_ = 57;
        message.inventory_block.inv_type = 26;
        message.inventory_block.wearable_type = 255;
        message.inventory_block.transaction_id = Uuid::new_v4();
        message.inventory_block.name = str_field(&upload.name);
        message.inventory_block.description = str_field(&upload.description);
        self.asset_imports.creates.insert(
            callback,
            Pending {
                started: Instant::now(),
                request,
                upload,
                caps: (cap, fetch, ais, descendants),
                legacy_asset: None,
                permissions_applied: false,
            },
        );
        self.send_main(&message, true);
    }

    /// LLSettingsVOBase::createInventoryItem (create_inventory_settings): a
    /// new sky, water or day cycle item in `parent`, its asset uploaded once
    /// the simulator has created it.
    pub(super) fn create_settings_item(
        &mut self,
        request: Uuid,
        parent: Uuid,
        name: String,
        subtype: u8,
        next_owner_mask: Option<u32>,
        data: Vec<u8>,
    ) {
        let fail = |sh: &Shared, reason: &str| {
            emit(
                sh,
                NetEvent::InventoryContentSaved {
                    request,
                    item: Uuid::nil(),
                    result: Err(reason.to_owned()),
                },
            );
        };
        // isInventoryEnabled: both settings capabilities ("SettingsUnsuported")
        let caps = self
            .main_cap("UpdateSettingsAgentInventory")
            .filter(|_| self.main_cap("UpdateSettingsTaskInventory").is_some())
            .zip(self.main_cap("FetchInventory2"));
        let Some(caps) = caps else {
            fail(self.sh, aurora_i18n::tr!("session-saving-is-unavailable-in-this-region"));
            return;
        };
        if self.asset_imports.documents_full() || subtype > 2 {
            fail(self.sh, aurora_i18n::tr!("session-saving-is-unavailable-in-this-region"));
            return;
        }
        let callback = self.inventory_callback(false);
        let mut message = CreateInventoryItem::default();
        message.agent_data.agent_id = self.agent_id();
        message.agent_data.session_id = self.session_id();
        message.inventory_block.folder_id = parent;
        message.inventory_block.callback_id = callback;
        message.inventory_block.transaction_id = Uuid::new_v4();
        message.inventory_block.next_owner_mask = next_owner_mask.unwrap_or(SETTINGS_NEXT_OWNER);
        // AT_SETTINGS, IT_SETTINGS; the settings type rides in the
        // wearable type field (create_inventory_settings)
        message.inventory_block.type_ = 56;
        message.inventory_block.inv_type = 25;
        message.inventory_block.wearable_type = subtype;
        message.inventory_block.name = str_field(&name);
        message.inventory_block.description = str_field("");
        self.asset_imports.settings.insert(
            callback,
            SettingsCreate {
                started: Instant::now(),
                request,
                folder: parent,
                data,
                caps,
                asset_type: 56,
            },
        );
        self.send_main(&message, true);
    }

    /// create_new_item (indra/newview/llviewerinventory.cpp): wait for the
    /// inventory callback before saving content to the new UUID.
    pub(super) fn import_desktop_text(&mut self, import: Import) {
        let request = import.request;
        let report = |sh, outcome| emit(sh, NetEvent::DesktopTextImport { request, outcome });
        if self.logout_started.is_some()
            || import.owner != self.agent_id()
            || import.root != self.login.inventory_root
            || import.folder.is_nil()
            || !crate::inventory::text_import::valid_name(&import.name)
        {
            report(self.sh, Outcome::Unavailable);
            return;
        }
        let Some(region) = self.main.filter(|_| self.main_cap("FetchInventoryDescendents2").is_some()) else {
            report(self.sh, Outcome::Busy(import.content));
            return;
        };
        if self.asset_imports.documents_full() {
            report(self.sh, Outcome::Busy(import.content));
            return;
        }

        let (cap_name, inventory_type, subtype) = match &import.content {
            Content::Notecard(_) => ("UpdateNotecardAgentInventory", 7, 255),
            Content::Script(_) => ("UpdateScriptAgent", 10, 0),
        };
        let Some(upload_cap) = self.main_cap(cap_name).filter(|cap| !cap.is_empty()) else {
            report(self.sh, Outcome::Unavailable);
            return;
        };
        let fetch_cap = self.main_cap("FetchInventory2").filter(|cap| !cap.is_empty());
        let callback = self.inventory_callback(false);
        let mut message = CreateInventoryItem::default();
        message.agent_data.agent_id = import.owner;
        message.agent_data.session_id = self.session_id();
        message.inventory_block.folder_id = import.folder;
        message.inventory_block.callback_id = callback;
        message.inventory_block.next_owner_mask = import.permissions.next_owner;
        message.inventory_block.type_ = import.content.asset_type() as i8;
        message.inventory_block.inv_type = inventory_type;
        message.inventory_block.wearable_type = subtype;
        message.inventory_block.name = str_field(&import.name);
        self.remember_creation_permissions(callback, import.folder, import.content.asset_type(), import.permissions);
        self.asset_imports.desktop_text.insert(
            callback,
            TextCreate {
                started: Instant::now(),
                import,
                region,
                upload_cap,
                fetch_cap,
            },
        );
        self.send_main(&message, true);
    }

    fn desktop_text_created(&mut self, callback: u32, pending: TextCreate, item: &InvItem) {
        let request = pending.import.request;
        if !text_item_matches(&pending.import, item) {
            self.asset_imports.creation_permissions.remove(&callback);
            emit(
                self.sh,
                NetEvent::DesktopTextImport {
                    request,
                    outcome: Outcome::Unknown,
                },
            );
            return;
        }
        emit(
            self.sh,
            NetEvent::DesktopTextImport {
                request,
                outcome: Outcome::Created(item.clone()),
            },
        );
        if self.logout_started.is_some() || self.main != Some(pending.region) {
            self.asset_imports.creation_permissions.remove(&callback);
            emit(
                self.sh,
                NetEvent::DesktopTextImport {
                    request,
                    outcome: Outcome::UploadFailed,
                },
            );
            return;
        }
        self.apply_creation_permissions(callback, item);
        let http = self.sh.caps_http.clone();
        let events = self.sh.events.clone();
        let item_id = item.id;
        tokio::spawn(async move {
            save_desktop_text(&http, pending, item_id, &events).await;
        });
    }

    /// LLPreviewNotecard::onSaveComplete / create_inventory_item: use the
    /// server's callback id before uploading, exactly as for settings items.
    pub(super) fn create_notecard_item(
        &mut self,
        request: Uuid,
        parent: Uuid,
        name: String,
        data: Vec<u8>,
        permissions: Option<crate::inventory::upload::Permissions>,
    ) {
        let caps = self.main_cap("UpdateNotecardAgentInventory").zip(self.main_cap("FetchInventory2"));
        let valid =
            !parent.is_nil() && !name.trim().is_empty() && name.len() <= 255 && crate::inventory::notecard::Notecard::decode(&data).is_ok();
        let Some(caps) = caps.filter(|_| valid && !self.asset_imports.documents_full()) else {
            emit(
                self.sh,
                NetEvent::InventoryContentSaved {
                    request,
                    item: Uuid::nil(),
                    result: Err(aurora_i18n::tr!("session-saving-is-unavailable-in-this-region").into()),
                },
            );
            return;
        };
        let callback = self.inventory_callback(false);
        let mut message = CreateInventoryItem::default();
        message.agent_data.agent_id = self.agent_id();
        message.agent_data.session_id = self.session_id();
        message.inventory_block.folder_id = parent;
        message.inventory_block.callback_id = callback;
        // create_new_item (indra/newview/llviewerinventory.cpp) leaves the
        // transaction null: this new note has no previously uploaded asset.
        message.inventory_block.next_owner_mask = permissions.map_or(0x7fff_ffff, |rights| rights.next_owner);
        if let Some(rights) = permissions {
            self.remember_creation_permissions(callback, parent, 7, rights);
        }
        message.inventory_block.type_ = 7;
        message.inventory_block.inv_type = 7;
        message.inventory_block.wearable_type = 255;
        message.inventory_block.name = str_field(&name);
        self.asset_imports.settings.insert(
            callback,
            SettingsCreate {
                started: Instant::now(),
                request,
                folder: parent,
                data,
                caps,
                asset_type: 7,
            },
        );
        self.send_main(&message, true);
    }

    /// LLScriptRecoverQueue::recoverNext (indra/newview/
    /// llfloaterscriptrecover.cpp): a script item under `parent` with
    /// PERM_MOVE | PERM_TRANSFER for the next owner and no transaction, its
    /// text uploaded once the callback names it (onCreateScript).
    pub(super) fn create_script_item(&mut self, request: Uuid, parent: Uuid, name: String, description: String, source: String) {
        let cap = self.main_cap("UpdateScriptAgent");
        let valid = !parent.is_nil() && !name.trim().is_empty() && name.len() <= 255 && source.len() <= crate::scripts::MAX_SOURCE;
        let Some(cap) = cap.filter(|_| valid && !self.asset_imports.documents_full()) else {
            emit(
                self.sh,
                NetEvent::InventoryContentSaved {
                    request,
                    item: Uuid::nil(),
                    result: Err(aurora_i18n::tr!("script-save-unavailable").into()),
                },
            );
            return;
        };
        let callback = self.inventory_callback(false);
        let mut message = CreateInventoryItem::default();
        message.agent_data.agent_id = self.agent_id();
        message.agent_data.session_id = self.session_id();
        message.inventory_block.folder_id = parent;
        message.inventory_block.callback_id = callback;
        message.inventory_block.next_owner_mask = SCRIPT_RECOVERY_NEXT_OWNER;
        // AT_LSL_TEXT, IT_LSL, NO_INV_SUBTYPE
        message.inventory_block.type_ = 10;
        message.inventory_block.inv_type = 10;
        message.inventory_block.wearable_type = 0;
        message.inventory_block.name = str_field(&name);
        message.inventory_block.description = str_field(&description);
        self.asset_imports.settings.insert(
            callback,
            SettingsCreate {
                started: Instant::now(),
                request,
                folder: parent,
                data: source.into_bytes(),
                // the text goes up through UpdateScriptAgent alone
                caps: (cap, String::new()),
                asset_type: 10,
            },
        );
        self.send_main(&message, true);
    }

    /// onCreateScript / onSavedScript: the recovered text, compiled for Mono.
    /// A script that does not compile is saved all the same, as the Legacy
    /// Viewer does (only an HTTP failure fails the recovery).
    fn recovered_script_created(&mut self, pending: SettingsCreate, item: &InvItem) {
        let http = self.sh.caps_http.clone();
        let events = self.sh.events.clone();
        let mut item = item.clone();
        tokio::spawn(async move {
            let target = crate::scripts::ScriptTarget {
                object: Uuid::nil(),
                item: item.id,
            };
            let options = crate::scripts::ScriptOptions {
                compile: crate::scripts::CompileTarget::for_item(item.flags),
                running: false,
                experience: Uuid::nil(),
            };
            let source = String::from_utf8_lossy(&pending.data).into_owned();
            let result = crate::scripts::save(&http, &pending.caps.0, target, options, source)
                .await
                .map(|saved| {
                    item.asset_id = saved.asset;
                    item.clone()
                });
            let _ = events.send(NetEvent::InventoryContentSaved {
                request: pending.request,
                item: item.id,
                result,
            });
        });
    }

    /// onInventoryItemCreated: everyone may copy the new settings item, then its asset
    /// goes up (updateInventoryItem).
    fn settings_created(&mut self, pending: SettingsCreate, item: &InvItem) {
        let owner = self.agent_id();
        if item.id.is_nil() || item.asset_type != pending.asset_type || item.parent != pending.folder || item.owner != owner {
            emit(
                self.sh,
                NetEvent::InventoryContentSaved {
                    request: pending.request,
                    item: item.id,
                    result: Err(aurora_i18n::tr!("inventory-operations-the-server-did-not-confirm-the-save").into()),
                },
            );
            return;
        }
        if pending.asset_type == 10 {
            self.recovered_script_created(pending, item);
            return;
        }
        if pending.asset_type == 56 && item.everyone_mask != PERM_COPY {
            let callback = self.inventory_callback(false);
            let mut message = UpdateInventoryItem::default();
            message.agent_data.agent_id = owner;
            message.agent_data.session_id = self.session_id();
            message.agent_data.transaction_id = Uuid::new_v4();
            let rights = crate::inventory::upload::Permissions {
                next_owner: item.next_owner_mask,
                group: item.group_mask,
                everyone: PERM_COPY,
            };
            message.inventory_data.push(permission_update(item, rights, callback));
            self.send_main(&message, true);
        }
        let http = self.sh.caps_http.clone();
        let events = self.sh.events.clone();
        let id = item.id;
        tokio::spawn(async move {
            let (cap, fetch) = pending.caps;
            let result = crate::inventory::operations::save_content(&http, &cap, &fetch, owner, id, pending.asset_type, pending.data).await;
            let _ = events.send(NetEvent::InventoryContentSaved {
                request: pending.request,
                item: id,
                result,
            });
        });
    }

    fn apply_creation_permissions(&mut self, callback: u32, item: &InvItem) {
        if let Some(pending) = self.asset_imports.creation_permissions.remove(&callback)
            && let Some(update) = pending.update(item, self.agent_id())
        {
            let mut message = UpdateInventoryItem::default();
            message.agent_data.agent_id = self.agent_id();
            message.agent_data.session_id = self.session_id();
            message.agent_data.transaction_id = Uuid::new_v4();
            message.inventory_data.push(update);
            self.send_main(&message, true);
        }
    }

    pub(super) fn asset_created(&mut self, from: std::net::SocketAddr, callback: u32, item: &InvItem) {
        if let Some(pending) = self.asset_imports.desktop_text.get(&callback) {
            if pending.region == from
                && let Some(pending) = self.asset_imports.desktop_text.remove(&callback)
            {
                self.desktop_text_created(callback, pending, item);
            }
            return;
        }
        self.apply_creation_permissions(callback, item);
        if let Some(pending) = self.asset_imports.settings.remove(&callback) {
            self.settings_created(pending, item);
            return;
        }
        let Some(mut pending) = self.asset_imports.creates.remove(&callback) else {
            return;
        };
        if let Some(asset) = pending.legacy_asset {
            let valid = !item.id.is_nil()
                && item.asset_id == asset
                && item.parent == pending.upload.folder
                && item.asset_type == pending.upload.kind.asset_type()
                && item.owner == self.agent_id();
            let rights = pending.upload.permissions;
            let masks_match = item.group_mask == (rights.group & item.owner_mask)
                && item.everyone_mask == (rights.everyone & item.owner_mask)
                && item.next_owner_mask == (rights.next_owner & item.owner_mask);
            if valid && !masks_match && !pending.permissions_applied {
                let callback = self.inventory_callback(false);
                let mut message = UpdateInventoryItem::default();
                message.agent_data.agent_id = self.agent_id();
                message.agent_data.session_id = self.session_id();
                message.agent_data.transaction_id = Uuid::new_v4();
                message.inventory_data.push(permission_update(item, rights, callback));
                pending.permissions_applied = true;
                self.asset_imports.creates.insert(callback, pending);
                self.send_main(&message, true);
                return;
            }
            let result = if item.asset_id == asset
                && item.parent == pending.upload.folder
                && item.asset_type == pending.upload.kind.asset_type()
                && item.owner == self.agent_id()
                && valid
                && masks_match
            {
                Ok(Receipt {
                    item: item.clone(),
                    cost: pending.upload.expected_cost,
                })
            } else {
                Err(Error::Verification)
            };
            emit(
                self.sh,
                NetEvent::AssetUploaded {
                    request: pending.request,
                    result,
                },
            );
            return;
        }
        let http = self.sh.caps_http.clone();
        let events = self.sh.events.clone();
        let owner = self.agent_id();
        let item = item.clone();
        tokio::spawn(async move {
            let request = pending.request;
            let result = async {
                if item.id.is_nil() || item.parent != pending.upload.folder || item.asset_type != 57 || item.owner != owner {
                    return Err(Error::Response);
                }
                let (cap, fetch, ais, descendants) = pending.caps;
                let rights = pending.upload.permissions;
                let change = Mutation {
                    operations: vec![Operation::Patch {
                        id: item.id,
                        folder: false,
                        body: aurora_llsd::llsd_map! {
                            "name" => pending.upload.name.as_str(), "desc" => pending.upload.description.as_str(),
                            "permissions" => aurora_llsd::llsd_map! { "next_owner_mask" => rights.next_owner as i32,
                                "group_mask" => rights.group as i32, "everyone_mask" => rights.everyone as i32 }
                        },
                    }],
                    refresh: vec![item.parent],
                    ..Default::default()
                };
                // LLObjectsMaterialItemCallback::fire (indra/newview/llmaterialeditor.cpp)
                // uploads the material whatever becomes of the name and rights
                // update: an item left without its material would never load.
                if let Err(reason) = crate::inventory::operations::mutate(&http, &ais, &descendants, owner, change).await {
                    log::warn!("material item name and rights not confirmed, its material is sent anyway: {reason}");
                }
                let saved = crate::inventory::operations::save_content(&http, &cap, &fetch, owner, item.id, 57, pending.upload.data)
                    .await
                    .map_err(material_save_error)?;
                if saved.id != item.id
                    || saved.parent != item.parent
                    || saved.owner != owner
                    || saved.asset_type != 57
                    || saved.asset_id.is_nil()
                {
                    return Err(Error::Verification);
                }
                Ok(Receipt { item: saved, cost: 0 })
            }
            .await;
            let _ = events.send(NetEvent::AssetUploaded { request, result });
        });
    }

    pub(super) fn expire_asset_imports(&mut self) {
        self.asset_imports
            .creation_permissions
            .retain(|_, pending| pending.started.elapsed() < Duration::from_secs(60));
        self.asset_imports.meshes.lock().retain(|_, pending| !pending.expired());
        self.asset_imports.desktop_text.retain(|_, pending| {
            if pending.started.elapsed() < Duration::from_secs(60) {
                return true;
            }
            emit(
                self.sh,
                NetEvent::DesktopTextImport {
                    request: pending.import.request,
                    outcome: Outcome::Unknown,
                },
            );
            false
        });
        self.asset_imports.settings.retain(|_, pending| {
            if pending.started.elapsed() < Duration::from_secs(60) {
                return true;
            }
            emit(
                self.sh,
                NetEvent::InventoryContentSaved {
                    request: pending.request,
                    item: Uuid::nil(),
                    result: Err(aurora_i18n::tr!("inventory-operations-the-server-did-not-confirm-the-save").into()),
                },
            );
            false
        });
        self.asset_imports.creates.retain(|_, pending| {
            if pending.started.elapsed() < Duration::from_secs(60) {
                return true;
            }
            log::warn!("asset import creation timed out before the simulator confirmed inventory item creation");
            emit(
                self.sh,
                NetEvent::AssetUploaded {
                    request: pending.request,
                    result: Err(Error::Unknown),
                },
            );
            false
        });
    }
}

fn text_item_matches(import: &Import, item: &InvItem) -> bool {
    let kind = import.content.asset_type();
    !item.id.is_nil()
        && item.parent == import.folder
        && item.owner == import.owner
        && !item.group_owned
        && item.asset_type == kind
        && item.inv_type == kind
        && (kind != 10 || !crate::scripts::is_slua_item(item.flags))
}

async fn save_desktop_text(http: &reqwest::Client, pending: TextCreate, item: Uuid, events: &crate::EventSender) {
    let TextCreate {
        import,
        upload_cap,
        fetch_cap,
        ..
    } = pending;
    let request = import.request;
    let emit = |outcome| events.send(NetEvent::DesktopTextImport { request, outcome });
    let kind = import.content.asset_type();
    let owner = import.owner;
    let folder = import.folder;
    let rights = import.permissions;
    let result = match import.content {
        Content::Notecard(data) => {
            let body = aurora_llsd::llsd_map! { "item_id" => item };
            match tokio::time::timeout(
                Duration::from_secs(60),
                crate::inventory::operations::upload_content_staged(http, &upload_cap, &body, data),
            )
            .await
            {
                Ok(Ok(ack)) => {
                    let asset = ack["new_asset"].as_uuid();
                    if asset.is_nil() {
                        Err(SaveError::uncertain(""))
                    } else {
                        Ok((asset, None))
                    }
                }
                Ok(Err(error)) => Err(error),
                Err(_) => Err(SaveError::uncertain("")),
            }
        }
        Content::Script(source) => {
            use crate::scripts::{CompileTarget, ScriptOptions, ScriptTarget};
            // LLPreviewLSL::saveIfNeeded (indra/newview/llpreviewscript.cpp).
            let target = ScriptTarget { object: Uuid::nil(), item };
            let options = ScriptOptions {
                compile: CompileTarget::Mono,
                running: false,
                experience: Uuid::nil(),
            };
            crate::scripts::save_staged(http, &upload_cap, target, options, source)
                .await
                .map(|mut saved| {
                    saved.errors.truncate(32);
                    for error in &mut saved.errors {
                        error.truncate(error.floor_char_boundary(512));
                    }
                    (saved.asset, Some(saved))
                })
        }
    };
    let (asset, script) = match result {
        Ok(saved) => saved,
        Err(error) => {
            emit(if error.before_content {
                Outcome::UploadFailed
            } else {
                Outcome::Unknown
            });
            return;
        }
    };

    if !emit(Outcome::Saved { asset, script }) {
        return;
    }

    // Permission updates use UDP and can arrive after the upload's first
    // FetchInventory2. Recheck the saved item, without sending content again.
    let mut verified_item = None;
    let mut permissions_verified = false;
    if let Some(fetch_cap) = fetch_cap {
        let body = crate::inventory::fetch_items_body(&[item], owner);
        for delay in [0, 300, 900] {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let fetch = crate::caps::post_llsd(http, &fetch_cap, &body);
            if let Ok(Ok(doc)) = tokio::time::timeout(Duration::from_secs(5), fetch).await
                && let Some(saved) = crate::inventory::parse_items_response(&doc).into_iter().find(|saved| {
                    saved.id == item
                        && saved.owner == owner
                        && saved.parent == folder
                        && saved.asset_type == kind
                        && saved.inv_type == kind
                        && saved.asset_id == asset
                })
            {
                let expected = permission_update(&saved, rights, 0);
                permissions_verified = saved.next_owner_mask == expected.next_owner_mask
                    && saved.group_mask == expected.group_mask
                    && saved.everyone_mask == expected.everyone_mask;
                verified_item = Some(saved);
                if permissions_verified {
                    break;
                }
            }
        }
    }
    emit(Outcome::Verification {
        asset,
        verified_item,
        permissions_verified,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn desktop_text_publishes_save_before_finishing_failed_readback() {
        use crate::outfits::tests::server;
        use aurora_llsd::llsd_map;

        let asset = Uuid::from_u128(12);
        let item_id = Uuid::from_u128(13);
        let owner = Uuid::from_u128(1);
        let root = Uuid::from_u128(2);
        let folder = Uuid::from_u128(3);
        let request = Uuid::from_u128(4);
        let source = "\tdefault { state_entry() { } }\r\n";
        let (file_url, upload_capture) = server(vec![(
            200,
            llsd_map! {
                "state" => "complete", "new_asset" => asset, "compiled" => false,
                "errors" => Llsd::Array(vec!["(3, 2) ERROR".into()]),
            },
        )])
        .await;
        let (update_url, update_capture) = server(vec![(200, llsd_map! { "state" => "upload", "uploader" => file_url })]).await;
        let (fetch_url, fetch_capture) = server(vec![(200, Llsd::new_map()), (200, Llsd::new_map()), (200, Llsd::new_map())]).await;

        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("viewer socket"));
        sock.writable().await.expect("socket ready");
        let simulator = UdpSocket::bind("127.0.0.1:0").await.expect("fake simulator");
        let region = simulator.local_addr().expect("region address");
        let mut sim = Sim::new(region, 10);
        sim.caps = Arc::new(HashMap::from([
            ("FetchInventoryDescendents2".into(), fetch_url.clone()),
            ("FetchInventory2".into(), fetch_url),
            ("UpdateScriptAgent".into(), update_url),
        ]));
        let (events, received) = crossbeam_channel::unbounded();
        let shared = crate::session::script_dialog_tests::test_shared(events);
        let mut session = crate::session::script_dialog_tests::test_session(&shared, sock, [(region, sim)].into(), region);
        Arc::get_mut(&mut session.login).expect("test fixture owns login").inventory_root = root;
        let rights = crate::inventory::upload::Permissions {
            next_owner: 0x8000,
            group: 0x2000,
            everyone: 0x8000,
        };
        session.import_desktop_text(Import {
            request,
            owner,
            root,
            folder,
            name: "Door".into(),
            permissions: rights,
            content: Content::Script(source.into()),
        });
        let mut bytes = [0; 1500];
        let (size, _) = tokio::time::timeout(Duration::from_secs(2), simulator.recv_from(&mut bytes))
            .await
            .expect("create sent")
            .expect("received datagram");
        let packet = parse_packet(&bytes[..size]).expect("create packet");
        assert!(packet.reliable());
        let create: CreateInventoryItem = packet.decode().expect("CreateInventoryItem");
        let callback = create.inventory_block.callback_id;
        assert_eq!(create.inventory_block.folder_id, folder);
        assert_eq!(create.inventory_block.next_owner_mask, rights.next_owner);
        assert_eq!(create.inventory_block.type_, 10);
        assert_eq!(create.inventory_block.inv_type, 10);
        assert_eq!(field_str(&create.inventory_block.name), "Door");
        let item = InvItem {
            id: item_id,
            parent: folder,
            owner,
            asset_type: 10,
            inv_type: 10,
            name: "Door".into(),
            owner_mask: u32::MAX,
            ..InvItem::default()
        };
        assert!(!text_item_matches(
            &session.asset_imports.desktop_text[&callback].import,
            &InvItem {
                owner: Uuid::nil(),
                ..item.clone()
            }
        ));
        assert!(!text_item_matches(
            &session.asset_imports.desktop_text[&callback].import,
            &InvItem {
                parent: Uuid::nil(),
                ..item.clone()
            }
        ));
        assert!(!text_item_matches(
            &session.asset_imports.desktop_text[&callback].import,
            &InvItem { flags: 1, ..item.clone() }
        ));
        session.asset_created(region, callback + 1, &item);
        session.asset_created("127.0.0.1:1".parse().expect("different region"), callback, &item);
        assert!(received.is_empty());
        session.asset_created(region, callback, &item);
        session.asset_created(region, callback, &item);
        assert!(matches!(
            received.try_recv().expect("creation event").1,
            NetEvent::DesktopTextImport { request: id, outcome: Outcome::Created(created) }
                if id == request && created.id == item_id
        ));
        let (size, _) = tokio::time::timeout(Duration::from_secs(2), simulator.recv_from(&mut bytes))
            .await
            .expect("permission correction sent")
            .expect("received datagram");
        let update: UpdateInventoryItem = parse_packet(&bytes[..size])
            .expect("permission packet")
            .decode()
            .expect("UpdateInventoryItem");
        assert_eq!(update.inventory_data[0].item_id, item_id);
        assert_eq!(update.inventory_data[0].group_mask, rights.group);
        assert_eq!(update.inventory_data[0].everyone_mask, rights.everyone);
        let next_event = || async {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let Ok((_, event)) = received.try_recv() {
                        break event;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("import event")
        };
        assert!(matches!(
            next_event().await,
            NetEvent::DesktopTextImport { request: id, outcome: Outcome::Saved { asset: saved, script: Some(script) } }
                if id == request && saved == asset && script.checked && !script.compiled && script.errors == ["(3, 2) ERROR"]
        ));
        assert!(
            !fetch_capture.is_finished(),
            "the upload acknowledgement precedes the final readback"
        );
        let (updates, uploads, fetches) = tokio::time::timeout(Duration::from_secs(10), async {
            (
                update_capture.await.expect("update capture"),
                upload_capture.await.expect("upload capture"),
                fetch_capture.await.expect("readback capture"),
            )
        })
        .await
        .expect("only localhost requests");
        assert_eq!(updates[0].1["item_id"].as_uuid(), item_id);
        assert_eq!(updates[0].1["target"].as_str(), "mono");
        assert_eq!(uploads[0].1.as_binary(), source.as_bytes());
        assert!(fetches.iter().all(|(_, body)| body["items"].at(0)["item_id"].as_uuid() == item_id));
        assert!(matches!(
            next_event().await,
            NetEvent::DesktopTextImport { request: id, outcome: Outcome::Verification {
                asset: saved, verified_item: None, permissions_verified: false,
            }} if id == request && saved == asset
        ));
    }

    #[test]
    fn material_save_error_keeps_the_server_reason() {
        assert_eq!(
            material_save_error("server rejected blend map".into()),
            Error::Uncertain(Rejection {
                status: None,
                identifier: String::new(),
                messages: vec!["server rejected blend map".into()],
            })
        );
    }

    #[test]
    fn creation_defaults_only_update_the_expected_owned_item_and_preserve_owner_rights() {
        let owner = Uuid::from_u128(1);
        let mut item = InvItem {
            id: Uuid::from_u128(2),
            parent: Uuid::from_u128(3),
            owner,
            asset_type: 7,
            owner_mask: 0x7fff_ffff,
            base_mask: 0x7fff_ffff,
            next_owner_mask: 0x7fff_ffff,
            ..Default::default()
        };
        let pending = CreationPermissions {
            started: Instant::now(),
            parent: item.parent,
            asset_type: 7,
            rights: crate::inventory::upload::Permissions {
                next_owner: 0x8_2000,
                group: 0x8_c000,
                everyone: 0x8000,
            },
        };
        let update = pending.update(&item, owner).unwrap();
        assert_eq!(
            (update.next_owner_mask, update.group_mask, update.everyone_mask),
            (0x8_2000, 0x8_c000, 0x8000)
        );
        assert_eq!((update.owner_mask, update.base_mask), (item.owner_mask, item.base_mask));
        for (parent, asset_type, item_owner) in [
            (Uuid::nil(), 7, owner),
            (item.parent, 10, owner),
            (item.parent, 7, Uuid::from_u128(4)),
        ] {
            assert!(
                pending
                    .update(
                        &InvItem {
                            parent,
                            asset_type,
                            owner: item_owner,
                            ..item.clone()
                        },
                        owner
                    )
                    .is_none()
            );
        }
        item.owner_mask = 0x2000;
        let update = pending.update(&item, owner).unwrap();
        assert_eq!((update.next_owner_mask, update.group_mask, update.everyone_mask), (0x2000, 0, 0));
    }

    #[test]
    fn mesh_import_parity_slow_quote_keeps_the_full_transfer_and_confirmation_windows() {
        let mut pending = MeshOffer {
            started: Instant::now() - Duration::from_secs(400),
            upload: crate::inventory::mesh_upload::Request {
                folder: Uuid::from_u128(1),
                texture_folder: Uuid::from_u128(2),
                name: "Mesh".into(),
                description: String::new(),
                query: Arc::new(vec![1]),
                resources: Arc::new(vec![1]),
                timeout_secs: 600,
            },
            offer: None,
        };
        assert!(!pending.expired(), "a fee request can take the configured 600 seconds");
        pending.quoted(crate::inventory::mesh_upload::Offer {
            uploader: String::new(),
            cost: 0,
            data: Llsd::Undef,
        });
        assert!(!pending.expired(), "confirmation time starts when the quote arrives");
        pending.started = Instant::now() - Duration::from_secs(301);
        assert!(pending.expired());
    }
    #[test]
    fn legacy_rights_update_preserves_identity_and_clamps_to_owned_permissions() {
        let item = InvItem {
            id: Uuid::from_u128(1),
            parent: Uuid::from_u128(2),
            asset_id: Uuid::from_u128(3),
            creator: Uuid::from_u128(4),
            owner: Uuid::from_u128(5),
            asset_type: 20,
            inv_type: 19,
            name: "Motion".into(),
            desc: "Test".into(),
            owner_mask: 0x0008a000,
            base_mask: 0x7fff_ffff,
            flags: 7,
            created_at: 12,
            ..Default::default()
        };
        let update = permission_update(
            &item,
            crate::inventory::upload::Permissions {
                next_owner: 0x7fff_ffff,
                group: 0x7fff_ffff,
                everyone: 0x7fff_ffff,
            },
            9,
        );
        assert_eq!(update.item_id, item.id);
        assert_eq!(update.folder_id, item.parent);
        assert_eq!(update.callback_id, 9);
        assert_eq!(update.transaction_id, Uuid::nil());
        assert_eq!(update.group_mask, item.owner_mask);
        assert_eq!(update.everyone_mask, item.owner_mask);
        assert_eq!(update.next_owner_mask, item.owner_mask);
        assert_eq!(update.type_, 20);
        assert_eq!(update.inv_type, 19);
        assert_eq!(field_str(&update.name), "Motion");
        assert_eq!(field_str(&update.description), "Test");
        // UUID bytes use little-endian words; the next-owner mask is omitted
        // by the protocol checksum, as LLPermissions::getCRC32 specifies.
        assert_eq!(
            update.crc,
            0x0f00_0000u32
                .wrapping_add(item.base_mask)
                .wrapping_add(item.owner_mask * 3)
                .wrapping_add(20 + 19 + 7 + 12)
        );
    }
}
