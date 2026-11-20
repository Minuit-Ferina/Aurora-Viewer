//! Stage explicit local files without mutating source or opening editors.
use super::{Error, Kind, Row, State};
use aurora_net::inventory::{
    notecard::{MAX_TEXT_BYTES, Notecard},
    text_import::{Content, valid_name},
};
use aurora_net::scripts::MAX_SOURCE;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub(super) fn row(path: PathBuf) -> Row {
    let file = path.file_name().and_then(|name| name.to_str());
    let classified = file.ok_or(Error::InvalidName).and_then(classify);
    let (name, kind, state) = match classified {
        Ok((name, kind)) => (name.into(), Some(kind), State::Waiting(path.clone())),
        Err(reason) => (String::new(), None, State::Failed(reason)),
    };
    Row {
        id: Uuid::new_v4(),
        filename: file.unwrap_or_default().into(),
        kind,
        name,
        created: None,
        state,
    }
}

fn classify(filename: &str) -> Result<(&str, Kind), Error> {
    let (name, ext) = filename.rsplit_once('.').ok_or(Error::Extension)?;
    let kind = if ext.eq_ignore_ascii_case("txt") {
        Kind::Note
    } else if ext.eq_ignore_ascii_case("lsl") {
        Kind::Script
    } else {
        return Err(Error::Extension);
    };
    if !valid_name(name) {
        return Err(Error::InvalidName);
    }
    Ok((name, kind))
}

fn limit(kind: Kind) -> usize {
    match kind {
        Kind::Note => MAX_TEXT_BYTES,
        Kind::Script => MAX_SOURCE,
    }
}

pub(super) fn read(path: &Path, kind: Kind) -> Result<Content, Error> {
    // Resolve the selected link once. Checking the opened handle too avoids
    // accepting a non-regular target if the path changed before open.
    let path = path.canonicalize().map_err(|_| Error::Read)?;
    if !path.metadata().map_err(|_| Error::Read)?.is_file() {
        return Err(Error::Read);
    }
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path)
    };
    #[cfg(not(unix))]
    let file = File::open(path);
    let file: File = file.map_err(|_| Error::Read)?;
    if !file.metadata().map_err(|_| Error::Read)?.is_file() {
        return Err(Error::Read);
    }

    let mut bytes = Vec::new();
    file.take((limit(kind) + 4) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Read)?;
    prepare(bytes, kind)
}

fn prepare(mut bytes: Vec<u8>, kind: Kind) -> Result<Content, Error> {
    if bytes.starts_with(b"\xef\xbb\xbf") {
        bytes.drain(..3);
    }
    if bytes.len() > limit(kind) {
        return Err(Error::TooLarge);
    }
    if bytes.contains(&0) {
        return Err(Error::Nul);
    }
    let mut text = String::from_utf8(bytes).map_err(|_| Error::Utf8)?;
    text.shrink_to_fit();
    match kind {
        Kind::Note => Notecard::default().encode(&text).map(Content::Notecard).map_err(|_| Error::Marker),
        Kind::Script => Ok(Content::Script(text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_final_extension_and_valid_utf8_byte_length() {
        assert_eq!(classify("notes.TXT"), Ok(("notes", Kind::Note)));
        assert_eq!(classify("分岐.v2.LsL"), Ok(("分岐.v2", Kind::Script)));
        assert_eq!(classify("door.lsl.txt"), Ok(("door.lsl", Kind::Note)));
        assert_eq!(classify("door.lua"), Err(Error::Extension));
        for name in [".txt", "  .lsl", "a|b.lsl", "abc\t.lsl"] {
            assert_eq!(classify(name), Err(Error::InvalidName));
        }
        assert!(classify(&format!("{}.lsl", "é".repeat(127))).is_ok());
        assert_eq!(classify(&format!("{}.lsl", "é".repeat(128))), Err(Error::InvalidName));
    }

    #[test]
    fn preserves_bom_free_source_and_keeps_serialized_notecard_looking_text_literal() {
        let literal = "Linden text version 2\r\n{\nLLEmbeddedItems version 1\n{\ncount 0\n}\nText length 0\n}\n\t夏🌏  ";
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend(literal.as_bytes());
        let Ok(Content::Notecard(data)) = prepare(bytes.clone(), Kind::Note) else {
            panic!("literal notecard")
        };
        let note = Notecard::decode(&data).expect("encoded note");
        assert_eq!(note.text, literal);
        assert!(note.attachment_names(literal).is_empty());
        assert!(String::from_utf8_lossy(&data).contains("LLEmbeddedItems version 1\n{\ncount 0\n"));
        assert!(matches!(prepare(bytes, Kind::Script), Ok(Content::Script(source)) if source == literal));
        assert!(matches!(prepare(Vec::new(), Kind::Script), Ok(Content::Script(source)) if source.is_empty()));
        let Ok(Content::Notecard(data)) = prepare(Vec::new(), Kind::Note) else {
            panic!("empty notecard")
        };
        assert_eq!(Notecard::decode(&data).expect("empty note").text, "");
    }

    #[test]
    fn checks_post_bom_byte_limits_and_rejects_invalid_text() {
        for kind in [Kind::Note, Kind::Script] {
            let mut exact = vec![0xef, 0xbb, 0xbf];
            exact.extend("é".repeat(limit(kind) / 2).as_bytes());
            assert!(prepare(exact.clone(), kind).is_ok());
            exact.push(b'!');
            assert!(matches!(prepare(exact, kind), Err(Error::TooLarge)));
            assert!(matches!(prepare(vec![0xff], kind), Err(Error::Utf8)));
            assert!(matches!(prepare(vec![0], kind), Err(Error::Nul)));
        }
        let marker = char::from_u32(aurora_net::inventory::notecard::FIRST_EMBEDDED_CHAR).expect("marker");
        assert!(matches!(prepare(marker.to_string().into_bytes(), Kind::Note), Err(Error::Marker)));
    }

    #[test]
    fn snapshots_regular_files_and_rejects_missing_or_nonregular_paths() {
        let dir = std::env::temp_dir().join(format!("aurora-text-import-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).expect("fixture directory");
        let path = dir.join("script.lsl");
        std::fs::write(&path, b"\xef\xbb\xbf\tline\r\n").expect("fixture file");
        let snapshot = read(&path, Kind::Script).expect("snapshot");
        std::fs::write(&path, b"new version").expect("modify fixture");
        assert!(matches!(snapshot, Content::Script(text) if text == "\tline\r\n"));
        assert!(matches!(read(&dir, Kind::Script), Err(Error::Read)));
        assert!(matches!(read(&dir.join("missing.txt"), Kind::Note), Err(Error::Read)));
        #[cfg(unix)]
        {
            let link = dir.join("dangling.txt");
            std::os::unix::fs::symlink("missing.txt", &link).expect("dangling link");
            assert!(matches!(read(&link, Kind::Note), Err(Error::Read)));
        }
        std::fs::remove_file(&path).expect("remove fixture");
        #[cfg(unix)]
        std::fs::remove_file(dir.join("dangling.txt")).expect("remove link");
        std::fs::remove_dir(dir).expect("remove empty fixture directory");
    }
}
