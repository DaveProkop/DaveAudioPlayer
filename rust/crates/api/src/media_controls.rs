//! OS media controls fed straight from the engine: on Windows the System Media Transport
//! Controls (media keys, the "now playing" flyout, the lock screen). Android gets the same
//! from `audio_service` on the Dart side, so other platforms compile to a no-op here.
//!
//! The controls live on the event hub thread (see `api::player`); button presses arrive on
//! WinRT thread-pool threads and go to the engine as ordinary commands.

use crossbeam_channel::Sender;
use dap_engine::{Command, PlayerState};

/// What the controls show about the current track.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackMeta {
    pub title: String,
    pub artist: String,
    pub album: String,
}

/// Playback state as the controls need it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playback {
    pub status: dap_engine::Status,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub rate: f64,
}

impl Playback {
    pub fn from_state(s: &PlayerState) -> Self {
        // Report the position as of now, not as of the state timestamp.
        let mut position_ms = s.position_ms;
        if s.status == dap_engine::Status::Playing {
            position_ms += (s.at.elapsed().as_millis() as f64 * s.rate) as u64;
        }
        Self {
            status: s.status,
            position_ms,
            duration_ms: s.duration_ms,
            rate: s.rate,
        }
    }
}

pub use imp::MediaControls;

#[cfg(windows)]
mod imp {
    use super::*;
    use dap_engine::Status;
    use windows::Foundation::{TimeSpan, TypedEventHandler};
    use windows::Media::Playback::MediaPlayer;
    use windows::Media::{
        MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
        SystemMediaTransportControls, SystemMediaTransportControlsButton,
        SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsTimelineProperties,
    };
    use windows::core::{HSTRING, Ref, Result};

    fn span(ms: u64) -> TimeSpan {
        TimeSpan {
            Duration: ms as i64 * 10_000, // 100 ns units
        }
    }

    /// Windows SMTC session. A `MediaPlayer` with its own command handling switched off is
    /// the documented way for a desktop app without a window handle to get one.
    pub struct MediaControls {
        player: MediaPlayer,
        smtc: SystemMediaTransportControls,
        tokens: [i64; 2],
    }

    impl MediaControls {
        pub fn new(commands: Sender<Command>) -> Result<Self> {
            // Joining the multithreaded apartment is harmless if the thread already has one.
            unsafe {
                let _ = windows::Win32::System::WinRT::RoInitialize(
                    windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
                );
            }
            let player = MediaPlayer::new()?;
            player.CommandManager()?.SetIsEnabled(false)?;
            let smtc = player.SystemMediaTransportControls()?;
            smtc.SetIsEnabled(true)?;
            smtc.SetIsPlayEnabled(true)?;
            smtc.SetIsPauseEnabled(true)?;
            smtc.SetIsNextEnabled(true)?;
            smtc.SetIsPreviousEnabled(true)?;
            smtc.SetPlaybackStatus(MediaPlaybackStatus::Closed)?;

            let tx = commands.clone();
            let buttons = smtc.ButtonPressed(&TypedEventHandler::new(
                move |_, args: Ref<SystemMediaTransportControlsButtonPressedEventArgs>| {
                    let cmd = match args.ok()?.Button()? {
                        SystemMediaTransportControlsButton::Play => Command::Play,
                        SystemMediaTransportControlsButton::Pause => Command::Pause,
                        SystemMediaTransportControlsButton::Stop => Command::Pause,
                        SystemMediaTransportControlsButton::Next => Command::Next,
                        SystemMediaTransportControlsButton::Previous => Command::Previous,
                        _ => return Ok(()),
                    };
                    let _ = tx.send(cmd);
                    Ok(())
                },
            ))?;
            let tx = commands;
            let seeks = smtc.PlaybackPositionChangeRequested(&TypedEventHandler::new(
                move |_, args: Ref<PlaybackPositionChangeRequestedEventArgs>| {
                    let pos = args.ok()?.RequestedPlaybackPosition()?.Duration / 10_000;
                    let _ = tx.send(Command::Seek {
                        position_ms: pos.max(0) as u64,
                    });
                    Ok(())
                },
            ))?;
            Ok(Self {
                player,
                smtc,
                tokens: [buttons, seeks],
            })
        }

        /// Shows a track (`None` clears the display).
        pub fn set_track(&mut self, meta: Option<&TrackMeta>) -> Result<()> {
            let display = self.smtc.DisplayUpdater()?;
            match meta {
                Some(m) => {
                    display.SetType(MediaPlaybackType::Music)?;
                    let music = display.MusicProperties()?;
                    music.SetTitle(&HSTRING::from(&m.title))?;
                    music.SetArtist(&HSTRING::from(&m.artist))?;
                    music.SetAlbumTitle(&HSTRING::from(&m.album))?;
                }
                None => display.ClearAll()?,
            }
            display.Update()
        }

        pub fn set_playback(&mut self, p: &Playback) -> Result<()> {
            self.smtc.SetPlaybackStatus(match p.status {
                Status::Playing => MediaPlaybackStatus::Playing,
                Status::Paused => MediaPlaybackStatus::Paused,
                Status::Stopped => MediaPlaybackStatus::Stopped,
            })?;
            self.smtc.SetPlaybackRate(p.rate)?;
            let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
            let end = p.duration_ms.unwrap_or(0);
            timeline.SetStartTime(span(0))?;
            timeline.SetEndTime(span(end))?;
            timeline.SetMinSeekTime(span(0))?;
            timeline.SetMaxSeekTime(span(end))?;
            let position = match p.duration_ms {
                Some(d) => p.position_ms.min(d),
                None => p.position_ms,
            };
            timeline.SetPosition(span(position))?;
            self.smtc.UpdateTimelineProperties(&timeline)
        }
    }

    impl Drop for MediaControls {
        fn drop(&mut self) {
            let _ = self.smtc.RemoveButtonPressed(self.tokens[0]);
            let _ = self.smtc.RemovePlaybackPositionChangeRequested(self.tokens[1]);
            let _ = self.smtc.SetIsEnabled(false);
            let _ = self.player.Close();
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    /// No OS integration on this platform (Android uses `audio_service` in Dart).
    pub struct MediaControls;

    impl MediaControls {
        pub fn new(_commands: Sender<Command>) -> Result<Self, String> {
            Ok(Self)
        }

        pub fn set_track(&mut self, _meta: Option<&TrackMeta>) -> Result<(), String> {
            Ok(())
        }

        pub fn set_playback(&mut self, _p: &Playback) -> Result<(), String> {
            Ok(())
        }
    }
}
