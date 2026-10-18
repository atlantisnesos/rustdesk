use std::{
    collections::HashMap,
    iter::FromIterator,
    sync::{Arc, Mutex},
};

use sciter::Value;

use hbb_common::{
    allow_err,
    config::{LocalConfig, PeerConfig},
    log,
};

#[cfg(not(feature = "flutter"))]
use crate::ui_session_interface::Session;
use crate::{common::get_app_name, ipc, ui_interface::*};

mod cm;
#[cfg(feature = "inline")]
pub mod inline;
pub mod remote;

#[allow(dead_code)]
type Status = (i32, bool, i64, String);

lazy_static::lazy_static! {
    // stupid workaround for https://sciter.com/forums/topic/crash-on-latest-tis-mac-sdk-sometimes/
    static ref STUPID_VALUES: Mutex<Vec<Arc<Vec<Value>>>> = Default::default();
}

#[cfg(not(feature = "flutter"))]
lazy_static::lazy_static! {
    pub static ref CUR_SESSION: Arc<Mutex<Option<Session<remote::SciterHandler>>>> = Default::default();
}

struct UIHostHandler;

pub fn start(args: &mut [String]) {
    #[cfg(target_os = "macos")]
    crate::platform::delegate::show_dock();
    #[cfg(all(target_os = "linux", feature = "inline"))]
    {
        let app_dir = std::env::var("APPDIR").unwrap_or("".to_string());
        let mut so_path = "/usr/share/rustdesk/libsciter-gtk.so".to_owned();
        for (prefix, dir) in [
            ("", "/usr"),
            ("", "/app"),
            (&app_dir, "/usr"),
            (&app_dir, "/app"),
        ]
        .iter()
        {
            let path = format!("{prefix}{dir}/share/rustdesk/libsciter-gtk.so");
            if std::path::Path::new(&path).exists() {
                so_path = path;
                break;
            }
        }
        sciter::set_library(&so_path).ok();
    }
    #[cfg(windows)]
    // Check if there is a sciter.dll nearby.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sciter_dll_path = parent.join("sciter.dll");
            if sciter_dll_path.exists() {
                // Try to set the sciter dll.
                let p = sciter_dll_path.to_string_lossy().to_string();
                log::debug!("Found dll:{}, \n {:?}", p, sciter::set_library(&p));
            }
        }
    }
    // https://github.com/c-smile/sciter-sdk/blob/master/include/sciter-x-types.h
    // https://github.com/rustdesk/rustdesk/issues/132#issuecomment-886069737
    #[cfg(windows)]
    allow_err!(sciter::set_options(sciter::RuntimeOptions::GfxLayer(
        sciter::GFX_LAYER::WARP
    )));
    use sciter::SCRIPT_RUNTIME_FEATURES::*;
    allow_err!(sciter::set_options(sciter::RuntimeOptions::ScriptFeatures(
        ALLOW_FILE_IO as u8 | ALLOW_SOCKET_IO as u8 | ALLOW_EVAL as u8 | ALLOW_SYSINFO as u8
    )));
    let mut frame = sciter::WindowBuilder::main_window().create();
    #[cfg(windows)]
    allow_err!(sciter::set_options(sciter::RuntimeOptions::UxTheming(true)));
    frame.set_title("Soporte - Atlantis Nesos");
    #[cfg(target_os = "macos")]
    crate::platform::delegate::make_menubar(frame.get_host(), args.is_empty());
    #[cfg(windows)]
    crate::platform::try_set_window_foreground(frame.get_hwnd() as _);
    let page;
    if args.len() > 1 && args[0] == "--play" {
        args[0] = "--connect".to_owned();
        let path: std::path::PathBuf = (&args[1]).into();
        let id = path
            .file_stem()
            .map(|p| p.to_str().unwrap_or(""))
            .unwrap_or("")
            .to_owned();
        args[1] = id;
    }
    if args.is_empty() {
        std::thread::spawn(move || check_zombie());
        crate::common::check_software_update();
        frame.event_handler(UI {});
        frame.sciter_handler(UIHostHandler {});
        page = "index.html";
        // Start pulse audio local server.
        #[cfg(target_os = "linux")]
        std::thread::spawn(crate::ipc::start_pa);
    } else if args[0] == "--install" {
        frame.event_handler(UI {});
        frame.sciter_handler(UIHostHandler {});
        page = "install.html";
    } else if args[0] == "--cm" {
        frame.register_behavior("connection-manager", move || {
            Box::new(cm::SciterConnectionManager::new())
        });
        page = "cm.html";
        *cm::HIDE_CM.lock().unwrap() = crate::ipc::get_config("hide_cm")
            .ok()
            .flatten()
            .unwrap_or_default()
            == "true";
    } else if (args[0] == "--connect"
        || args[0] == "--file-transfer"
        || args[0] == "--port-forward"
        || args[0] == "--rdp")
        && args.len() > 1
    {
        #[cfg(windows)]
        {
            let hw = frame.get_host().get_hwnd();
            crate::platform::windows::enable_lowlevel_keyboard(hw as _);
        }
        let mut iter = args.iter();
        let Some(cmd) = iter.next() else {
            log::error!("Failed to get cmd arg");
            return;
        };
        let cmd = cmd.to_owned();
        let Some(id) = iter.next() else {
            log::error!("Failed to get id arg");
            return;
        };
        let id = id.to_owned();
        let pass = iter.next().unwrap_or(&"".to_owned()).clone();
        let args: Vec<String> = iter.map(|x| x.clone()).collect();
        frame.set_title(&id);
        frame.register_behavior("native-remote", move || {
            let handler =
                remote::SciterSession::new(cmd.clone(), id.clone(), pass.clone(), args.clone());
            #[cfg(not(feature = "flutter"))]
            {
                *CUR_SESSION.lock().unwrap() = Some(handler.inner());
            }
            Box::new(handler)
        });
        page = "remote.html";
    } else {
        log::error!("Wrong command: {:?}", args);
        return;
    }
    #[cfg(feature = "inline")]
    {
        let html = if page == "index.html" {
            inline::get_index()
        } else if page == "cm.html" {
            inline::get_cm()
        } else if page == "install.html" {
            inline::get_install()
        } else {
            inline::get_remote()
        };
        frame.load_html(html.as_bytes(), Some(page));
    }
    #[cfg(not(feature = "inline"))]
    frame.load_file(&format!(
        "file://{}/src/ui/{}",
        std::env::current_dir()
            .map(|c| c.display().to_string())
            .unwrap_or("".to_owned()),
        page
    ));
    let hide_cm = *cm::HIDE_CM.lock().unwrap();
    if !args.is_empty() && args[0] == "--cm" && hide_cm {
        // run_app calls expand(show) + run_loop, we use collapse(hide) + run_loop instead to create a hidden window
        frame.collapse(true);
        frame.run_loop();
        return;
    }
    frame.run_app();
}

struct UI {}

impl UI {
    fn recent_sessions_updated(&self) -> bool {
        recent_sessions_updated()
    }

    fn get_id(&self) -> String {
        ipc::get_id()
    }

    fn temporary_password(&mut self) -> String {
        temporary_password()
    }

    fn update_temporary_password(&self) {
        update_temporary_password()
    }

    fn set_permanent_password(&self, password: String) {
        let _ = set_permanent_password_with_result(password);
    }

    fn is_local_permanent_password_set(&self) -> bool {
        is_local_permanent_password_set()
    }

    fn is_permanent_password_set(&self) -> bool {
        is_permanent_password_set()
    }

    fn get_remote_id(&mut self) -> String {
        LocalConfig::get_remote_id()
    }

    fn set_remote_id(&mut self, id: String) {
        LocalConfig::set_remote_id(&id);
    }

    fn goto_install(&mut self) {
        goto_install();
    }

    fn install_me(&mut self, _options: String, _path: String) {
        install_me(_options, _path, false, false);
    }

    fn update_me(&self, _path: String) {
        update_me(_path);
    }

    fn run_without_install(&self) {
        run_without_install();
    }

    fn show_run_without_install(&self) -> bool {
        show_run_without_install()
    }

    fn get_license(&self) -> String {
        get_license()
    }

    fn get_option(&self, key: String) -> String {
        get_option(key)
    }

    fn get_local_option(&self, key: String) -> String {
        get_local_option(key)
    }

    fn set_local_option(&self, key: String, value: String) {
        set_local_option(key, value);
    }

    fn peer_has_password(&self, id: String) -> bool {
        peer_has_password(id)
    }

    fn forget_password(&self, id: String) {
        forget_password(id)
    }

    fn get_peer_option(&self, id: String, name: String) -> String {
        get_peer_option(id, name)
    }

    fn set_peer_option(&self, id: String, name: String, value: String) {
        set_peer_option(id, name, value)
    }

    fn using_public_server(&self) -> bool {
        crate::using_public_server()
    }

    fn is_incoming_only(&self) -> bool {
        hbb_common::config::is_incoming_only()
    }

    pub fn is_outgoing_only(&self) -> bool {
        hbb_common::config::is_outgoing_only()
    }

    pub fn is_custom_client(&self) -> bool {
        crate::common::is_custom_client()
    }

    pub fn is_disable_settings(&self) -> bool {
        hbb_common::config::is_disable_settings()
    }

    pub fn is_disable_account(&self) -> bool {
        hbb_common::config::is_disable_account()
    }

    pub fn is_disable_installation(&self) -> bool {
        hbb_common::config::is_disable_installation()
    }

    pub fn is_disable_ab(&self) -> bool {
        hbb_common::config::is_disable_ab()
    }

    fn get_options(&self) -> Value {
        let hashmap: HashMap<String, String> =
            serde_json::from_str(&get_options()).unwrap_or_default();
        let mut m = Value::map();
        for (k, v) in hashmap {
            m.set_item(k, v);
        }
        m
    }

    fn test_if_valid_server(&self, host: String, test_with_proxy: bool) -> String {
        test_if_valid_server(host, test_with_proxy)
    }

    fn get_sound_inputs(&self) -> Value {
        Value::from_iter(get_sound_inputs())
    }

    fn set_options(&self, v: Value) {
        let mut m = HashMap::new();
        for (k, v) in v.items() {
            if let Some(k) = k.as_string() {
                if let Some(v) = v.as_string() {
                    if !v.is_empty() {
                        m.insert(k, v);
                    }
                }
            }
        }
        set_options(m);
    }

    fn set_option(&self, key: String, value: String) {
        set_option(key, value);
    }

    fn install_path(&mut self) -> String {
        install_path()
    }

    fn install_options(&self) -> String {
        install_options()
    }

    fn get_socks(&self) -> Value {
        Value::from_iter(get_socks())
    }

    fn set_socks(&self, proxy: String, username: String, password: String) {
        set_socks(proxy, username, password)
    }

    fn is_installed(&self) -> bool {
        is_installed()
    }

    fn get_supported_privacy_mode_impls(&self) -> String {
        serde_json::to_string(&crate::privacy_mode::get_supported_privacy_mode_impl())
            .unwrap_or_default()
    }

    fn is_root(&self) -> bool {
        is_root()
    }

    fn is_release(&self) -> bool {
        #[cfg(not(debug_assertions))]
        return true;
        #[cfg(debug_assertions)]
        return false;
    }

    fn is_share_rdp(&self) -> bool {
        is_share_rdp()
    }

    fn set_share_rdp(&self, _enable: bool) {
        set_share_rdp(_enable);
    }

    fn is_installed_lower_version(&self) -> bool {
        is_installed_lower_version()
    }

    fn closing(&mut self, x: i32, y: i32, w: i32, h: i32) {
        crate::server::input_service::fix_key_down_timeout_at_exit();
        LocalConfig::set_size(x, y, w, h);
    }

    fn get_size(&mut self) -> Value {
        let s = LocalConfig::get_size();
        let mut v = Vec::new();
        v.push(s.0);
        v.push(s.1);
        v.push(s.2);
        v.push(s.3);
        Value::from_iter(v)
    }

    fn get_mouse_time(&self) -> f64 {
        get_mouse_time()
    }

    fn check_mouse_time(&self) {
        check_mouse_time()
    }

    fn get_connect_status(&mut self) -> Value {
        let mut v = Value::array(0);
        let x = get_connect_status();
        v.push(x.status_num);
        v.push(x.key_confirmed);
        v.push(x.id);
        v
    }

    #[inline]
    fn get_peer_value(id: String, p: PeerConfig) -> Value {
        let values = vec![
            id,
            p.info.username.clone(),
            p.info.hostname.clone(),
            p.info.platform.clone(),
            p.options.get("alias").unwrap_or(&"".to_owned()).to_owned(),
        ];
        Value::from_iter(values)
    }

    fn get_peer(&self, id: String) -> Value {
        let c = get_peer(id.clone());
        Self::get_peer_value(id, c)
    }

    fn get_fav(&self) -> Value {
        Value::from_iter(get_fav())
    }

    fn store_fav(&self, fav: Value) {
        let mut tmp = vec![];
        fav.values().for_each(|v| {
            if let Some(v) = v.as_string() {
                if !v.is_empty() {
                    tmp.push(v);
                }
            }
        });
        store_fav(tmp);
    }

    fn get_recent_sessions(&mut self) -> Value {
        // to-do: limit number of recent sessions, and remove old peer file
        let peers: Vec<Value> = PeerConfig::peers(None)
            .drain(..)
            .map(|p| Self::get_peer_value(p.0, p.2))
            .collect();
        Value::from_iter(peers)
    }

    fn get_icon(&mut self) -> String {
        get_icon()
    }

    fn remove_peer(&mut self, id: String) {
        PeerConfig::remove(&id);
    }

    fn remove_discovered(&mut self, id: String) {
        remove_discovered(id);
    }

    fn send_wol(&mut self, id: String) {
        crate::lan::send_wol(id)
    }

    fn new_remote(&mut self, id: String, remote_type: String, force_relay: bool) {
        new_remote(id, remote_type, force_relay)
    }

    fn is_process_trusted(&mut self, _prompt: bool) -> bool {
        is_process_trusted(_prompt)
    }

    fn is_can_screen_recording(&mut self, _prompt: bool) -> bool {
        is_can_screen_recording(_prompt)
    }

    fn is_installed_daemon(&mut self, _prompt: bool) -> bool {
        is_installed_daemon(_prompt)
    }

    fn get_error(&mut self) -> String {
        get_error()
    }

    fn is_login_wayland(&mut self) -> bool {
        is_login_wayland()
    }

    fn current_is_wayland(&mut self) -> bool {
        current_is_wayland()
    }

    fn get_software_update_url(&self) -> String {
        crate::SOFTWARE_UPDATE_URL.lock().unwrap().clone()
    }

    fn get_new_version(&self) -> String {
        get_new_version()
    }

    fn get_version(&self) -> String {
        get_version()
    }

    fn get_fingerprint(&self) -> String {
        get_fingerprint()
    }

    fn get_app_name(&self) -> String {
        get_app_name()
    }

    fn get_software_ext(&self) -> String {
        #[cfg(windows)]
        let p = "exe";
        #[cfg(target_os = "macos")]
        let p = "dmg";
        #[cfg(target_os = "linux")]
        let p = "deb";
        p.to_owned()
    }

    fn get_software_store_path(&self) -> String {
        let mut p = std::env::temp_dir();
        let name = crate::SOFTWARE_UPDATE_URL
            .lock()
            .unwrap()
            .split("/")
            .last()
            .map(|x| x.to_owned())
            .unwrap_or(crate::get_app_name());
        p.push(name);
        format!("{}.{}", p.to_string_lossy(), self.get_software_ext())
    }

    fn create_shortcut(&self, _id: String) {
        #[cfg(windows)]
        create_shortcut(_id)
    }

    fn discover(&self) {
        std::thread::spawn(move || {
            allow_err!(crate::lan::discover());
        });
    }

    fn get_lan_peers(&self) -> String {
        // let peers = get_lan_peers()
        //     .into_iter()
        //     .map(|mut peer| {
        //         (
        //             peer.remove("id").unwrap_or_default(),
        //             peer.remove("username").unwrap_or_default(),
        //             peer.remove("hostname").unwrap_or_default(),
        //             peer.remove("platform").unwrap_or_default(),
        //         )
        //     })
        //     .collect::<Vec<(String, String, String, String)>>();
        serde_json::to_string(&get_lan_peers()).unwrap_or_default()
    }

    fn get_uuid(&self) -> String {
        get_uuid()
    }

    fn open_url(&self, url: String) {
        #[cfg(windows)]
        let p = "explorer";
        #[cfg(target_os = "macos")]
        let p = "open";
        #[cfg(target_os = "linux")]
        let p = if std::path::Path::new("/usr/bin/firefox").exists() {
            "firefox"
        } else {
            "xdg-open"
        };
        allow_err!(std::process::Command::new(p).arg(url).spawn());
    }

    fn change_id(&self, id: String) {
        reset_async_job_status();
        let old_id = self.get_id();
        change_id_shared(id, old_id);
    }

    fn http_request(&self, url: String, method: String, body: Option<String>, header: String) {
        http_request(url, method, body, header)
    }

    fn post_request(&self, url: String, body: String, header: String) {
        post_request(url, body, header)
    }

    fn is_ok_change_id(&self) -> bool {
        hbb_common::machine_uid::get().is_ok()
    }

    fn get_async_job_status(&self) -> String {
        get_async_job_status()
    }

    fn get_http_status(&self, url: String) -> Option<String> {
        get_async_http_status(url)
    }

    fn t(&self, name: String) -> String {
        crate::client::translate(name)
    }

    fn is_xfce(&self) -> bool {
        crate::platform::is_xfce()
    }

    fn get_api_server(&self) -> String {
        get_api_server()
    }

    fn has_hwcodec(&self) -> bool {
        has_hwcodec()
    }

    fn has_vram(&self) -> bool {
        has_vram()
    }

    fn get_langs(&self) -> String {
        get_langs()
    }

    fn video_save_directory(&self, root: bool) -> String {
        video_save_directory(root)
    }

    fn handle_relay_id(&self, id: String) -> String {
        handle_relay_id(&id).to_owned()
    }

    fn get_login_device_info(&self) -> String {
        get_login_device_info_json()
    }

    fn support_remove_wallpaper(&self) -> bool {
        support_remove_wallpaper()
    }

    fn has_valid_2fa(&self) -> bool {
        has_valid_2fa()
    }

    fn generate2fa(&self) -> String {
        generate2fa()
    }

    pub fn verify2fa(&self, code: String) -> bool {
        verify2fa(code)
    }

    fn verify_login(&self, raw: String, id: String) -> bool {
        crate::verify_login(&raw, &id)
    }

    fn generate_2fa_img_src(&self, data: String) -> String {
        let v = qrcode_generator::to_png_to_vec(data, qrcode_generator::QrCodeEcc::Low, 128)
            .unwrap_or_default();
        let s = hbb_common::sodiumoxide::base64::encode(
            v,
            hbb_common::sodiumoxide::base64::Variant::Original,
        );
        format!("data:image/png;base64,{s}")
    }

    pub fn check_hwcodec(&self) {
        check_hwcodec()
    }

    fn is_option_fixed(&self, key: String) -> bool {
        crate::ui_interface::is_option_fixed(&key)
    }

    fn get_builtin_option(&self, key: String) -> String {
        crate::ui_interface::get_builtin_option(&key)
    }

    fn is_remote_modify_enabled_by_control_permissions(&self) -> String {
        match crate::ui_interface::is_remote_modify_enabled_by_control_permissions() {
            Some(true) => "true",
            Some(false) => "false",
            None => "",
        }
        .to_string()
    }
}

impl sciter::EventHandler for UI {
    sciter::dispatch_script_call! {
        fn t(String);
        fn get_api_server();
        fn is_xfce();
        fn using_public_server();
        fn is_custom_client();
        fn is_outgoing_only();
        fn is_incoming_only();
        fn is_disable_settings();
        fn is_disable_account();
        fn is_disable_installation();
        fn is_disable_ab();
        fn get_id();
        fn temporary_password();
        fn update_temporary_password();
        fn set_permanent_password(String);
        fn is_local_permanent_password_set();
        fn is_permanent_password_set();
        fn get_remote_id();
        fn set_remote_id(String);
        fn closing(i32, i32, i32, i32);
        fn get_size();
        fn new_remote(String, String, bool);
        fn send_wol(String);
        fn remove_peer(String);
        fn remove_discovered(String);
        fn get_connect_status();
        fn get_mouse_time();
        fn check_mouse_time();
        fn get_recent_sessions();
        fn get_peer(String);
        fn get_fav();
        fn store_fav(Value);
        fn recent_sessions_updated();
        fn get_icon();
        fn install_me(String, String);
        fn is_installed();
        fn get_supported_privacy_mode_impls();
        fn is_root();
        fn is_release();
        fn set_socks(String, String, String);
        fn get_socks();
        fn is_share_rdp();
        fn set_share_rdp(bool);
        fn is_installed_lower_version();
        fn install_path();
        fn install_options();
        fn goto_install();
        fn is_process_trusted(bool);
        fn is_can_screen_recording(bool);
        fn is_installed_daemon(bool);
        fn get_error();
        fn is_login_wayland();
        fn current_is_wayland();
        fn get_options();
        fn get_option(String);
        fn get_local_option(String);
        fn set_local_option(String, String);
        fn get_peer_option(String, String);
        fn peer_has_password(String);
        fn forget_password(String);
        fn set_peer_option(String, String, String);
        fn get_license();
        fn test_if_valid_server(String, bool);
        fn get_sound_inputs();
        fn set_options(Value);
        fn set_option(String, String);
        fn get_software_update_url();
        fn get_new_version();
        fn get_version();
        fn get_fingerprint();
        fn update_me(String);
        fn show_run_without_install();
        fn run_without_install();
        fn get_app_name();
        fn get_software_store_path();
        fn get_software_ext();
        fn open_url(String);
        fn change_id(String);
        fn get_async_job_status();
        fn post_request(String, String, String);
        fn is_ok_change_id();
        fn create_shortcut(String);
        fn discover();
        fn get_lan_peers();
        fn get_uuid();
        fn has_hwcodec();
        fn has_vram();
        fn get_langs();
        fn video_save_directory(bool);
        fn handle_relay_id(String);
        fn get_login_device_info();
        fn support_remove_wallpaper();
        fn has_valid_2fa();
        fn generate2fa();
        fn generate_2fa_img_src(String);
        fn verify2fa(String);
        fn check_hwcodec();
        fn verify_login(String, String);
        fn is_option_fixed(String);
        fn get_builtin_option(String);
        fn is_remote_modify_enabled_by_control_permissions();
    }
}

impl sciter::host::HostHandler for UIHostHandler {
    fn on_graphics_critical_failure(&mut self) {
        log::error!("Critical rendering error: e.g. DirectX gfx driver error. Most probably bad gfx drivers.");
    }
}

#[cfg(not(target_os = "linux"))]
fn get_sound_inputs() -> Vec<String> {
    let mut out = Vec::new();
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    if let Ok(devices) = host.devices() {
        for device in devices {
            if device.default_input_config().is_err() {
                continue;
            }
            if let Ok(name) = device.name() {
                out.push(name);
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn get_sound_inputs() -> Vec<String> {
    crate::platform::linux::get_pa_sources()
        .drain(..)
        .map(|x| x.1)
        .collect()
}

// sacrifice some memory
pub fn value_crash_workaround(values: &[Value]) -> Arc<Vec<Value>> {
    let persist = Arc::new(values.to_vec());
    STUPID_VALUES.lock().unwrap().push(persist.clone());
    persist
}

pub fn get_icon() -> String {
    // 128x128
    #[cfg(target_os = "macos")]
    // 128x128 on 160x160 canvas, then shrink to 128, mac looks better with padding
    {
        "data:image/png;base64,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".into()
    }
    #[cfg(not(target_os = "macos"))] // 128x128 no padding
    {
        "data:image/png;base64,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".into()
    }
}
