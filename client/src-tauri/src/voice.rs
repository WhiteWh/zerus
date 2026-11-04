//! Voice messages (the `media` feature): one voice driver per composer key,
//! holding the composer's state machine and its microphone capture. The
//! webview sends inputs and receives [`Out`] on its channel: the controls'
//! state, the level, questions, accepted messages and transcripts to
//! insert. Recordings stay in Rust.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;
use zerus_client_core::default_hgs;
use zerus_client_core::drafts::DraftStore;
use zerus_client_core::voice_composer::{Inserted, Preferences};
use zerus_client_core::voice_driver::{Input, Out, VoiceDriver};
use zerus_client_media::{microphones, video_reason, MediaCapture};

// Debug-only synthetic capture for the private built-app campaign. It cannot
// enter a release build and also requires the existing replay/fake-hgs smoke gate.
#[cfg(debug_assertions)]
mod fixture;

fn capture() -> Box<dyn zerus_client_core::voice_driver::Capture> {
    #[cfg(debug_assertions)]
    if let Some(capture) = fixture::capture() {
        return capture;
    }
    Box::new(MediaCapture::default())
}

/// Voice drivers open at once; a composer closes its own when it goes.
pub const MAX_DRIVERS: usize = 32;
const TOO_MANY: &str = "Too many voice composers are open. Close one and try again.";
const REPLAY_NEEDS_HGS: &str = "Voice in replay mode needs ZERUS_CLIENT_HGS.";

/// The open voice drivers, by composer key.
pub struct Voices {
    hgs: PathBuf,
    drafts: Arc<DraftStore>,
    drivers: Mutex<HashMap<String, OpenVoice>>,
}

struct OpenVoice {
    driver: VoiceDriver,
    active: Arc<AtomicBool>,
    visible: bool,
}

impl Voices {
    /// `drafts` is the app's one draft store, shared with the composers.
    pub fn new(drafts: Arc<DraftStore>) -> Voices {
        Voices {
            hgs: default_hgs(),
            drafts,
            drivers: Mutex::new(HashMap::new()),
        }
    }

    /// At exit: closes every driver, cancelling recordings under way and
    /// waiting until their draft saves are queued. Blocks, bounded.
    pub fn close_all(&self) {
        let drivers: Vec<OpenVoice> = self
            .drivers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .map(|(_, driver)| driver)
            .collect();
        for driver in drivers {
            driver.active.store(false, Ordering::SeqCst);
            driver.driver.close();
        }
    }
}

/// An input from the webview; one per user action, so its size does not
/// matter.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum VoiceInput {
    Update { input_visible: bool },
    Press,
    Stop { forget_send: bool },
    Send,
    Discard,
    Refresh,
    ChooseProvider { index: usize },
    Confirm { ticket: u64, confirmed: bool },
    Inserted { token: u64, inserted: InsertedAs },
    Playing { playing: bool },
}

/// How a transcript's insertion ended.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertedAs {
    /// The draft changed; nothing was inserted.
    Refused,
    Saved,
    NotSaved,
}

impl VoiceInput {
    fn driver(self) -> Option<Input> {
        Some(match self {
            VoiceInput::Update { .. } => return None,
            VoiceInput::Press => Input::Press,
            VoiceInput::Stop { forget_send } => Input::Stop { forget_send },
            VoiceInput::Send => Input::Send,
            VoiceInput::Discard => Input::Discard,
            VoiceInput::Refresh => Input::Refresh,
            VoiceInput::ChooseProvider { index } => Input::ChooseProvider(index),
            VoiceInput::Confirm { ticket, confirmed } => Input::Confirm { ticket, confirmed },
            VoiceInput::Inserted { token, inserted } => Input::Inserted {
                token,
                inserted: match inserted {
                    InsertedAs::Refused => Inserted::Refused,
                    InsertedAs::Saved => Inserted::Saved(true),
                    InsertedAs::NotSaved => Inserted::Saved(false),
                },
            },
            VoiceInput::Playing { playing } => Input::Playing(playing),
        })
    }
}

/// Opens the voice driver of a composer, replacing an earlier one for the
/// same key; its outputs go to `events`, a round video's preview frames to
/// `frames` as raw JPEG bytes (the latest only, at most 15 a second). A
/// replay reaches only an explicit `ZERUS_CLIENT_HGS`, never the real CLI.
#[tauri::command]
pub async fn voice_open(
    state: State<'_, Voices>,
    composers: State<'_, crate::composer::Composers>,
    key: String,
    events: Channel<Out>,
    frames: Channel<InvokeResponseBody>,
) -> Result<(), String> {
    if crate::actions::blocked() {
        return Err(REPLAY_NEEDS_HGS.into());
    }
    {
        let drivers = state.drivers.lock().unwrap_or_else(PoisonError::into_inner);
        if !drivers.contains_key(&key) && drivers.len() >= MAX_DRIVERS {
            return Err(TOO_MANY.into());
        }
    }
    let active = Arc::new(AtomicBool::new(true));
    let live = Arc::clone(&active);
    let (owner, composers) = (key.clone(), composers.inner().clone());
    let driver = VoiceDriver::start(
        state.hgs.clone(),
        Arc::clone(&state.drafts),
        capture(),
        Box::new(move |out| {
            if !live.load(Ordering::SeqCst) {
                return;
            }
            match &out {
                Out::Shown { shown } => {
                    #[cfg(debug_assertions)]
                    fixture::note(
                        "shown",
                        serde_json::json!({"state":shown.state,"key":shown.key,"id":shown.id,"recording":shown.recording}),
                    );
                    composers.voice_recording(&owner, shown.recording.then_some(shown.key.as_str()))
                }
                Out::Message { key, message } => composers.voice_message(key, message),
                _ => {}
            }
            let _ = events.send(out);
        }),
        Box::new(move |jpeg| {
            let _ = frames.send(InvokeResponseBody::Raw(jpeg));
        }),
        || uuid::Uuid::new_v4().to_string(),
    );
    let previous = state
        .drivers
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            key,
            OpenVoice {
                driver,
                active,
                visible: true,
            },
        );
    // Dropping never waits: the replaced driver's thread ends on its own.
    if let Some(previous) = &previous {
        previous.active.store(false, Ordering::SeqCst);
    }
    drop(previous);
    Ok(())
}

/// An input for an open voice driver.
#[tauri::command]
pub async fn voice_input(
    state: State<'_, Voices>,
    composers: State<'_, crate::composer::Composers>,
    settings: State<'_, crate::settings::Store>,
    key: String,
    handle: u64,
    input: VoiceInput,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    fixture::note("input", serde_json::to_value(&input).unwrap_or_default());
    let mut drivers = state.drivers.lock().unwrap_or_else(PoisonError::into_inner);
    let open = drivers
        .get_mut(&key)
        .ok_or("The voice composer is not open.")?;
    // Stopping private capture/playback or rejecting a dialog never needs a
    // current session, and must remain possible while the user navigates away.
    if matches!(
        &input,
        VoiceInput::Stop { .. }
            | VoiceInput::Playing { .. }
            | VoiceInput::Confirm {
                confirmed: false,
                ..
            }
    ) {
        if let Some(input) = input.driver() {
            open.driver.input(input);
        }
        return Ok(());
    }
    let Some(target) = composers.voice_target(handle) else {
        open.driver.input(Input::Stop { forget_send: true });
        return Err("The voice composer is not current.".into());
    };
    if let VoiceInput::Update { input_visible } = &input {
        open.visible = *input_visible;
    }
    let text = |key| settings.0.get(key).as_str().unwrap_or_default().to_owned();
    // Every user action adopts current native identities and settings first.
    // The renderer cannot keep a stale conversation available or select a provider implicitly.
    open.driver.input(Input::Update {
        target,
        input_visible: open.visible,
        preferences: Preferences {
            video_mode: settings.0.flag("speech/video_mode"),
            preview_before_send: settings.0.flag("speech/preview_before_send"),
            microphone: text("speech/microphone"),
            machine_id: text("speech/machine_id"),
            profile_id: text("speech/profile"),
        },
    });
    if let Some(input) = input.driver() {
        open.driver.input(input);
    }
    Ok(())
}

/// Closes a composer's voice driver without waiting; a recording under way
/// is cancelled, a saved one stays in its draft.
#[tauri::command]
pub async fn voice_close(
    state: State<'_, Voices>,
    composers: State<'_, crate::composer::Composers>,
    key: String,
) -> Result<(), String> {
    let driver = state
        .drivers
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&key);
    if let Some(driver) = &driver {
        driver.active.store(false, Ordering::SeqCst);
    }
    composers.voice_recording(&key, None);
    drop(driver);
    Ok(())
}

/// A microphone Settings > Voice input offers.
#[derive(Clone, Debug, Serialize)]
pub struct MicrophoneRow {
    pub id: String,
    pub name: String,
    pub default: bool,
}

/// This computer's microphones; reading the devices can take a moment.
#[tauri::command]
pub async fn voice_microphones() -> Result<Vec<MicrophoneRow>, String> {
    let found = tauri::async_runtime::spawn_blocking(microphones)
        .await
        .map_err(|error| error.to_string())?;
    Ok(found
        .into_iter()
        .map(|microphone| MicrophoneRow {
            id: microphone.id,
            name: microphone.name,
            default: microphone.default,
        })
        .collect())
}

/// Whether this build records round video here, and why not.
#[derive(Clone, Debug, Serialize)]
pub struct VideoSupport {
    pub available: bool,
    /// Video's tooltip while unavailable: a missing encoder, or a build
    /// without video.
    pub reason: Option<String>,
}

/// Round video's availability: Video shows only while available. One
/// probe per process; the first can take a moment.
#[tauri::command]
pub async fn voice_video() -> Result<VideoSupport, String> {
    #[cfg(debug_assertions)]
    if fixture::enabled() {
        return Ok(VideoSupport {
            available: true,
            reason: None,
        });
    }
    let reason = tauri::async_runtime::spawn_blocking(video_reason)
        .await
        .map_err(|error| error.to_string())?;
    Ok(VideoSupport {
        available: reason.is_none(),
        reason,
    })
}

/// Read one exact saved recording as bounded raw IPC bytes for playback.
/// It stays out of JSON and does not submit, mutate or recover the draft.
#[tauri::command]
pub async fn voice_preview(
    state: State<'_, Voices>,
    key: String,
    id: String,
) -> Result<tauri::ipc::Response, String> {
    let (send, receive) = std::sync::mpsc::channel();
    {
        let drivers = state.drivers.lock().unwrap_or_else(PoisonError::into_inner);
        let open = drivers.get(&key).ok_or("The voice composer is not open.")?;
        open.driver.preview(id, move |preview| {
            let _ = send.send(preview);
        });
    }
    tauri::async_runtime::spawn_blocking(move || {
        let preview = receive
            .recv_timeout(std::time::Duration::from_secs(5))
            .ok()
            .flatten()
            .ok_or("The saved recording changed. Open it again.")?;
        let bytes = if preview.video.is_empty() {
            zerus_client_core::voice::wav(&preview.pcm)
        } else {
            preview.video
        };
        if bytes.is_empty() || bytes.len() > 10 * 1024 * 1024 {
            return Err("Could not prepare the recording for playback.".into());
        }
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(|error| error.to_string())?
}

fn permission_platform() -> zerus_client_core::voice_composer::Platform {
    use zerus_client_core::voice_composer::Platform;
    if cfg!(target_os = "macos") {
        Platform::Macos
    } else if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

#[tauri::command]
pub fn voice_permission() -> serde_json::Value {
    serde_json::json!({
        "text": zerus_client_core::voice_composer::permission_text(permission_platform()),
        "can_open": zerus_client_core::voice_composer::microphone_settings_url(permission_platform()).is_some(),
    })
}

#[tauri::command]
pub async fn voice_open_settings(app: tauri::AppHandle) -> Result<(), String> {
    let url = zerus_client_core::voice_composer::microphone_settings_url(permission_platform())
        .ok_or("Open your desktop's Settings and check microphone permissions for Zerus.")?;
    crate::external_open::url(&app, url)
}
