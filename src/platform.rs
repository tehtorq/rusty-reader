//! The few places where a phone differs from a desktop: where files live, how books
//! arrive, and which parts of the screen the system keeps for itself.

use std::path::{Path, PathBuf};

use eframe::egui;

/// Where settings and reading positions are kept.
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    {
        android::app()?.internal_data_path()
    }
    #[cfg(not(target_os = "android"))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("rusty-reader"))
    }
}

/// Ask the person for books. On Android the picker answers later, through [`take_picked_books`].
pub fn pick_books() -> Vec<PathBuf> {
    #[cfg(target_os = "android")]
    {
        android::pick_book();
        Vec::new()
    }
    #[cfg(not(target_os = "android"))]
    {
        rfd::FileDialog::new().add_filter("EPUB books", &["epub"]).pick_files().unwrap_or_default()
    }
}

/// Books handed to the app from outside (the Android picker, "Open with" or "Share"), a batch
/// at a time.
pub fn take_picked_books() -> Vec<PathBuf> {
    #[cfg(target_os = "android")]
    {
        android::take_picked_books()
    }
    #[cfg(not(target_os = "android"))]
    {
        Vec::new()
    }
}

/// Leave the app, as the back gesture does from its first screen.
/// The display's refresh rate, where the platform will say.
pub fn refresh_rate() -> Option<f32> {
    #[cfg(target_os = "android")]
    {
        android::refresh_rate()
    }
    #[cfg(not(target_os = "android"))]
    {
        None
    }
}

/// A short tap of the phone's vibration motor, to confirm a long press did something.
pub fn buzz() {
    #[cfg(target_os = "android")]
    android::buzz();
}

pub fn leave(ctx: &egui::Context) {
    #[cfg(target_os = "android")]
    {
        // Like any Android app: step back to the home screen, still running, instead of quitting.
        let _ = ctx;
        android::move_to_back();
    }
    #[cfg(not(target_os = "android"))]
    {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// A folder the app watches for new books, on platforms without a file picker.
pub fn books_dir() -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    {
        let dir = android::app()?.external_data_path()?.join("Books");
        let _ = std::fs::create_dir_all(&dir);
        Some(dir)
    }
    #[cfg(not(target_os = "android"))]
    {
        None
    }
}

/// Whether this file is the app's own copy, safe to delete when the book leaves the shelf.
pub fn is_managed(path: &Path) -> bool {
    books_dir().is_some_and(|dir| path.starts_with(dir))
}

/// Every EPUB in the books folder.
pub fn found_books() -> Vec<PathBuf> {
    let Some(dir) = books_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut books: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| is_epub(p)).collect();
    books.sort();
    books
}

pub fn is_epub(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("epub"))
}

/// Space taken by the status bar, gesture bar and camera cut-out, for egui to keep clear of.
pub fn safe_area(ctx: &egui::Context) -> Option<egui::SafeAreaInsets> {
    #[cfg(target_os = "android")]
    {
        android::safe_area(ctx)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = ctx;
        None
    }
}

#[cfg(target_os = "android")]
pub mod android {
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use eframe::egui;
    use std::ffi::c_void;

    use jni::objects::{JClass, JObject, JObjectArray, JString, JValue};
    use jni::sys::{jfloat, jint, jlong};
    use jni::{JNIEnv, JavaVM, NativeMethod};

    use crate::speech::{self, Control, Engine, Heard, State, Voice, VoiceInfo};
    use winit::platform::android::activity::AndroidApp;

    static APP: OnceLock<AndroidApp> = OnceLock::new();

    pub fn set_app(app: AndroidApp) {
        let _ = APP.set(app);
        register_natives();
    }

    /// Give Java's `Narration.heard` and `Narration.control` their Rust bodies. (Java can't
    /// find them by name: the library was loaded by NativeActivity, not `System.loadLibrary`.)
    fn register_natives() {
        with_activity(|env, activity| {
            let loader =
                env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
            let name = env.new_string("com.rustyreader.Narration")?;
            let class = env
                .call_method(
                    &loader,
                    "loadClass",
                    "(Ljava/lang/String;)Ljava/lang/Class;",
                    &[(&name).into()],
                )?
                .l()?;
            let methods = [
                NativeMethod {
                    name: "heard".into(),
                    sig: "(IJ)V".into(),
                    fn_ptr: native_heard as *mut c_void,
                },
                NativeMethod {
                    name: "control".into(),
                    sig: "(I)V".into(),
                    fn_ptr: native_control as *mut c_void,
                },
                NativeMethod {
                    name: "progress".into(),
                    sig: "(Ljava/lang/String;F)V".into(),
                    fn_ptr: native_progress as *mut c_void,
                },
                NativeMethod {
                    name: "failure".into(),
                    sig: "(Ljava/lang/String;)V".into(),
                    fn_ptr: native_failure as *mut c_void,
                },
            ];
            env.register_native_methods(JClass::from(class), &methods)
        });
    }

    extern "system" fn native_heard(_env: JNIEnv, _class: JClass, kind: jint, id: jlong) {
        let id = id as u64;
        speech::heard(match kind {
            0 => Heard::Started(id),
            1 => Heard::Done(id),
            _ => Heard::Failed(id),
        });
    }

    extern "system" fn native_progress(mut env: JNIEnv, _class: JClass, voice: JString, fraction: jfloat) {
        if let Ok(voice) = env.get_string(&voice) {
            speech::download_progress(&String::from(voice), fraction);
        }
    }

    extern "system" fn native_failure(mut env: JNIEnv, _class: JClass, message: JString) {
        if let Ok(message) = env.get_string(&message) {
            let message = String::from(message);
            log::error!("{message}");
            speech::failure(&message);
        }
    }

    extern "system" fn native_control(_env: JNIEnv, _class: JClass, action: jint) {
        speech::control(match action {
            0 => Control::Play,
            1 => Control::Pause,
            2 => Control::Toggle,
            4 => Control::Next,
            5 => Control::Previous,
            _ => Control::Stop,
        });
    }

    /// Android's own text-to-speech, through `Narration.java`.
    #[derive(Default)]
    pub struct SystemVoice;

    impl Voice for SystemVoice {
        fn speak(&mut self, id: u64, text: &str, paragraph_end: bool) {
            with_activity(|env, activity| {
                let text = env.new_string(text)?;
                env.call_method(
                    activity,
                    "speak",
                    "(JLjava/lang/String;Z)V",
                    &[JValue::Long(id as i64), (&text).into(), JValue::Bool(paragraph_end.into())],
                )
                .map(drop)
            });
        }

        fn stop(&mut self) {
            with_activity(|env, activity| env.call_method(activity, "silence", "()V", &[]).map(drop));
        }

        fn set_rate(&mut self, rate: f32) {
            with_activity(|env, activity| {
                env.call_method(activity, "speechRate", "(F)V", &[JValue::Float(rate)]).map(drop)
            });
        }

        fn set_language(&mut self, language: &str) {
            with_activity(|env, activity| {
                let tag = env.new_string(language)?;
                env.call_method(activity, "speechLanguage", "(Ljava/lang/String;)V", &[(&tag).into()])
                    .map(drop)
            });
        }

        fn set_voice(&mut self, voice: Option<&str>) {
            with_activity(|env, activity| {
                let name = match voice {
                    Some(v) => env.new_string(v)?.into(),
                    None => JObject::null(),
                };
                env.call_method(activity, "speechVoice", "(Ljava/lang/String;)V", &[(&name).into()]).map(drop)
            });
        }

        fn current_voice(&mut self) -> Option<String> {
            with_activity(|env, activity| {
                let name =
                    env.call_method(activity, "speechCurrentVoice", "()Ljava/lang/String;", &[])?.l()?;
                if name.is_null() {
                    return Ok(None);
                }
                Ok(Some(String::from(env.get_string(&JString::from(name))?)))
            })
            .flatten()
        }

        fn engines(&mut self) -> Vec<Engine> {
            with_activity(|env, activity| {
                let list = env.call_method(activity, "speechEngines", "()[Ljava/lang/String;", &[])?.l()?;
                let list = JObjectArray::from(list);
                let mut engines = Vec::new();
                for i in 0..env.get_array_length(&list)? {
                    let entry = JString::from(env.get_object_array_element(&list, i)?);
                    let entry = String::from(env.get_string(&entry)?);
                    if let Some((id, name)) = entry.split_once('\t') {
                        engines.push(Engine { id: id.into(), name: name.into() });
                    }
                }
                Ok(engines)
            })
            .unwrap_or_default()
        }

        fn set_engine(&mut self, engine: Option<&str>) {
            with_activity(|env, activity| {
                let name = match engine {
                    Some(e) => env.new_string(e)?.into(),
                    None => JObject::null(),
                };
                env.call_method(activity, "speechEngine", "(Ljava/lang/String;)V", &[(&name).into()])
                    .map(drop)
            });
        }

        fn voices(&mut self) -> Vec<VoiceInfo> {
            with_activity(|env, activity| {
                let list = env.call_method(activity, "speechVoices", "()[Ljava/lang/String;", &[])?.l()?;
                let list = JObjectArray::from(list);
                let mut voices = Vec::new();
                for i in 0..env.get_array_length(&list)? {
                    let entry = JString::from(env.get_object_array_element(&list, i)?);
                    let entry = String::from(env.get_string(&entry)?);
                    if let [id, language, name] = entry.splitn(3, '\t').collect::<Vec<_>>()[..] {
                        voices.push(VoiceInfo {
                            id: id.into(),
                            language: language.into(),
                            name: name.into(),
                        });
                    }
                }
                Ok(voices)
            })
            .unwrap_or_default()
        }

        fn now_playing(&mut self, state: State, title: &str, author: Option<&str>) {
            let state = match state {
                State::Idle => 0,
                State::Playing => 1,
                State::Paused => 2,
            };
            with_activity(|env, activity| {
                let title = env.new_string(title)?;
                let author = env.new_string(author.unwrap_or_default())?;
                env.call_method(
                    activity,
                    "nowPlaying",
                    "(ILjava/lang/String;Ljava/lang/String;)V",
                    &[JValue::Int(state), (&title).into(), (&author).into()],
                )
                .map(drop)
            });
        }
    }

    pub fn app() -> Option<&'static AndroidApp> {
        APP.get()
    }

    /// Insets change rarely (rotation, the cut-out moving sides), so ask Java only now and then.
    pub fn safe_area(ctx: &egui::Context) -> Option<egui::SafeAreaInsets> {
        static CACHE: Mutex<Option<(Instant, [i32; 4])>> = Mutex::new(None);
        let mut cache = CACHE.lock().ok()?;
        let fresh = cache.is_some_and(|(at, _)| at.elapsed() < Duration::from_millis(500));
        if !fresh {
            let insets = system_insets().or(cache.map(|(_, i)| i))?;
            *cache = Some((Instant::now(), insets));
        }
        let [left, top, right, bottom] = (*cache)?.1;
        let ppp = ctx.pixels_per_point();
        Some(egui::SafeAreaInsets(egui::epaint::MarginF32 {
            left: left as f32 / ppp,
            right: right as f32 / ppp,
            top: top as f32 / ppp,
            bottom: bottom as f32 / ppp,
        }))
    }

    /// Ask `ReaderActivity` to show the system document picker; the books arrive later
    /// through [`take_picked_books`].
    pub fn pick_book() {
        with_activity(|env, activity| env.call_method(activity, "pickBook", "()V", &[]).map(drop));
    }

    pub fn refresh_rate() -> Option<f32> {
        with_activity(|env, activity| env.call_method(activity, "refreshRate", "()F", &[])?.f())
            .filter(|&hz| hz > 0.0)
    }

    pub fn buzz() {
        with_activity(|env, activity| env.call_method(activity, "buzz", "()V", &[]).map(drop));
    }

    pub fn move_to_back() {
        with_activity(|env, activity| {
            env.call_method(activity, "moveTaskToBack", "(Z)Z", &[JValue::Bool(1)]).map(drop)
        });
    }

    /// The next batch of books from the picker, "Open with" or "Share", already copied into the
    /// books folder. Checked a few times a second; each batch is handed over once.
    pub fn take_picked_books() -> Vec<PathBuf> {
        static LAST: Mutex<Option<Instant>> = Mutex::new(None);
        let Ok(mut last) = LAST.lock() else { return Vec::new() };
        if last.is_some_and(|at| at.elapsed() < Duration::from_millis(300)) {
            return Vec::new();
        }
        *last = Some(Instant::now());
        with_activity(|env, activity| {
            let batch = env.call_method(activity, "takePickedBooks", "()[Ljava/lang/String;", &[])?.l()?;
            if batch.is_null() {
                return Ok(Vec::new());
            }
            let batch = JObjectArray::from(batch);
            let mut paths = Vec::new();
            for i in 0..env.get_array_length(&batch)? {
                let path = JString::from(env.get_object_array_element(&batch, i)?);
                paths.push(PathBuf::from(String::from(env.get_string(&path)?)));
            }
            Ok(paths)
        })
        .unwrap_or_default()
    }

    /// `getWindow().getDecorView().getRootWindowInsets().getInsets(systemBars | displayCutout)`,
    /// in physical pixels, as `[left, top, right, bottom]`.
    fn system_insets() -> Option<[i32; 4]> {
        with_activity(|env, activity| {
            let window = env.call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
            let decor = env.call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?.l()?;
            let insets =
                env.call_method(&decor, "getRootWindowInsets", "()Landroid/view/WindowInsets;", &[])?.l()?;
            if insets.is_null() {
                return Ok(None);
            }
            let kind = "android/view/WindowInsets$Type";
            let bars = env.call_static_method(kind, "systemBars", "()I", &[])?.i()?;
            let cutout = env.call_static_method(kind, "displayCutout", "()I", &[])?.i()?;
            let insets = env
                .call_method(
                    &insets,
                    "getInsets",
                    "(I)Landroid/graphics/Insets;",
                    &[JValue::Int(bars | cutout)],
                )?
                .l()?;
            let mut side = |name: &str| env.get_field(&insets, name, "I").and_then(|v| v.i());
            Ok(Some([side("left")?, side("top")?, side("right")?, side("bottom")?]))
        })
        .flatten()
    }

    /// Run some JNI calls against the activity, in a local reference frame so nothing leaks.
    fn with_activity<T>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<T>) -> Option<T> {
        // Not `ndk_context`: that holds the Application, which has none of the Activity's methods.
        let app = app()?;
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.ok()?;
        let mut env = vm.attach_current_thread_permanently().ok()?;
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        let result = env.with_local_frame(16, |env| f(env, &activity));
        // A pending Java exception would poison every later JNI call.
        if let Err(error) = &result {
            log::warn!("JNI call failed: {error}");
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        result.ok()
    }
}
