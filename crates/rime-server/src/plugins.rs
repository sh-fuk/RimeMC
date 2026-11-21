//! Bukkit-API plugin host.
//!
//! Plugin jars in `[plugins].dir` are loaded into a JVM embedded in the
//! server process (JNI). RimeMC ships its own implementation of a subset of
//! the `org.bukkit` API (`bukkit-api/` in the repository, prebuilt into
//! `assets/bukkit/rime-bukkit-api.jar`), so plugins that only use that
//! subset run unchanged; anything outside it fails with `NoSuchMethodError`
//! / `NoClassDefFoundError` when the plugin reaches it, and the error is
//! logged without taking the server down.
//!
//! Calls into Java are serialized by one re-entrant lock; Java calls back
//! into Rust through the `Bridge` natives below, which never take that lock.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use anyhow::{bail, Context};
use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jdouble, jfloat, jint, jlong, jobject, jobjectArray, jstring, JNI_FALSE, JNI_TRUE};
use jni::{InitArgsBuilder, JNIEnv, JNIVersion, JavaVM, NativeMethod};
use parking_lot::ReentrantMutex;

use crate::game::Request;
use crate::ops::CommandSender;
use crate::ServerState;

const API_JAR: &[u8] = include_bytes!("../assets/bukkit/rime-bukkit-api.jar");
const BRIDGE: &str = "dev/rime/bukkit/Bridge";

struct Host {
    vm: JavaVM,
    bridge: GlobalRef,
    lock: ReentrantMutex<()>,
}

static HOST: OnceLock<Host> = OnceLock::new();
static STATE: OnceLock<Weak<ServerState>> = OnceLock::new();
static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();

fn state() -> Option<Arc<ServerState>> {
    STATE.get()?.upgrade()
}

/// True once plugins are loaded (hooks are no-ops otherwise).
pub fn active() -> bool {
    HOST.get().is_some()
}

/// Starts the JVM and loads + enables every plugin jar. Does nothing when
/// the plugin folder holds no jars.
pub fn start(st: &Arc<ServerState>, base: &Path) -> anyhow::Result<()> {
    let cfg = &st.cfg.plugins;
    if !cfg.enabled {
        return Ok(());
    }
    let dir = base.join(&cfg.dir);
    let has_jars = std::fs::read_dir(&dir)
        .map(|r| r.flatten().any(|e| e.path().extension().is_some_and(|x| x == "jar")))
        .unwrap_or(false);
    if !has_jars {
        let _ = std::fs::create_dir_all(&dir);
        return Ok(());
    }
    let libjvm = find_libjvm(&cfg.java_home).context("plugins need a Java 17+ runtime; set [plugins].java_home")?;
    let api = api_jar_path();
    std::fs::write(&api, API_JAR).context("writing the Bukkit API jar")?;

    let mut args = InitArgsBuilder::new()
        .version(JNIVersion::V8)
        .option(format!("-Djava.class.path={}", api.display()))
        // leave SIGINT/SIGTERM to the server
        .option("-Xrs")
        .option("-Djava.awt.headless=true");
    for o in &cfg.jvm_options {
        args = args.option(o.as_str());
    }
    let vm = JavaVM::with_libjvm(args.build()?, || Ok(libjvm.clone())).with_context(|| format!("starting the JVM from {}", libjvm.display()))?;
    let _ = STATE.set(Arc::downgrade(st));
    if let Ok(h) = tokio::runtime::Handle::try_current() {
        let _ = RUNTIME.set(h);
    }
    let bridge = {
        let mut env = vm.attach_current_thread_permanently()?;
        let class = env.find_class(BRIDGE).context("Bridge class missing from the API jar")?;
        env.register_native_methods(&class, &natives())?;
        env.new_global_ref(&class)?
    };
    if HOST.set(Host { vm, bridge, lock: ReentrantMutex::new(()) }).is_err() {
        bail!("plugins already started");
    }
    let dir_s = std::fs::canonicalize(&dir).unwrap_or(dir).display().to_string();
    let names = call(Vec::new(), |env, c| {
        let d = env.new_string(&dir_s)?;
        let arr = env.call_static_method(c, "load", "(Ljava/lang/String;)[Ljava/lang/String;", &[JValue::Object(&d)])?.l()?;
        strings(env, arr)
    });
    tracing::info!("loaded {} Bukkit plugin(s): {}", names.len(), names.join(", "));
    call((), |env, c| {
        env.call_static_method(c, "enable", "()V", &[])?;
        Ok(())
    });
    Ok(())
}

/// Disables every plugin (server shutdown).
pub fn stop() {
    call((), |env, c| {
        env.call_static_method(c, "disable", "()V", &[])?;
        Ok(())
    });
    if active() {
        let _ = std::fs::remove_file(api_jar_path());
    }
}

fn api_jar_path() -> PathBuf {
    std::env::temp_dir().join(format!("rime-bukkit-api-{}.jar", std::process::id()))
}

/// Runs due scheduler tasks (once per game tick).
pub fn tick() {
    call((), |env, c| {
        env.call_static_method(c, "tick", "()V", &[])?;
        Ok(())
    });
}

/// PlayerJoinEvent: returns the join message to broadcast (empty = none).
pub fn join(name: &str, msg: &str) -> String {
    call_name_msg("join", name, msg).unwrap_or_else(|| msg.to_string())
}

/// PlayerQuitEvent: returns the quit message to broadcast (empty = none).
pub fn quit(name: &str, msg: &str) -> String {
    call_name_msg("quit", name, msg).unwrap_or_else(|| msg.to_string())
}

/// AsyncPlayerChatEvent: the formatted line, or None when cancelled.
pub fn chat(name: &str, msg: &str) -> Option<String> {
    if !active() {
        return Some(format!("<{name}> {msg}"));
    }
    let fallback = format!("<{name}> {msg}");
    call(Some(fallback), |env, c| {
        let (n, m) = (env.new_string(name)?, env.new_string(msg)?);
        let r = env.call_static_method(c, "chat", "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;", &[JValue::Object(&n), JValue::Object(&m)])?.l()?;
        if r.is_null() {
            return Ok(None);
        }
        Ok(Some(env.get_string(&JString::from(r))?.into()))
    })
}

/// PlayerCommandPreprocessEvent + plugin commands: true when handled.
pub fn command(name: &str, line: &str) -> bool {
    call(false, |env, c| {
        let (n, l) = (env.new_string(name)?, env.new_string(line)?);
        env.call_static_method(c, "command", "(Ljava/lang/String;Ljava/lang/String;)Z", &[JValue::Object(&n), JValue::Object(&l)])?.z()
    })
}

/// Names of plugin commands (for the client's command tree).
pub fn command_names() -> Vec<String> {
    call(Vec::new(), |env, c| {
        let arr = env.call_static_method(c, "commandNames", "()[Ljava/lang/String;", &[])?.l()?;
        strings(env, arr)
    })
}

/// BlockBreakEvent: true when a plugin cancelled it.
pub fn block_break(name: &str, p: [i32; 3]) -> bool {
    call(false, |env, c| {
        let n = env.new_string(name)?;
        env.call_static_method(c, "blockBreak", "(Ljava/lang/String;III)Z", &[JValue::Object(&n), JValue::Int(p[0]), JValue::Int(p[1]), JValue::Int(p[2])])?.z()
    })
}

/// BlockPlaceEvent: true when a plugin cancelled it.
pub fn block_place(name: &str, p: [i32; 3], block: &str) -> bool {
    call(false, |env, c| {
        let (n, b) = (env.new_string(name)?, env.new_string(block)?);
        env.call_static_method(c, "blockPlace", "(Ljava/lang/String;IIILjava/lang/String;)Z", &[JValue::Object(&n), JValue::Int(p[0]), JValue::Int(p[1]), JValue::Int(p[2]), JValue::Object(&b)])?.z()
    })
}

fn call_name_msg(method: &str, name: &str, msg: &str) -> Option<String> {
    if !active() {
        return None;
    }
    call(None, |env, c| {
        let (n, m) = (env.new_string(name)?, env.new_string(msg)?);
        let r = env.call_static_method(c, method, "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;", &[JValue::Object(&n), JValue::Object(&m)])?.l()?;
        if r.is_null() {
            return Ok(Some(String::new()));
        }
        Ok(Some(env.get_string(&JString::from(r))?.into()))
    })
}

/// Runs `f` with the Bridge class on this thread's JNIEnv; any Java
/// exception is logged and cleared and `default` returned.
fn call<R>(default: R, f: impl FnOnce(&mut JNIEnv, &JClass) -> jni::errors::Result<R>) -> R {
    let Some(h) = HOST.get() else { return default };
    let _g = h.lock.lock();
    let mut env = match h.vm.attach_current_thread_permanently() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("plugins: attaching thread: {e}");
            return default;
        }
    };
    // SAFETY: the global reference holds the Bridge class object for the life of the VM.
    let class = unsafe { JClass::from_raw(h.bridge.as_obj().as_raw()) };
    let r = env.with_local_frame(64, |env| f(env, &class));
    // `class` borrows the global ref; don't let its drop do anything
    std::mem::forget(class);
    match r {
        Ok(v) => v,
        Err(e) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_describe();
                let _ = env.exception_clear();
            }
            tracing::error!("plugins: call into Java failed: {e}");
            default
        }
    }
}

fn strings(env: &mut JNIEnv, arr: JObject) -> jni::errors::Result<Vec<String>> {
    if arr.is_null() {
        return Ok(Vec::new());
    }
    let arr = jni::objects::JObjectArray::from(arr);
    let n = env.get_array_length(&arr)?;
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let s = env.get_object_array_element(&arr, i)?;
        out.push(env.get_string(&JString::from(s))?.into());
    }
    Ok(out)
}

/// libjvm from the configured home, $JAVA_HOME, or macOS `java_home`.
fn find_libjvm(configured: &str) -> Option<PathBuf> {
    let mut homes: Vec<PathBuf> = Vec::new();
    if !configured.is_empty() {
        homes.push(configured.into());
    }
    if let Ok(h) = std::env::var("JAVA_HOME") {
        homes.push(h.into());
    }
    if let Ok(o) = std::process::Command::new("/usr/libexec/java_home").output() {
        if o.status.success() {
            homes.push(String::from_utf8_lossy(&o.stdout).trim().into());
        }
    }
    if let Ok(o) = std::process::Command::new("java").args(["-XshowSettings:properties", "-version"]).output() {
        let text = String::from_utf8_lossy(&o.stderr).to_string();
        if let Some(l) = text.lines().find(|l| l.trim_start().starts_with("java.home =")) {
            homes.push(l.split_once('=').map(|x| x.1.trim()).unwrap_or("").into());
        }
    }
    let lib = if cfg!(target_os = "macos") { "libjvm.dylib" } else if cfg!(windows) { "jvm.dll" } else { "libjvm.so" };
    homes.iter().flat_map(|h| [h.join("lib/server").join(lib), h.join("jre/lib/server").join(lib), h.join("bin/server").join(lib)]).find(|p| Path::new(p).exists())
}

// ------------------------------------------------------------------ natives

fn natives() -> Vec<NativeMethod> {
    fn m(name: &str, sig: &str, f: *mut std::ffi::c_void) -> NativeMethod {
        NativeMethod { name: name.into(), sig: sig.into(), fn_ptr: f }
    }
    const S: &str = "Ljava/lang/String;";
    vec![
        m("log", &format!("(I{S})V"), n_log as *mut _),
        m("broadcast", &format!("({S})V"), n_broadcast as *mut _),
        m("players", &format!("()[{S}"), n_players as *mut _),
        m("playerUuid", &format!("({S}){S}"), n_player_uuid as *mut _),
        m("playerPos", &format!("({S})[D"), n_player_pos as *mut _),
        m("sendMessage", &format!("({S}{S})V"), n_send_message as *mut _),
        m("request", &format!("({S}I{S}DDDFF)V"), n_request as *mut _),
        m("isOp", &format!("({S})Z"), n_is_op as *mut _),
        m("gamemode", &format!("({S})I"), n_gamemode as *mut _),
        m("health", &format!("({S})D"), n_health as *mut _),
        m("getBlock", &format!("(III){S}"), n_get_block as *mut _),
        m("setBlock", &format!("(III{S})Z"), n_set_block as *mut _),
        m("giveItem", &format!("({S}{S}I)Z"), n_give_item as *mut _),
        m("worldTime", "()J", n_world_time as *mut _),
        m("setWorldTime", "(J)V", n_set_world_time as *mut _),
        m("maxPlayers", "()I", n_max_players as *mut _),
        m("motd", &format!("(){S}"), n_motd as *mut _),
        m("dispatchConsole", &format!("({S}){S}"), n_dispatch_console as *mut _),
    ]
}

fn rs(env: &mut JNIEnv, s: &JString) -> String {
    if s.is_null() {
        return String::new();
    }
    env.get_string(s).map(|s| s.into()).unwrap_or_default()
}

fn js(env: &mut JNIEnv, s: &str) -> jstring {
    env.new_string(s).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
}

fn player(name: &str) -> Option<Arc<crate::game::Player>> {
    state()?.game.find(name)
}

extern "system" fn n_log(mut env: JNIEnv, _: JClass, level: jint, msg: JString) {
    let m = rs(&mut env, &msg);
    match level {
        2 => tracing::error!("[plugin] {m}"),
        1 => tracing::warn!("[plugin] {m}"),
        _ => tracing::info!("[plugin] {m}"),
    }
}

extern "system" fn n_broadcast(mut env: JNIEnv, _: JClass, msg: JString) {
    let m = rs(&mut env, &msg);
    if let Some(st) = state() {
        tracing::info!("[broadcast] {m}");
        st.game.broadcast(st.game.system_chat(&m));
    }
}

extern "system" fn n_players(mut env: JNIEnv, _: JClass) -> jobjectArray {
    let names: Vec<String> = state().map(|st| st.game.players().iter().map(|p| p.name.clone()).collect()).unwrap_or_default();
    let r = (|| -> jni::errors::Result<jobjectArray> {
        let arr = env.new_object_array(names.len() as i32, "java/lang/String", JObject::null())?;
        for (i, n) in names.iter().enumerate() {
            let s = env.new_string(n)?;
            env.set_object_array_element(&arr, i as i32, s)?;
        }
        Ok(arr.into_raw())
    })();
    r.unwrap_or(std::ptr::null_mut())
}

extern "system" fn n_player_uuid(mut env: JNIEnv, _: JClass, name: JString) -> jstring {
    let n = rs(&mut env, &name);
    match player(&n) {
        Some(p) => js(&mut env, &p.uuid.to_string()),
        None => std::ptr::null_mut(),
    }
}

extern "system" fn n_player_pos(mut env: JNIEnv, _: JClass, name: JString) -> jobject {
    let n = rs(&mut env, &name);
    let Some(p) = player(&n) else { return std::ptr::null_mut() };
    let v = {
        let pose = p.pose.lock();
        [pose.pos[0], pose.pos[1], pose.pos[2], pose.yaw as f64, pose.pitch as f64]
    };
    match env.new_double_array(5) {
        Ok(a) => {
            let _ = env.set_double_array_region(&a, 0, &v);
            a.into_raw()
        }
        Err(_) => std::ptr::null_mut(),
    }
}

extern "system" fn n_send_message(mut env: JNIEnv, _: JClass, name: JString, msg: JString) {
    let (n, m) = (rs(&mut env, &name), rs(&mut env, &msg));
    if let (Some(st), Some(p)) = (state(), player(&n)) {
        p.send(st.game.system_chat(&m));
    }
}

#[allow(clippy::too_many_arguments)]
extern "system" fn n_request(mut env: JNIEnv, _: JClass, name: JString, kind: jint, arg: JString, x: jdouble, y: jdouble, z: jdouble, yaw: jfloat, pitch: jfloat) {
    let (n, a) = (rs(&mut env, &name), rs(&mut env, &arg));
    let Some(p) = player(&n) else { return };
    let r = match kind {
        0 => Request::Teleport([x, y, z], yaw, pitch),
        1 => Request::Gamemode(x as i32),
        2 => Request::Kick(a),
        3 => Request::Command(a),
        4 => Request::Chat(a),
        _ => return,
    };
    p.requests.lock().push(r);
}

extern "system" fn n_is_op(mut env: JNIEnv, _: JClass, name: JString) -> jboolean {
    let n = rs(&mut env, &name);
    let op = state().is_some_and(|st| st.ops.is_op(&CommandSender::Player { name: n }));
    if op {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

extern "system" fn n_gamemode(mut env: JNIEnv, _: JClass, name: JString) -> jint {
    let n = rs(&mut env, &name);
    player(&n).map(|p| p.gamemode.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(0)
}

extern "system" fn n_health(mut env: JNIEnv, _: JClass, name: JString) -> jdouble {
    let n = rs(&mut env, &name);
    player(&n).map(|p| p.vit.lock().health() as f64).unwrap_or(0.0)
}

extern "system" fn n_get_block(mut env: JNIEnv, _: JClass, x: jint, y: jint, z: jint) -> jstring {
    match state().and_then(|st| st.game.world.get_block(x, y, z)) {
        Some(b) => js(&mut env, b.name()),
        None => std::ptr::null_mut(),
    }
}

extern "system" fn n_set_block(mut env: JNIEnv, _: JClass, x: jint, y: jint, z: jint, material: JString) -> jboolean {
    let m = rs(&mut env, &material);
    let Some(st) = state() else { return JNI_FALSE };
    let Some(b) = crate::registry::BlockRegistry::get().state(&m, &[]) else { return JNI_FALSE };
    if st.game.change_block([x, y, z], b, false, None).is_some() {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

extern "system" fn n_give_item(mut env: JNIEnv, _: JClass, name: JString, material: JString, count: jint) -> jboolean {
    let (n, m) = (rs(&mut env, &name), rs(&mut env, &material));
    let (Some(st), Some(p)) = (state(), player(&n)) else { return JNI_FALSE };
    let items = crate::data::Items::get();
    let Some(id) = items.id(&m).filter(|i| *i != 0) else { return JNI_FALSE };
    let mut left = count.max(0);
    while left > 0 {
        let n = left.min(items.max_stack(id) as i32);
        let held = p.held.load(std::sync::atomic::Ordering::Relaxed) as i16;
        let rest = p.inv.lock().add(crate::data::Stack::new(id, n as u8), held);
        left -= n - rest as i32;
        if rest > 0 {
            break;
        }
    }
    st.game.flush_inventory(&p);
    if left == 0 {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

extern "system" fn n_world_time(_: JNIEnv, _: JClass) -> jlong {
    state().map(|st| st.game.clock() as i64 + 1000).unwrap_or(0)
}

extern "system" fn n_set_world_time(_: JNIEnv, _: JClass, t: jlong) {
    if let Some(st) = state() {
        st.game.set_day_time(t.rem_euclid(24000) as u64);
    }
}

extern "system" fn n_max_players(_: JNIEnv, _: JClass) -> jint {
    state().map(|st| st.cfg.server.max_players as i32).unwrap_or(0)
}

extern "system" fn n_motd(mut env: JNIEnv, _: JClass) -> jstring {
    let m = state().map(|st| st.cfg.server.motd.clone()).unwrap_or_default();
    js(&mut env, &m)
}

/// Console commands run asynchronously; the reply goes to the server log.
extern "system" fn n_dispatch_console(mut env: JNIEnv, _: JClass, line: JString) -> jstring {
    let l = rs(&mut env, &line);
    if let (Some(st), Some(rt)) = (state(), RUNTIME.get()) {
        rt.spawn(async move {
            let reply = st.dispatch(&CommandSender::Console, &l).await;
            if !reply.is_empty() {
                tracing::info!("[plugin console] {reply}");
            }
        });
    }
    js(&mut env, "")
}
