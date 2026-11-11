//! Windows SMTC round trip: our session must show up in the system's session list with the
//! track we set, and commands from the system (the flyout, media keys) must reach the
//! engine command channel.
#![cfg(windows)]

use std::time::{Duration, Instant};

use crossbeam_channel::unbounded;
use dap_api::media_controls::{MediaControls, Playback, TrackMeta};
use dap_engine::{Command, Status};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
};

fn find_session(title: &str) -> Option<GlobalSystemMediaTransportControlsSession> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
        .ok()?
        .join()
        .ok()?;
    while Instant::now() < deadline {
        for session in manager.GetSessions().ok()? {
            let props = session.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
            if props.Title().ok()? == title {
                return Some(session);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// A 2x2 24-bit BMP, enough for Windows to accept as a thumbnail.
fn write_bmp(path: &std::path::Path) {
    let mut b = Vec::new();
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(54u32 + 16).to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&2i32.to_le_bytes());
    b.extend_from_slice(&2i32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0; 24]);
    // Two rows of two pixels, each row padded to 4 bytes.
    b.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0]);
    std::fs::write(path, b).unwrap();
}

#[test]
fn smtc_session_shows_track_and_forwards_commands() {
    let cover = std::env::temp_dir().join(format!("dap-smtc-cover-{}.bmp", std::process::id()));
    write_bmp(&cover);
    let (tx, rx) = unbounded();
    let mut controls = MediaControls::new(tx).expect("SMTC available");
    let title = format!("dap smtc test {}", std::process::id());
    controls
        .set_track(Some(&TrackMeta {
            title: title.clone(),
            artist: "Test Artist".into(),
            album: "Test Album".into(),
            cover: Some(cover.clone()),
        }))
        .unwrap();
    controls
        .set_playback(&Playback {
            status: Status::Playing,
            position_ms: 1_000,
            duration_ms: Some(60_000),
            rate: 1.0,
        })
        .unwrap();

    let session = find_session(&title).expect("our session is listed by Windows");
    let props = session.TryGetMediaPropertiesAsync().unwrap().join().unwrap();
    assert_eq!(props.Artist().unwrap().to_string(), "Test Artist");
    assert_eq!(props.AlbumTitle().unwrap().to_string(), "Test Album");
    // The thumbnail is published asynchronously; poll for it.
    let deadline = Instant::now() + Duration::from_secs(5);
    let thumbnail = loop {
        let props = session.TryGetMediaPropertiesAsync().unwrap().join().unwrap();
        match props.Thumbnail() {
            Ok(t) => break Ok(t),
            Err(e) if Instant::now() > deadline => break Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    assert!(thumbnail.is_ok(), "the cover reaches Windows: {thumbnail:?}");

    assert!(session.TryPauseAsync().unwrap().join().unwrap());
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(5)),
        Ok(Command::Pause)
    ));

    assert!(session.TrySkipNextAsync().unwrap().join().unwrap());
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(5)),
        Ok(Command::Next)
    ));

    // 100 ns units: 30 s.
    assert!(session
        .TryChangePlaybackPositionAsync(300_000_000)
        .unwrap()
        .join()
        .unwrap());
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(5)),
        Ok(Command::Seek { position_ms: 30_000 })
    ));

    drop(controls);
    let _ = std::fs::remove_file(&cover);
}
