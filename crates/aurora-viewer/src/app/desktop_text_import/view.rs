//! Item and queued-file statuses shown inside the inventory.
use super::*;
use crate::ui::inventory::text_import::{Feedback, PendingBatch, PendingRow, Status};
use aurora_i18n::tr;

pub(super) fn feedback(inv: &Inventory, ctl: &Controller) -> Feedback {
    let mut result = Feedback {
        choice: ctl.awaiting_folder.front().map(|drop| drop.id),
        ..Default::default()
    };
    for batch in &ctl.batches {
        let mut rows = Vec::new();
        for row in &batch.rows {
            let status = row_status(row);
            if let Some(id) = row.created.filter(|id| inv.items.contains_key(id)) {
                result.items.insert(id, status);
            } else {
                rows.push(PendingRow {
                    name: row.filename.clone(),
                    status,
                });
            }
        }
        if !rows.is_empty() || batch.rows.iter().any(|row| row.state.pending()) {
            result.pending.push(PendingBatch {
                id: batch.id,
                folder: batch.to.folder,
                rows,
                cancellable: batch.rows.iter().any(|row| row.state.pending()),
                finished: batch.rows.iter().all(|row| !row.state.unfinished()),
            });
        }
    }
    result
}

pub(super) fn row_status(row: &Row) -> Status {
    let message = status(row.kind, &row.state);
    let mut error = matches!(row.state, State::Failed(_));
    let mut detail = String::new();
    let label = match &row.state {
        State::Failed(Error::Cancelled) => message,
        State::Failed(_) => {
            detail.push_str(message);
            tr!("desktop-text-import-error")
        }
        State::Saved { script, verified } => {
            if let Some(script) = script.as_ref().filter(|script| !script.compiled) {
                error = true;
                detail = script.errors.join("\n");
            }
            if *verified == Some(false) {
                if !detail.is_empty() {
                    detail.push('\n');
                }
                detail.push_str(tr!("desktop-text-import-unverified"));
            }
            message
        }
        _ => message,
    };
    Status {
        label: label.into(),
        detail,
        error,
    }
}

fn status(kind: Option<Kind>, state: &State) -> &str {
    match state {
        State::Waiting(_) | State::Ready(_) => tr!("desktop-text-import-queued"),
        State::Reading => tr!("desktop-text-import-reading"),
        State::Creating => tr!("desktop-text-import-creating"),
        State::Uploading if kind == Some(Kind::Script) => tr!("desktop-text-import-compiling"),
        State::Uploading => tr!("desktop-text-import-uploading"),
        State::Saved { script, .. } => match script {
            Some(script) if !script.compiled => tr!("desktop-text-import-compile-error"),
            Some(script) if script.checked => tr!("desktop-text-import-compiled"),
            Some(_) => tr!("desktop-text-import-unchecked"),
            None => tr!("desktop-text-import-saved"),
        },
        State::Failed(error) => match error {
            Error::InvalidName => tr!("desktop-text-import-error-name"),
            Error::Extension => tr!("desktop-text-import-error-extension"),
            Error::Read => tr!("desktop-text-import-error-read"),
            Error::TooLarge => tr!("desktop-text-import-error-size"),
            Error::Utf8 => tr!("desktop-text-import-error-utf8"),
            Error::Nul => tr!("desktop-text-import-error-nul"),
            Error::Marker => tr!("desktop-text-import-error-marker"),
            Error::Payload => tr!("desktop-text-import-error-payload"),
            Error::Destination => tr!("desktop-text-import-error-destination"),
            Error::Restricted => tr!("desktop-text-import-error-restricted"),
            Error::Unavailable => tr!("desktop-text-import-error-unavailable"),
            Error::Upload => tr!("desktop-text-import-error-upload"),
            Error::Unknown => tr!("desktop-text-import-error-unknown"),
            Error::Cancelled => tr!("desktop-text-import-error-cancelled"),
        },
    }
}
