//! Composer attachment encoding runs outside egui's drawing pass.
use super::*;
use base64::Engine;

const MAX_ATTACHMENT_BYTES: u64 = 8 * 1024 * 1024;

pub(super) fn encode_files(
    paths: Vec<PathBuf>,
    capabilities: Option<Value>,
) -> Result<Vec<Value>, String> {
    paths.into_iter().map(|path| {
        let metadata = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if metadata.len() > MAX_ATTACHMENT_BYTES { return Err(format!("{} exceeds the 8 MiB attachment limit",path.display())); }
        let mime = match path.extension().and_then(|v|v.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
            "png"=>"image/png", "jpg"|"jpeg"=>"image/jpeg", "gif"=>"image/gif", "webp"=>"image/webp",
            "wav"=>"audio/wav", "mp3"=>"audio/mpeg", "ogg"=>"audio/ogg",
            "txt"|"md"|"rs"|"py"|"toml"=>"text/plain", "json"=>"application/json", _=>"application/octet-stream",
        };
        allowed(mime,capabilities.as_ref())?;
        let bytes=std::fs::read(&path).map_err(|e|e.to_string())?;
        let data=base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(if mime.starts_with("image/") || mime.starts_with("audio/") {
            json!({"type":if mime.starts_with("image/"){ "image" }else{"audio"},"mimeType":mime,"data":data})
        } else {
            let mut uri=reqwest::Url::parse("attachment:///").map_err(|e|e.to_string())?;
            uri.set_path(&path.file_name().unwrap_or_default().to_string_lossy());
            json!({"type":"resource","resource":{"uri":uri.to_string(),"mimeType":mime,"blob":data}})
        })
    }).collect()
}
fn allowed(mime: &str, capabilities: Option<&Value>) -> Result<(), String> {
    let Some(caps) = capabilities else {
        return if mime.starts_with("image/") {
            Ok(())
        } else {
            Err("Codex accepts image attachments; use @ to reference other workspace files.".into())
        };
    };
    let capability = if mime.starts_with("image/") {
        "images"
    } else if mime.starts_with("audio/") {
        "audio"
    } else {
        "embeddedContext"
    };
    if caps[capability] == true {
        Ok(())
    } else {
        Err(format!(
            "The selected agent does not advertise support for {mime} attachments"
        ))
    }
}

impl NativeApp {
    pub(super) fn attach_clipboard_image(
        &self,
        key: ConversationKey,
        width: usize,
        height: usize,
        rgba: Vec<u8>,
        ctx: &egui::Context,
    ) {
        let tx = self.background_tx.clone();
        let wake = ctx.clone();
        let epoch = self.connection_epoch;
        self.runtime.handle.spawn_blocking(move||{
            let result=(|| {
                let image=image::RgbaImage::from_raw(width.try_into().map_err(|_|"Clipboard image width is too large")?,height.try_into().map_err(|_|"Clipboard image height is too large")?,rgba).ok_or("Clipboard image dimensions do not match its pixels")?;
                let mut bytes=std::io::Cursor::new(Vec::new());
                image::DynamicImage::ImageRgba8(image).write_to(&mut bytes,image::ImageFormat::Png).map_err(|e|e.to_string())?;
                if bytes.get_ref().len() as u64>MAX_ATTACHMENT_BYTES {return Err("Clipboard image exceeds the 8 MiB attachment limit".into());}
                Ok(vec![json!({"type":"image","mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())})])
            })();
            let _=tx.send(BackgroundResult::Attachments(key,epoch,result));wake.request_repaint();
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attachments_require_the_selected_runtime_capability() {
        assert!(allowed("image/png", None).is_ok());
        assert!(allowed("application/pdf", None).is_err());
        assert!(allowed("image/png", Some(&json!({"audio":true}))).is_err());
        assert!(allowed("audio/wav", Some(&json!({"audio":true}))).is_ok());
        assert!(allowed("application/pdf", Some(&json!({"embeddedContext":true}))).is_ok());
    }
}
