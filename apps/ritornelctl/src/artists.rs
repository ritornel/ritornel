//! `ritornelctl artists pictures <folder>`: takes each artist's portrait
//! from a picture folder, as `scan` takes music from a folder. See "Artist
//! pictures" in docs/plan.md.
//!
//! The folder becomes a named root on the server, which only ever reads
//! it. Running it again takes what the folder chooses now, and drops what
//! it no longer chooses.

use std::io::Write;
use std::time::Duration;

use ritornel_client::contract::methods::ArtistsImportPictures;
use ritornel_client::contract::{ImportPicturesParams, PicturesImported};
use ritornel_client::local;

use crate::cli::{Error, PicturesArgs};
use crate::server;

/// How long the import may take: it reads every portrait the folder chose.
const IMPORT_WAIT: Duration = Duration::from_secs(300);

pub(crate) fn pictures(
    args: &PicturesArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    // The server is given the folder's real path, as `scan` gives it.
    let folder = std::fs::canonicalize(&args.folder).map_err(Error::Folder)?;
    let (Some(path), true) = (folder.to_str(), folder.is_dir()) else {
        return Err(Error::Usage(
            "artists pictures needs a folder whose path is UTF-8",
        ));
    };
    let name = match &args.name {
        Some(name) => name.clone(),
        None => folder
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("artists")
            .to_owned(),
    };
    let key = ritornel_client::idempotency_key()?;
    let params = ImportPicturesParams {
        idempotency_key: key,
        name,
        path: path.to_owned(),
    };
    let imported = server::ask_for(&args.server, env, Some(IMPORT_WAIT), async |client| {
        client.call::<ArtistsImportPictures>(&params).await
    })?;
    server::print(
        &args.server,
        out,
        &imported,
        serde_json::to_string_pretty,
        render,
    )
}

fn render(imported: &PicturesImported) -> String {
    server::facts(&[
        ("Artists", imported.artists.to_string()),
        ("By MusicBrainz ID", imported.by_mbid.to_string()),
        ("By name", imported.by_name.to_string()),
        ("No portrait", imported.without.to_string()),
        ("Unreadable", imported.unreadable.to_string()),
        ("Dropped", imported.dropped.to_string()),
        ("Names alike", imported.ambiguous.to_string()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornel_client::contract::{Id, Receipt, ReceiptStatus};

    #[test]
    fn what_was_imported_is_one_fact_a_line() {
        let imported = PicturesImported {
            receipt: Receipt {
                command_id: Id::from_bytes([1; 16]),
                status: ReceiptStatus::Done,
                revision: Some(4),
            },
            artists: 487,
            by_mbid: 380,
            by_name: 20,
            without: 87,
            unreadable: 1,
            dropped: 2,
            ambiguous: 3,
        };
        assert_eq!(
            render(&imported),
            "Artists           487\n\
             By MusicBrainz ID 380\n\
             By name           20\n\
             No portrait       87\n\
             Unreadable        1\n\
             Dropped           2\n\
             Names alike       3\n"
        );
    }
}
