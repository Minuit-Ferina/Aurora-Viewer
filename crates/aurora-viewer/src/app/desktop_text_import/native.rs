//! Native transfer IDs bind the drop position to the paths received later.
use super::*;
use crate::ui::inventory::text_import::{self, Target};
use winit::{
    data_transfer::{DataTransferId, DataTransferLimits, TypeHint},
    dpi::PhysicalPosition,
    error::TransferError,
    event_loop::{AsyncRequestSerial, DndAction},
    window::Window,
};

const URI_BYTES: usize = 1024 * 1024;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Default)]
pub(super) struct DropState {
    drag: Option<Drag>,
    transfer: Option<Transfer>,
}

struct Drag {
    window: WindowId,
    id: DataTransferId,
    position: Option<PhysicalPosition<f64>>,
    copy: Option<bool>,
    next_frame: Instant,
}

struct Transfer {
    window: WindowId,
    id: DataTransferId,
    serial: AsyncRequestSerial,
    session: Session,
    destination: Option<Destination>,
    deadline: Instant,
}

impl DropState {
    pub(super) fn expire(&mut self, now: Instant) -> bool {
        if self.transfer.as_ref().is_some_and(|transfer| transfer.deadline <= now) {
            self.transfer = None;
            return true;
        }
        false
    }

    fn drag_matches(&self, window: WindowId, id: DataTransferId) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.window == window && drag.id == id)
    }

    fn take_transfer(&mut self, window: WindowId, id: DataTransferId, serial: AsyncRequestSerial) -> Option<Transfer> {
        if self
            .transfer
            .as_ref()
            .is_some_and(|transfer| transfer.id == id && transfer.window == window && transfer.serial == serial)
        {
            self.transfer.take()
        } else {
            None
        }
    }
}

impl Controller {
    pub(in crate::app) fn drag_pointer(&self, window: WindowId) -> Option<(DataTransferId, PhysicalPosition<f64>)> {
        let drag = self.native.drag.as_ref().filter(|drag| drag.window == window)?;
        Some((drag.id, drag.position?))
    }

    pub(in crate::app) fn drag_frame_deadline(&self) -> Option<Instant> {
        self.native.drag.as_ref().map(|drag| drag.next_frame)
    }

    /// Windows and Explorer exchange hover messages synchronously. Leave the
    /// event loop available between frames, even at the unfocused frame cap.
    pub(in crate::app) fn pace_drag(&mut self, start: Instant, period: Option<Duration>) -> bool {
        let Some(drag) = self.native.drag.as_mut() else {
            return false;
        };
        let period = period.unwrap_or_default().max(Duration::from_millis(16));
        drag.next_frame = (start + period).max(Instant::now() + Duration::from_millis(2));
        true
    }
}

impl App {
    fn desktop_text_surface(&self, id: WindowId) -> Option<(&dyn Window, egui::ViewportId)> {
        let gfx = self.gfx.as_ref()?;
        if gfx.window.id() == id {
            Some((gfx.window.as_ref(), egui::ViewportId::ROOT))
        } else {
            self.native_windows.desktop_inventory_surface(id)
        }
    }

    fn desktop_text_native_target(&self, id: WindowId, position: Option<PhysicalPosition<f64>>) -> Target {
        let Some((window, viewport)) = self.desktop_text_surface(id) else {
            return Target::Reject;
        };
        text_import::hit(
            &self.egui_ctx,
            viewport,
            id,
            window.surface_size(),
            window.scale_factor(),
            position,
            &self.world.inventory,
        )
    }

    pub(in crate::app) fn desktop_text_drop_enabled(&self) -> bool {
        !self.desktop_text_import.exiting
            && matches!(self.screen, Screen::World)
            && !self.world.rlva.has("showinv")
            && !self.exit_prompt.open
    }

    fn reply_to_desktop_drag(&mut self, event_loop: &dyn ActiveEventLoop, window: WindowId, position: Option<PhysicalPosition<f64>>) {
        let copy = self.desktop_text_native_target(window, position) != Target::Reject;
        if let Some(drag) = &mut self.desktop_text_import.native.drag
            && drag.copy != Some(copy)
        {
            let actions: &[DndAction] = if copy { &[DndAction::Copy] } else { &[] };
            if event_loop.set_valid_dnd_actions(drag.id, actions).is_ok() {
                drag.copy = Some(copy);
            }
        }
    }

    pub(in crate::app) fn desktop_text_native(&mut self, event_loop: &dyn ActiveEventLoop, window: WindowId, event: &WindowEvent) -> bool {
        if !self.desktop_text_drop_enabled() {
            self.desktop_text_import.native.drag = None;
            return false;
        }
        match *event {
            WindowEvent::DragEntered { id, position } => {
                if self.desktop_text_import.native.transfer.is_some()
                    || self.desktop_text_surface(window).is_none()
                    || !event_loop.data_transfer(id).is_ok_and(|data| data.has_type(&TypeHint::UriList))
                {
                    return false;
                }
                self.desktop_text_import.native.drag = Some(Drag {
                    window,
                    id,
                    position,
                    copy: None,
                    next_frame: Instant::now(),
                });
                self.reply_to_desktop_drag(event_loop, window, position);
            }
            WindowEvent::DragPosition { id, position, .. } => {
                let native = &mut self.desktop_text_import.native;
                if !native.drag_matches(window, id) {
                    return false;
                }
                if let Some(drag) = &mut native.drag {
                    drag.position = Some(position);
                }
                self.reply_to_desktop_drag(event_loop, window, Some(position));
            }
            WindowEvent::DragLeft { id } => {
                let native = &mut self.desktop_text_import.native;
                if !native.drag_matches(window, id) {
                    return false;
                }
                native.drag = None;
            }
            WindowEvent::DragDropped { id, .. } => {
                let native = &mut self.desktop_text_import.native;
                if !native.drag_matches(window, id) {
                    return false;
                }
                let position = native.drag.take().and_then(|drag| drag.position);
                let target = self.desktop_text_native_target(window, position);
                let destination = match target {
                    Target::Folder(folder) => {
                        let Some(to) = self.text_import_destination(folder) else {
                            return true;
                        };
                        Some(to)
                    }
                    Target::Choose => None,
                    Target::Reject => return true,
                };
                match request_paths(event_loop, id) {
                    Ok(serial) => {
                        self.desktop_text_import.native.transfer = Some(Transfer {
                            window,
                            id,
                            serial,
                            session: self.text_import_session(),
                            destination,
                            deadline: Instant::now() + TRANSFER_TIMEOUT,
                        });
                    }
                    Err(_) => self.desktop_text_drop_unavailable(),
                }
            }
            WindowEvent::DataTransferReceived { id, serial, ref value } => {
                let Some(transfer) = self.desktop_text_import.native.take_transfer(window, id, serial) else {
                    return false;
                };
                if transfer.session != self.text_import_session() || transfer.deadline <= Instant::now() {
                    return true;
                }
                match value.try_as_uris().ok().and_then(|uris| local_paths(&uris)) {
                    Some(paths) if !paths.is_empty() => {
                        let queued = if let Some(to) = transfer.destination {
                            self.desktop_text_import.enqueue(Uuid::new_v4(), to, paths)
                        } else {
                            self.desktop_text_import.await_folder(transfer.session, paths)
                        };
                        if !queued {
                            self.inventory_ui.message = aurora_i18n::tr!("desktop-text-import-error-full").into();
                        }
                    }
                    _ => self.desktop_text_drop_unavailable(),
                }
            }
            _ => return false,
        }
        self.egui_ctx.request_repaint_of(egui::ViewportId::ROOT);
        self.native_windows.repaint_inventory_surface(window);
        true
    }

    pub(super) fn desktop_text_drop_unavailable(&mut self) {
        self.inventory_ui.message = aurora_i18n::tr!("desktop-text-import-error-transfer").into();
    }
}

fn request_paths(event_loop: &dyn ActiveEventLoop, id: DataTransferId) -> Result<AsyncRequestSerial, TransferError> {
    let result = event_loop.fetch_data_transfer_with_limits(
        id,
        &TypeHint::UriList,
        DataTransferLimits {
            max_bytes: URI_BYTES,
            timeout: TRANSFER_TIMEOUT,
        },
    );
    // Win32 and AppKit carry platform file lists. X11 has no per-request limit in this backend.
    match result {
        Err(TransferError::NotSupported(_)) => event_loop.fetch_data_transfer(id, &TypeHint::UriList),
        result => result,
    }
}

fn local_paths(uris: &[String]) -> Option<Vec<PathBuf>> {
    if uris.len() > MAX_UNFINISHED || uris.iter().try_fold(0usize, |bytes, uri| bytes.checked_add(uri.len()))? > URI_BYTES {
        return None;
    }
    let mut paths = Vec::with_capacity(uris.len());
    for uri in uris {
        let url = reqwest::Url::parse(uri).ok()?;
        if url.scheme() != "file"
            || url.host_str().is_some_and(|host| host != "localhost")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        let path = url.to_file_path().ok()?;
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    Some(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_response_cannot_retarget_or_reuse_a_drop() {
        let window = WindowId::from_raw(1);
        let other_window = WindowId::from_raw(2);
        let id = DataTransferId::from_raw(1);
        let previous = DataTransferId::from_raw(2);
        let serial = AsyncRequestSerial::get();
        let session = Session {
            generation: Uuid::from_u128(1),
            owner: Uuid::from_u128(2),
            root: Uuid::from_u128(3),
        };
        let folder = Uuid::from_u128(4);
        let now = Instant::now();
        let mut state = DropState {
            transfer: Some(Transfer {
                window,
                id,
                serial,
                session,
                destination: Some(Destination {
                    session,
                    folder,
                    defaults: Defaults::default(),
                }),
                deadline: now + TRANSFER_TIMEOUT,
            }),
            ..Default::default()
        };
        assert!(state.take_transfer(other_window, id, serial).is_none());
        assert!(state.take_transfer(window, previous, serial).is_none());
        assert!(state.take_transfer(window, id, AsyncRequestSerial::get()).is_none());
        let transfer = state.take_transfer(window, id, serial).expect("matching response");
        assert_eq!(transfer.destination.expect("captured destination").folder, folder);
        assert!(state.take_transfer(window, id, serial).is_none());
    }

    #[test]
    fn accepts_only_local_file_urls_and_preserves_batch_order() {
        let folder = std::env::temp_dir();
        let a = folder.join("Café #1.txt");
        let b = folder.join("Script.lsl");
        let url = |path: &PathBuf| reqwest::Url::from_file_path(path).expect("local file URI").to_string();
        assert_eq!(local_paths(&[url(&a), url(&b), url(&a)]), Some(vec![a, b]));
        assert_eq!(local_paths(&["https://example.invalid/Script.lsl".into()]), None);
        assert_eq!(local_paths(&["file://example.invalid/Script.lsl".into()]), None);
    }
}
