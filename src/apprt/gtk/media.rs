//! Port of `src/apprt/gtk/media.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

use std::path::Path;

use gtk4::prelude::*;

pub fn from_filename(path: &str) -> Option<gtk4::MediaFile> {
    debug_assert!(Path::new(path).is_absolute());
    if let Err(err) = std::fs::File::open(path) {
        log::warn!(target: "gtk_media", "unable to access {path}: {err}");
        return None;
    }
    Some(gtk4::MediaFile::for_filename(path))
}

pub fn from_resource(path: &str) -> Option<gtk4::MediaFile> {
    debug_assert!(Path::new(path).is_absolute());

    match gio::resources_get_info(path, gio::ResourceLookupFlags::NONE) {
        Err(err) => {
            log::warn!(
                target: "gtk_media",
                "failed to find resource {path}: {} {} {}",
                err.domain().as_str(),
                err.code(),
                err.message(),
            );
            return None;
        }
        Ok(_) => {}
    }

    Some(gtk4::MediaFile::for_resource(path))
}

/// Get-or-create a reusable bell MediaFile targeting `path`.
///
/// `current` is the surface's currently-cached MediaFile (or null). If it
/// already targets `path` it is returned unchanged; otherwise it is unref'd and
/// a fresh MediaFile is built for `path`. Returns null (after freeing `current`)
/// if `path` is inaccessible, leaving the caller's slot empty.
///
/// Reusing one MediaFile per surface is what prevents the GStreamer pipeline
/// leak: `gtk.MediaFile.newForFilename` spins up a full pipeline (and, via the
/// GTK4 GStreamer backend's GL sink, gstglcontext/gldisplay-event threads) that
/// is never torn down on the happy path, so allocating one per bell leaked a
/// pipeline + its threads on every ring. See the caller in surface.zig.
pub fn bell_media_file(
    current: Option<gtk4::MediaFile>,
    path: &str,
    required: bool,
) -> Option<gtk4::MediaFile> {
    if let Some(media_file) = current {
        if is_for_path(&media_file, path) {
            return Some(media_file);
        }
        drop(media_file);
    }

    let media_file = from_filename(path)?;

    // If the audio file is marked as required, we'll emit an error if there
    // was a problem playing it. Otherwise there will be silence. We connect
    // this once, here, because the MediaFile is reused across bells.
    //
    // NOTE: we intentionally do NOT connect notify::ended to unref. The
    // MediaFile is owned by the surface and replayed via `seek(0)` for every
    // bell; unref'ing on `ended` is precisely what previously discarded (and
    // leaked) a pipeline per ring.
    if required {
        media_file.connect_notify_local(Some("error"), |media_file, _| {
            media_file_error(media_file.downcast_ref::<gtk4::MediaFile>().unwrap());
        });
    }

    Some(media_file)
}

/// (Re)play `media_file` at `volume`. `seek(0)` rewinds first so that a
/// previously-ended stream plays again; without it playback only ever happens
/// once (see #8957). Safe on a freshly-created stream as well.
pub fn play_bell(media_file: &gtk4::MediaFile, volume: f64) {
    let media_stream = media_file.upcast_ref::<gtk4::MediaStream>();
    media_stream.set_volume(volume);
    media_stream.seek(0);
    media_stream.play();
}

/// Whether `media_file` was created for `path`.
fn is_for_path(media_file: &gtk4::MediaFile, path: &str) -> bool {
    let Some(file) = media_file.file() else { return false };
    let Some(cur) = file.path() else { return false };
    cur.as_os_str() == path
}

fn media_file_error(media_file: &gtk4::MediaFile) {
    let path = media_file.file().and_then(|file| file.path());

    let media_stream = media_file.upcast_ref::<gtk4::MediaStream>();
    let Some(err) = media_stream.error() else { return };
    log::warn!(
        target: "gtk_media",
        "error playing sound from {}: {} {} {}",
        path.as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<<unknown>>".to_owned()),
        err.domain().as_str(),
        err.code(),
        err.message(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bell_media_file_reuses_one_media_file_per_path() {
        // Regression guard for the audio-bell thread leak: each bell must replay a
        // single cached MediaFile, not allocate a fresh GStreamer pipeline (which
        // leaked gstglcontext/gldisplay-event threads) per ring. We assert the
        // reuse contract of bellMediaFile directly; this needs no display and no
        // playback (MediaFile is lazy), only that the path comparison drives reuse.
        //
        // The files need not exist: MediaFile only records the path until
        // played. (As in Zig, `fromFilename` checks that the file is readable,
        // so this returns early unless they do; creating them here would make
        // GTK load its GStreamer backend, which aborts the process when the
        // `playbin` element is missing.)
        let path_a = "/tmp/ghostty-bell-test-a.oga".to_owned();
        let path_b = "/tmp/ghostty-bell-test-b.oga".to_owned();

        let (pa, pb) = (path_a.clone(), path_b.clone());
        crate::apprt::gtk::ext::test_gtk::run(move || {
            let Some(current) = bell_media_file(None, &pa, false) else { return };
            let first = current.clone();
            assert!(is_for_path(&current, &pa));

            // Same path => identical object (the leak regression is rebuilding here).
            let current = bell_media_file(Some(current), &pa, false).unwrap();
            assert_eq!(first, current);
            drop(first);

            // Changed path => rebuilt object targeting the new path (old one freed).
            let Some(current) = bell_media_file(Some(current), &pb, false) else { return };
            assert!(is_for_path(&current, &pb));
            assert!(!is_for_path(&current, &pa));
        });

        // An inaccessible path yields nothing.
        crate::apprt::gtk::ext::test_gtk::run(|| {
            assert!(bell_media_file(None, "/nonexistent/ghostty-bell.oga", true).is_none());
        });
    }
}
