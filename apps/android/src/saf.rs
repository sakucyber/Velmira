use std::path::{Path, PathBuf};

use android_activity::AndroidApp;
use jni::objects::{JObject, JString};
use jni::{jni_sig, jni_str, Env, JValue, JavaVM};

pub enum ImportOutcome {
    File(PathBuf),
    Cancelled,
    Error(String),
}

pub fn begin(app: &AndroidApp) -> Result<(), String> {
    with_activity(app, |env, activity| {
        env.call_method(activity, jni_str!("beginBookImport"), jni_sig!("()V"), &[])?
            .v()
    })
}

pub fn take(app: &AndroidApp) -> Result<Option<ImportOutcome>, String> {
    let response = with_activity(app, |env, activity| {
        let result = env
            .call_method(
                activity,
                jni_str!("takeBookImport"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        let result = env.cast_local::<JString>(result)?;
        if result.is_null() {
            return Ok(None);
        }
        result.try_to_string(env).map(Some)
    })?;

    let Some(response) = response else {
        return Ok(None);
    };
    if response == "cancel" {
        return Ok(Some(ImportOutcome::Cancelled));
    }
    match response.split_once('\n') {
        Some(("ok", path)) if Path::new(path).is_absolute() => {
            Ok(Some(ImportOutcome::File(PathBuf::from(path))))
        }
        Some(("error", message)) => Ok(Some(ImportOutcome::Error(message.to_owned()))),
        _ => Err("Unexpected response from the Android document picker.".into()),
    }
}

pub fn remove_import(path: &Path) {
    let _ = std::fs::remove_file(path);
    if let Some(directory) = path.parent() {
        let _ = std::fs::remove_dir(directory);
    }
}

pub fn set_handles_back(app: &AndroidApp, enabled: bool) -> Result<(), String> {
    with_activity(app, |env, activity| {
        env.call_method(
            activity,
            jni_str!("setHandlesBack"),
            jni_sig!("(Z)V"),
            &[JValue::Bool(enabled)],
        )?
        .v()
    })
}

pub fn safe_insets(app: &AndroidApp) -> Result<Option<[u16; 4]>, String> {
    let packed = with_activity(app, |env, activity| {
        env.call_method(activity, jni_str!("getSafeInsets"), jni_sig!("()J"), &[])?
            .j()
    })?;
    Ok((packed < 0).then(|| {
        let pixels = packed as u64;
        [
            pixels as u16,
            (pixels >> 16) as u16,
            (pixels >> 32) as u16,
            ((pixels >> 48) & 0x7fff) as u16,
        ]
    }))
}

pub fn set_dark_system_bar_icons(app: &AndroidApp, dark: bool) -> Result<(), String> {
    with_activity(app, |env, activity| {
        env.call_method(
            activity,
            jni_str!("setDarkSystemBarIcons"),
            jni_sig!("(Z)V"),
            &[JValue::Bool(dark)],
        )?
        .v()
    })
}

pub fn edit_field(
    app: &AndroidApp,
    title: &str,
    value: &str,
    kind: i32,
    limit: i32,
) -> Result<(), String> {
    with_activity(app, |env, activity| {
        let title = env.new_string(title)?;
        let value = env.new_string(value)?;
        env.call_method(
            activity,
            jni_str!("editField"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;II)V"),
            &[
                JValue::Object(title.as_ref()),
                JValue::Object(value.as_ref()),
                JValue::Int(kind),
                JValue::Int(limit),
            ],
        )?
        .v()
    })
}

pub fn take_field(app: &AndroidApp) -> Result<Option<Option<String>>, String> {
    with_activity(app, |env, activity| {
        let text = env
            .call_method(
                activity,
                jni_str!("takeField"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        let text = env.cast_local::<JString>(text)?;
        if text.is_null() {
            return Ok(None);
        }
        let value = text.try_to_string(env)?;
        Ok(Some(value.strip_prefix("ok\n").map(str::to_owned)))
    })
}

pub fn take_reader_back(app: &AndroidApp) -> Result<bool, String> {
    with_activity(app, |env, activity| {
        env.call_method(activity, jni_str!("takeReaderBack"), jni_sig!("()Z"), &[])?
            .z()
    })
}

pub fn background_task(app: &AndroidApp) -> Result<(), String> {
    with_activity(app, |env, activity| {
        env.call_method(activity, jni_str!("backgroundTask"), jni_sig!("()V"), &[])?
            .v()
    })
}

pub fn copy_text(app: &AndroidApp, text: &str) -> Result<(), String> {
    with_activity(app, |env, activity| {
        let text = env.new_string(text)?;
        env.call_method(
            activity,
            jni_str!("copyReaderText"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(text.as_ref())],
        )?
        .v()
    })
}

fn with_activity<T>(
    app: &AndroidApp,
    call: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> Result<T, String> {
    let vm = JavaVM::singleton().map_err(|err| err.to_string())?;
    let raw_activity = app.activity_as_ptr().cast();
    vm.attach_current_thread(|env| {
        // The borrowed global reference remains valid while AndroidApp is alive.
        let activity = unsafe { env.as_cast_raw::<JObject>(&raw_activity)? };
        call(env, &activity)
    })
    .map_err(|err| format!("Android Activity call failed: {err}"))
}
