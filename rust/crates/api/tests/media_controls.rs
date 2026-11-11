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

#[test]
fn smtc_session_shows_track_and_forwards_commands() {
    let (tx, rx) = unbounded();
    let mut controls = MediaControls::new(tx).expect("SMTC available");
    let title = format!("dap smtc test {}", std::process::id());
    controls
        .set_track(Some(&TrackMeta {
            title: title.clone(),
            artist: "Test Artist".into(),
            album: "Test Album".into(),
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
}
