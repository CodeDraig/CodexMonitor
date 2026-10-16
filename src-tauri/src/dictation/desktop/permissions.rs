#[cfg(target_os = "macos")]
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
#[cfg(target_os = "macos")]
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tauri::AppHandle;
#[cfg(target_os = "macos")]
use tokio::sync::oneshot;

#[cfg(target_os = "macos")]
static MIC_PERMISSION_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Checks microphone authorization status on macOS.
#[cfg(target_os = "macos")]
fn check_microphone_authorization() -> Result<AVAuthorizationStatus, String> {
    let media_type = unsafe { AVMediaTypeAudio.ok_or("Failed to get audio media type")? };
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };
    Ok(status)
}

/// Requests microphone permission on macOS.
/// Returns Ok(true) if permission was granted, Ok(false) if denied,
/// or Err with a message if the request failed.
#[cfg(target_os = "macos")]
pub(super) async fn request_microphone_permission(app: &AppHandle) -> Result<bool, String> {
    let status = check_microphone_authorization()?;

    match status {
        AVAuthorizationStatus::Authorized => Ok(true),
        AVAuthorizationStatus::Denied | AVAuthorizationStatus::Restricted => {
            // Some macOS versions report Denied before the first prompt; try once per process.
            if MIC_PERMISSION_REQUESTED.swap(true, Ordering::SeqCst) {
                return Ok(false);
            }
            request_microphone_permission_with_completion(app).await
        }
        AVAuthorizationStatus::NotDetermined | _ => {
            MIC_PERMISSION_REQUESTED.store(true, Ordering::SeqCst);
            request_microphone_permission_with_completion(app).await
        }
    }
}

#[cfg(target_os = "macos")]
fn trigger_microphone_permission_request(tx: oneshot::Sender<Result<bool, String>>) {
    use block2::RcBlock;
    use objc2::runtime::Bool;

    let media_type = match unsafe { AVMediaTypeAudio } {
        Some(media_type) => media_type,
        None => {
            let _ = tx.send(Err("Failed to get audio media type".to_string()));
            return;
        }
    };

    let tx = Arc::new(Mutex::new(Some(tx)));
    let tx_clone = Arc::clone(&tx);
    let block = RcBlock::new(move |granted: Bool| {
        if let Ok(mut guard) = tx_clone.lock() {
            if let Some(sender) = guard.take() {
                let _ = sender.send(Ok(granted.as_bool()));
            }
        }
    });

    unsafe {
        AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &block);
    }
}

#[cfg(target_os = "macos")]
async fn request_microphone_permission_with_completion(app: &AppHandle) -> Result<bool, String> {
    // Trigger the permission request (this shows the system dialog)
    // Ensure we do this on the main thread so the system dialog appears.
    let (tx, rx) = oneshot::channel();
    let app_handle = app.clone();
    app_handle
        .run_on_main_thread(move || {
            trigger_microphone_permission_request(tx);
        })
        .map_err(|error| error.to_string())?;

    match tokio::time::timeout(Duration::from_secs(60), rx).await {
        Ok(Ok(Ok(granted))) => Ok(granted),
        Ok(Ok(Err(error))) => Err(error),
        Ok(Err(_)) => Err("Failed to request microphone permission.".to_string()),
        Err(_) => Err("Microphone permission request timed out.".to_string()),
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) async fn request_microphone_permission(_app: &AppHandle) -> Result<bool, String> {
    // On non-macOS platforms, assume permission is granted
    // (Linux doesn't have the same permission model)
    Ok(true)
}
