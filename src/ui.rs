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
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAIAAAACACAYAAADDPmHLAAAw1ElEQVR4nO29eXxd1XX3/V17n3MnDZZkeR4Az8gGDAZKKSkhIxCgaUCCkDZtkr7h8yQd36ZJ+7SJcJM2Ce3Tp0PSt+R5k7QlCUQCQgkxUxIgUHDACsbYAhtsgwGDB83Snc7Zez1/nHvl2djYlmyc3+dzLVn33nP22Wvttde8hRMSKmjlVxHd/Z2Z/6sjm542f06YCRaqmjkQn+JUpovzU8WaiQITVLVGIWOUAMALsUBRREa8SD/O94iRbRh5TRxbELc51vD50paBTa/+6YWFPYeikowDYM+xnAiQ8R7AoUGFdqR1MdK57kZl+XJffeeMu55bWPTuAnFumSLniXIKhrQgVlErYFGMihhBDSACkvzY4x6qoIAq4kXVI3gFJ4pXIcZTQnhZVFb5QFZljF357AdPXz96ifZ207r4Rulch7IcPREY4nhmAKGjwyzrm2O6ti5zLBcPcP6KDfX9+dJl4vXdiF6ESLNCFiVjgjAQGyAogiak1AoNVPGqqGqFynvSRkSQyk8ju/hDK79r9aouRuM4VtGiQMF77TFqH/MS/6Seunu72uYOANCuZtn0Ltv1yWWuIqWOS2Y4/hhAVZZ9oyuoXTCkj1xySQyw8K7n6iRyHwL/ATUsRaUBpdFkMoERQbxD1KPOETmvkSqxV5xCrIrzSGihLrTUBgG5UAhFCCpEjlWJVMlHynAUMxQ5Ig/WoFaEQCAQITRCaI2ItagY1Fi8Kr5YcIj0ItqPZzWOexkOb1//e6cPAVz80EPB8IY66frksnjvLWu8cfwwQLualsXrgm66HW1tDmBex5qLDVyPkfMEpouxU2w2i8Qx4mKci7XkVEvOS8l5QmtlVk2KOXUpZtWmmJ4LmZ4NmZgJqQ8tKZsQ3QoYAVN5fI8msl4h9krZK4ORo6cY8Xo+Ymsh4pWRMpsGy2wZKVN2XjNWSFujGStibCBqAzQIcIUC6v02xW/F85Qn+N6LbS2PANDRYVtosd3rFsdViTbeGH8GqBK+bUkZ4Mz/XF1TzPnrVO0VxgQtqC4IauuRqAhxROS8G4mdFGMvNaGVxY05FjdkOL0hyym1KRpTlgkpS31oqQkNWWuOaHhF5xmOPIORY6Ds6Cs7Xh4u81x/ge7+Imv78oxETjOB0ZrAamiNJQjRMEM8PAgiG7yPu0XcPZm8uW3NR5eOALR0rE0dD4wwfgxQEfVdN5wbASy68+cTvctcB/b9iF4Q5GonCSDFEZzXeDBypuRUpuZCWdZcw1lNWRbUZ5hZk2JaLmRyJjjgrbwmq/xQH1ZJpIM5yBe2F2Nez0e8OlJmw2CRZ3oLdPWM8MZIpGkrWh9ab40EmqlBAZcf3qEqK8HdH6i9tbttSS/AsptXheO5NYwLA1zc/lDwyI3vdIjotLtfy9WWd7YZ5ErFvD+om1AjpQIaleKS8zJQdqYhHcq5zTnObsrR0phlYUOG+XXpfa4be030e0CQimX21h9SK/8kGpyO/j/YD2e8MFhi/UCR7r4CT/fmWbUzT38p0vqU9RlrVMJ0oOks8dDAiHh/vzf6w+FU1PH6VefmUZWLb3zYPrI80XnGEmPLAO3tpnXxYums7PEL7lh9qRBc49Frw9qGWvLDqI+jkcibsld7Wl2a8yfVcPbEHOc152hpyO5xucgrUtnL5QgIfbio2Ip4FFUI92KI5/oLPLkzz9M9eZ7aMcKmoRIpI64mMF6CICRbSzTcP2xUb1Pxd2y4eul9AK0davc2c481xo4BOtTSJgnhb1+zSNFrBD4Z1E+cpSODqHNRPnZWETO/Ps15zbVcMKWG902fQC6oKGsKTjVR4Cpm2/EAhcQaULCya+vIx8qDWwd4fNswXT0jbBgoIuBzgXVibSjZWuLh/lcU/YYgt2+45szngT3m6ljj2M+halUW6ynf3pxJ1w6+H2N/39ZOeA/5IdTH5WKsgVPMvPoM5zTluGxWPe+eXl/5OkSqFc39+CH6gVBlBqcQioy6m36ydZAVrw7wdE+BjYNFjOAzgYnF2BS5WtzgwI/F+H8pDvY+8PLHLimOejuPsW5wbOezXU1Vy537g2dnWScfF/V/aTLZUEuFUuw1LHnMjFzI4qYs1502kfdMrwMS2xwFa45/oh8ICriKXlL1Ofx46xDf39zDs70FtuYjUgYfGhNJKpv2UaGsIn/jSvrtjR8+4xVgjzk8Fjh2c9vebli+3NPRYeey6HwT2C8ENXWX+qGBSFVNWbE5a5hXl+bD85q47rQmICG8cGKs9kNFVSoouxjhts29fO/FXjYOlcg7T0pwIuJN3YQwHh68XzE3vujWPkVbmxudy2OAYzrHF3Q8nu2xDa2i7iabzkzRYqEUQ8orMikT8P4ZE/jTMyZTHwa4imvW7u2if5th9+ccjGL+17Pbue+1AXYWY4ygAZQlm027QnGbGj5bcEOdr7btFYA6ijj6sy2AwsyOx7M1UnsjIp8lcb07D4GIsnBCls+dMYWLptRRjcAczOZ+O6KyMyACj24b4qZnt7F+oICqYCAWwaogzpivlKKBv3617cICIolSdBRxdKddVRDRZd9d1TyYDr9uwkwbUdkBximStsIHZjXw+bOmUROaYzCAEw9Vco5Eni8+8zo/eqWfklNsogJ6Uinro2JHw3D4qad+p6WnOsdH6/5H5ifdHe3tBhGdf/uaOYPpVKfN5FqJyt4KtuRUJmUsn186jS8vm0FNYCrOml+iOg81geHLy2bwV2dNY1LGUnIqVrCUy96msq39NXHHgo7VpyGitLcfNbodHRqoGkT8nDtWL7Fqv23T2XO1VCAU6I8850ysoX3pNM6amH3b7/FHCqfKMz0Flq/eyi968jSElkgVSWdxpcIqJ+5jm65eurY650d6vyPnpNZWi4if9711Z4dqbgnS2XO1XCIUtD/yXDZrAl+7cBbnNOewIsdnUPw4gZIoh+c05/jahbO4bNYE+iNHaFAtl7Dp7LnGyy0tHWuXIuJpbbVHes8jY4D2dkNnp1t0x+olNuW+JencUsolQrz2R15+a+5EvrJsBrNyqdEH/OX6PzAq+jMAs3JpvrJsBtfPnUh/yUmIV8olgkzNUmf8txfd0b2Ezk53pNvBW6dHRRmZf/uaOXj9fpCrOVfLJQ28Y9CpfGLBJP6gZTKNKYuHX+75h4Fq+pAB+sqOf+nexrc27KTOisbGIqmUxIWRpyTFtRt+Y+nmI1EM3xpNKjdc9B/dE12t77SpzCWUSxrgGYxVPrmwmd8/fTITUhanYA9ylwNaNWMY3DkcHO4sV5/hII95QFTnbqDs+Ppz27l5/U7qA9EYgVRaXKHwkA2C1uc/9Natg7cwx4mP+pSHX0qnegb/w6Zr2rRc9CFqBiLPJxY08ydLplAfWmKF4AB38KrEzmHMvhJMSBhDBOx+3h8veKUaFN4vDvjOaAbzHj9Gr7l7AGlvVOdwMHL877Xb+OaGnTSEhrKiks6KKwx3aG3uoy9ePr9cufphMcGBsyj2+yBVLlNJ7xj6a5PJtGmp4EPB9Eeej8ydyJ8sTojvDkJ8SFy9qeDNb++9IsdJPMBUEwwOFwf5ysGkIyRz6BTqQ8ufLJ5CwSnf2dhDY2gkKhW8TWfb3EjxJZQ/R0QPVxIc+tNU899B5n1/9e/YIPgm4K1gByPHZTMb+Oq5M2lM28TL9SYifMfQMN95YhXbB4f2ec+I4EslLlu2lF9fOBenOm7mY1VxHYk9j74xxOrePBlr98kqhn0TzXdPStk9E1l1V4JJcybgylkNNKUPvBiq+QdGoK/k+NyqV7n31X7qA4sTnIJR7z/2wtozb+HGirw5RCY4VAkgtHUaOtvc3O/+/IIgV/dF9V5RNYVYWdac4/NLp70p8T2JYlMol7np3p/w93fcA1G07wethZE8D2zYxHdv+CiLpk0hco7QHrHVc9iobkUjkee/Xunn2xt2UhcGOL+vCX6oyy5hisQkzljYXnD8QcskMtbs11KqMpFXaEwnDrVthTJreoukrRgRUQlSX5p3etf6F+XclbR2WJLpftMhHdqyqkSjFt713HS8ftOE4aWuWIw9GkxKh3z912ZzdlPuTVe+V8WI8Fr/AC1/+bcUo4j9CncBay35/gGuf+dF/Mv1H6KhJgck0mEs4Ssrr6cY8w/rttGxuZfawI4GdY4UCmQDwz3vnc/kTDB6vwN9tioJftGT59OPb2FnKcIgsc1mAx+V76Ugv7f++tO3HmoE8c0lQMXjdMq3N2dcqe9/BBOaL/VDfWVFUmlj+KMlkw+J+HvDqyd2Hn+AiTSxI8xkuLtrNWfOnM5nL383sXOIteOiDyhJqLpcSRs/WgxwOCru7pLgnIk5/njxZL64+nXK3ge+OFKWXMNlLuq/Yd6KDX/74uULSofiLXyz+8vFDz9sUJV0qv9SG6Q+o4P9sUdSgnDF7AauPa1plGsPhzDVCp0Dwaui1jIyOMy3fvY4Dz//AqG1OHdcpNMfNVRX9aEiyZVImODaOU18YHYDIHiVlI4MxMbaz5re4UtRTWj3JmQ5OAN0dJhHLrkknnNP9zzJpj5lUpmUSqJ1LJqQ4fNnTT2oyDoYDuWZY+ewtTVseOkV2u9aQV8+T2AN/m3mT34rj1Nlgs+fNZWF9WmcgiKYVCYl2fSn5tzRPe+RSy6J6eg4KI0P/Kaq0NqqdHTYsOSvNTX179VSPi57H0zKhHzmzCnkQnvM4/jOeySX5dH1m/jM9/+L2Ht0PwrYyQgjUBNaPnfWVCZlQsreB1rKxyZX876QuI2ODktrq+5mwe17jQO9sewbXQEifoEsfJ+in2RkSGPF5gLD+2fU844pdcfmqfaCqiIiEEfc/fSz/PtjT2KtOaDucDLioil1vG9GPdnAECtWC3lVMZ9cGJ7xHkT8sm90HVDX2z8DtKvpuuHcqOVra2sNcnVQ1zhL1UUlp3ZuXYbPnDElMY+O2SPtCa+KSaXY2dPLTff+mA3btu/fejgJIQAKnzljKnPq0pScWtRFQW3DbBNH1yy7e1Wu64ZzI9p1v7Te7x9bFq8LAKIp5moPH9b8EMXYBzNyIR+Z20RdaMc8rOtVIZXihdfe4NO33I4I+7XFT0Z4oD40/PbciUzPhRRjH2h+CAcfHonS18Aumu6NfRlA2033us74zDtWTxb0yqC+MafORV4xSxpz/OapjcSV4oyxhlTszEfWv8hf3fkjrDH79cidbDCSmKi/eWojixuzOMWoc1FQ31jjlSsX3P18c/e6zhjdN3S8zx9aOlsDli/3ecfV4C6nMEw+dnZOfZrr5zaRMjJu4lcBCSxRqcy/PvQYP33uhdG/n+wQhJQRPjJnInPr0+SdsxSGQfUDphx/iOXLfUtn6z5SYE8GUJXudYvjmR2PNxkxl4X1DVmNo1hFzLLmHBdNqSXy+qYBjGMJVcAa+gaH+ONb76AvX/ilFCAJKkVeecfUWs6ZmENVjEZRHNY3ZFF/xcyOtU3d6xbHe1sEezDAsi4ClouvMdmrUH8R5SJ5B/Pr01w5q4FwHFf/HhBBrOHZl1/h//3+XYjImzqWTgYISReTK2c3ML8+Td4D5SLeu4tqrV7BcvHLuvb0/u7BAF2b8K0dHRaCq2xdQyPlkis7H5zbXMMFk2qIvBIcJ+F5RZAg5D8f+zn/8fhTo9G2k1kaBCYpkf/VSTWc01xDyflAyyVn6xoa1fnfUFXp2sQemvMoOVs7OizrbtS1svBC7/1Si2rBqZ5al+bXp9SRPsJOG8cC1VXffvvddG99A1OJsJ3MTKBA2hounlLHaXVpik7VAqp+6eLOZ36NdTdqu+4yCUd/2TRnjmH5cu9UfsukMjMojDBYju35k2q4cHINTo+f1b87jAhbtu3kL+64h/58YZQJTlYEJkktv2hKDedNqmGgHFuKIyrp3MxY5LdYvtzf09U1GlevklSuWLbMLbqzeyLC+UEmk4q9usZUIGdPzNKYDiqlTMfB/r8XvCphTZZ7Vnbx9Z8+SjmOEyY4SaWAVJobNqQDzm7K0pgOJPbqbCabAnNeS8fapjmbllXzdCsM0Nphlot45/wVoDMkKjEYOXNucw2/0lw7mqF6vCJ2nrAmy1d+eD8PdK9PChFVj3YZ3QkDQ7IVXDCplnMn1jAcOWOiIoKf5dErOtvE0ZoEiSoM0Fr5anyFBMEkiSMtOy9nT8yxcEImqdM/jit6tNKdI18o8uW772f9G9sJzMkbLzCS5KAtmJBh6cQcRedF4kjFBpPU8AFglOYVBrhR592yst6ILA0yOS0776fmUtLSmIWKl+k4lP57wHlPkMnw+Jrn+OcfP0qhXMYawb/dYseHAKnQTARaGrJMyaWk7Lw3mZx6/NJpd6/K0XqjAhhQQZZ7SddcjtIgcSz5yMuy5hoWNWRGExBOBJSdI2yo4/978CG++ejKJHTMyekprCbotDRmOLe5hnzkxbhYDKaxrpy9ElnuSfqdVr4A71JMg/iYvHOytDHLnEorNnO8L/8qVPFeMUb4h/se4sfd67FGiN2Y9Fs6rlCl2al1Kc5szFJwTsTHqNJo1L8LAN1dtzP+IpvJBD6OtTa0smBCsvqrrdhOFDjvsWGKzVte5d9++hiv9PYnqWQnWeRQKq5hi7BwQoZcYMXHsdpMJlDDr1U/ZxDRM+56biGYZiNC0ame0ZBjRk1q9EInGiLnCOpruXtlF//04MMAJ6WDqEq7mTUpzmjMUXSqFQVx8sK7nluISOIRKnp3gUJWvKPkvJzemGF6LgROIPG/F7wqGgbc/MjjfOPhxwmMIT7JpECVdtNzIS2NGUrOi3iHQtZ7d0HyGcA4d64oGfGeovOc3pChORNU2tSN3wMcCbxXbBgy3DfIvz783zzx4kvYk4wJKtYgzZmARRMylJwi3pPQ2i2DCgOocp4EQaDek7JGTqlNI1Tq8sbzCY4Q3nuCbIY1Gzby9/f/lHIUV84FOTm2gt1pOLMmhRUR9R4JbKBwNoCZdvOqHMIpYgMi73R2TZqmdOIqPnAu6YkBBbyABpZ7utbw5RUPEFpL+SSSAlUaNmcCTqlLE3mnEgQYldO4eVVoaqbUn4qYtCEJJc6pT1MfJgxwgtMf2JVQWi6V+LeHn+CuXzxL6iSyCqo0bEhZ5tSlib1iEFQke+qUmjlGfLxIwKCeyCuzalLUjTLA24EFQL3HZlJs39nHl+55gC09fcnfx3lcY4EqDetCy6yaVNJS33sEbOjjxSbw0VxVDQQl9jA9G1JX7eH39qB/peGeIIHh6c0vcePd92GNOSmkQJWGdaFhRk1I7EFQVDUwEXOMVzMbSZRBhzI1FxyXyR9HClWFwOKd55bHn+Rbj64kOImyitPWMCUTMHoihWDUmtnGiU4XFUOlxfmkTJhoj+M42GMF9YoEAXEU8z/v/BHPvvp6kk843gM7xqgG/5szQbIFqCKIATfDiPNTVTCoEopIXZDs/15PbBPwYBARtg8M8ie3/YChYult+5xQMQUrUq4+ZQkk6VeiitHYTzVi7URBjVelLjSkDq9r0AkHpXIYpCo/ebabf/7xI+M9pDFDygr1KZv4BkQNxjYZgXpAVJW6MBjtZ/92XhWQMIENAr7wg3v5SaXA5O2KKi1DEerD0f5GYoxOMKquTjTpLZYNdp2meTLAqYIof3BLBy/v7AU4YBaRwgnfl8CKkA0qfYgUUdVao9g0IqKqpIwZ8x484w1V5bnXXufLP3qAgXwx6VC2HyYwBjIneHMKI8kJZ1rpfKVC2hj1o7u+Pcw2L28HqIINQ25+8Gfc/fSzAHtkFVc7cUxMBVw+cwINKYs/Qe0Gwx4SXgxyPGb6jz2890gq5A9vu5Ofb3o5+dtuUqA6Z+dNquGzZ05lJPbHdZLs4cB4MaO+Aaej52OeVEjC3kL/wCCf/8EKNu7owYiMegoTUyrp2vmBmRP44CmNRIk79YSCR5ME3wTqVZ0RXAlVFRHKXk/oPe5I4FUJUyl+smo1//n4U+SjuFJksWsrUKApHfDHLZOZkg2TfgUnELxC2Vda7iTOoKIRsUMqSdSwEO/BIScdYlXIpPnHH97Pvc+sw5gKA1TmpEruOXVpvnD2dGL1J1S8xKlSiBPJpYKKyLDxKgOAighDUTzKACcjG6gqJggYLBRo/697eWrTywTGEO0naPSuafV8fF4zkdfjvgClOrpIlcHIVSWXeu8HDD7uVcQbIwxFnvKYn1+9f4jIuJiksXOEuRzdG1/iH3/yKFv7BxBkn8ihFbhh0WTOaspV6iaObyYAKDtlqOwwRlAVb6DHiDVviOJBiFR1KE5y6Me1ylYEjWK8c+PCBNUqo//6759zyxOrKs0p91SPlaRx81+eNY3aMBhtlXu8QdnVX3mw7IiVpMxX1IvYNwzY11SSzcwK7ChG41oMKgBxzIxJTUyqr0uYYIwPjfCV0PFIuczXH3iYFWu6Ca0l2q3ApGoZnDMxx/9YOAkRoeT0uCyirRaL7izGBEaqncq9R7ca8fEraBL9DRDeyMeUxrEfrzEGnKe5toZPvesd/Or8OfjBQcIwGNNkztg5gtpaXnljO1+850E2vLF9n1QyI4li9TvzJ/K+GfVYSU74OB4lQcl5thXjXf1hFC+eLcZLuFFEYkUIDLxWiBiOkoccN93GGvryRa5Yupi/vfpKpk5sIsoXsMHYnhfg4pggl+Wp9S9y030/pVAu77MVGBGsEf6oZTLzJ2SSSqoxHeXBUaXhUOR5bSQiMJX2OiKxpO1Go6F/XlGPGAIjvDJcZjBKRN14OoUCIwzkC7xz0Tz+6KrLyNkAYj+mtnc1dKw+5q4nn+aWx1ftU2ZWjbfPq8/wyYXNTMxY8vH4dlLbHVUaDkWOV0aSimmMQcEVkW4zsm3TS6gveZJJ3zxU2o0Bxg9WBKeK856PXXgev/OeX8dF0Zgrhc57JJOlp3+Ar6z4MWte2bpP7wErQuSVD85u5IOnNBJK1eEypkPdL6qj7C87Ng6VSBnBo4hq4aVtP9tkXr/hqryK3awuJjRWtoyU6C0lDHD0jih+CxAhNBZrDJMn1PFnl17CxWe24EqlMT9JzDuPyWbYvG07f/i9OxksFPcR81aEWJVPLZrMO6fVJw6X46CwokrDncWYl4dKhMaKxjEe3cwNN0QGQPCrNI5jMYbIeX15uJRYAub4yJcrRhGnNU/kn677IDMnT8JF8Zj2LKiKUQM8uWkzf3PPA4kmvdvkVMfTmLb8zvwmljRlGY7d+DbVZBcNXx0p41UVY9DYxeJ5GqqlYcZ2qVBUY0hbw3P9RXYW49Hz+8YDu9ckVHsCnzV7Jt/6+IdJp4IxH1diGgYURwrc/PDjPLhu/T4iPhAh9spFU+poOzVpqh358TOpq41ddhRjnhsokraCGoMKBQ3MKqgWhxq7UqCgxpK2Rrv7imzNJ6d5jUvsW9njrNnd5/mdC+fxD9dfDW5sFUKoVBynQgaGh/n0LR3sGNr3yLvACB742IKJXHNqIyNR4nkbD1RptzUf0d1XJG2NqrEIFK3kVgIYVGX9B09fD7rDq5KxImv787w2khxEOX4SYE8pUG0HGwYBH/3Vc/l/3n3R+J0cYgwbd/TwqVtu38dFnEhNRRB+e95E3j2jnv5SPC6pdlXavTZSYm1fnowV8Uka3PbnPzR3A6oyKp1E5TFXLMYmCGQkcrp+oIhSTSEa87Hv97TpKhPUZjL8zW9ewa/MPS0Rw2M9QBHUOe76xRq++ehKnNuzF5GtbAXz6tN8ZG4TM2tSRGPcYl81oZ1HWT9YIh87NUEgrlSK1Mt/Vz832v/Bi/mpiPapCchaq8/0Fdg0VEreG4dtYD/0T/5eWUnNdTX8+yeuZ0pdXWIajvUKM4Y4jvls5910b922DxOayuS/e1o9H1/QTD72BDJ22kCVZpsGS6zpyZMNrKoJELQf5acACBgQRduNBH0rEOn3NtRcaLRr5wjP9xfHKRu2qgQcHIumTeHbv/cRwjDAODcuTDAwOMwNt3yfgUIR2NWGxpCcuxUYoe20Rq6d08SOYkQ4RmIgOeganusvsWrnCLnAqA8C9ULvUKb0Q1QNVFrE0HmjrP/gRUN4VvtiXlLWmG35sq7rK4Am2u2YS9lD/NylS07nbz90RXKgZByPeeDIhAFPPP8iX17xIIVyubJgksmylVhBQyrgY/OaOXtijqLXY+7M0t1o1t1fYFsh0pQxxufzYpyufv2qc/N0dgpULZTOTgC8co+6eLsGoaSt0ad78qwfKFZOqxxbDhCRQ9byP/Wud/BbF56HqTLBGEoCr4oJA/71vof4SfcLo6bzrqziJKtoUUOGT58+CaPH3k3sK/bfhoEiq3vyZKxRDUNRH+8QZAUwSvMKA7T5dlUTIveoymsaZqgNrV/VM8LKHcPjWixancwDvbxXMmHAX3/wMn5l3mmJVj7WdY0Cw6Uyf95xFy9s24k1u9LKq+OwAu+eXs8nFkyitxQf062gWgy6cvswq3aOUBta78MMKuYVUy/3tHaopbPTwy4fhd7T1WW725b0euFJVyyUAiO2rxTr6t4CfaW4khQ5hlKgspS8Hpz1RJLQ7eyJTfx9229w6tQp+GIpqeQYI3ivhNkM3Ztf5sa776NneARjzOj5wtXkmow1fGxBM++cWsdw7I4JkyqJtdFXdjzdW6CvHGtgxPpioaTIk92XLundNKermiKwy0k1Z9MmT3u7ManwFl8uvEa2hgmpwD25Y4T/3j5SMW2OwYgPAMEQ2CQWEAbB6Jawv1dgLV6VX513Gn/XeiWzp01GS6Ux3QqiOCZVV8etj63kP59YRbHSkGr3rdOTlGh/5oypNKfDYzKO2Cdm6GNvDPHkjhEmpAJHpgZfLrwq6HdobzdXLFs2mtkymh/Q2dbm6FD74at44rY71j7jkDkZK7J5qMSj24Z4z/S6MXFmqHowhu2Dg/yfnz3Boy9sZLhUfnNiKsTeU5MOyWTS41LI55wjMIZ/XvFjlsyYyntbFhL7xGO5++jPmpjj06dP5kvPbD3qW4EARef52RtDbB4u0pwOxIEYzOrnrl7yuFy9xCwX2ZcBAJbNwSwXiRbd9exdbnjg4jCTbUrHxXjVjpFg5Y4R3jm1jthzTE8O8UkEgx0Dg/z7/Q9BFO9Kyj+kC3iozUEmPeaKq6vUFry09Q3+bsWPWThlErMnNhF7T2D2bLn5W/OaWLVzmHtfHSR7lPJcEtoIj74xzC9682SsjUmlAzc80Guwd4uILrtZTRfsnwG6lhHTriYfPPvDTGyuJpW5Klcs8sJgiXu29PNrk2srnzz2kkCsJaivO2wxriQ6wXilakdxTKphAg+ueoabpk3hK1dfSS6dSqyF3UrvLcJfnTWd9f0ltpfio2JmK0rk4Ydb+tkwUKLOCprKYkrFx/Il/WFyJDB75H3vuZZFtGXxumDLFWf2+cjdFw32FyQMAxH1XT15Ht02TGiS4+OPNVSVyDlKcXxYr3Icj3ueflxJJfvOoz/nOyu7kEqZ2R6jEpiaDfmzM6fSkLJHXJDjKq7fR98Y4hc9eYzgJUwF8UBvAc+PtnzkzL7k+Ng9szz2EebdrZ0x7e2m1vrbgRVka8lZ6zYNlvjexh7K/uSsHzwceFXEWgYHBvnGQ4/x5KaXCa3F75VKpgLvm1HPB2ZNIGOPrDObopS98t1NvWwaLJGzxpGtAZEfEQR30N5uulsXR3t/b9/dXJb7lsWtweq2c3YYrz+MB/pGxNrQCH5tX4E7X+ojEDlpawgPFZFz2Loann7+Bf7mngfIR9E+Zx1XU8v/uGUKTam33pzTVzx/d27uY11fASt4sTaMB3uHrcjdz3+opadlcWuA7JvjtV91rnvd4higJtvcaYVbJVdHJjDx1nzEdzf2MhQdGxt2H1Tr8o7mawzhnIeaLD9c/Sztd65AVfdpVm0EQitHFMYwwEDZ851NPWzNR2QCE0uuDlFza39P6Q7YRdP9fXdfLBe/7OZVYddVM/I+CG+Ph/u3IDZMW3Ebh4r83bPbRjtRHztUZuQEJX4ydE3yF6OYW3/exW1P/mK/h1ccyWLSygX+/tk32DRUIm3FITaMh/tfxnDH6zecm19286qQ5bJfL86B750cMix0dspCs/AvTF3DF3VooFx0mpqSTfF358/goil1hzX4qib8Wv8AC/7iixTK8T6NGq0xuFKZM06dxU1tH+RX5s4mij32CBzoziuhtWzp6eVT3+ngiec3IYEdM2XRGoMbyXPOovn84A8+wYyGhuRMn6PgV1HgsW1D/NmTr7GtUCZtpWxqJ6T8UP9frtf1XwWgtdXvT/zDwdLVRJTOTqGtzcWhvU1Hhu+XdC5IGRPvKEbctOYN8pE/ZrpAGARMrq+lMZdjcn0tE2tq3vJrcl0tjbksZ82awT9edzXTmhrHNJ3MqUI6xdMbN/OZ79+FNYI7ChPnFYYjz1efeYMdxYiUMbHJ5AI/PPRArLaDtrZKeveB87sP7tJpa/MXP/RQsPGqxRs1jv/VR6WSkESzNgyU+OIzW0d76Bwu3mz6VZVSFCXmYOwOGhA6lFe1oue802bzjd+9FoBwrOIFqkhgUeDuZ9Zy070/SQpOjyClzWuiP3zxma1sGCxV+zupL5dKxPHXNrYu3njxQw8FtLUd9CZvNgP6yDvf6RHR4kjvAz6O/05qGgIjWlaUe7b0c9um3lEmOBw+2Ns9utdtd33mKL12dyh94KzFfP23W4nK0Zjl6qlXJAwp5Yv8049/xhMvbn5LUqiaoGMEbt3Uy4ot/YBi0LKpqw+9i29y9bkHEElo9yZkefMlIOJpbzcvf+ySoolTN2tp+F6TqUkJEpc8/PO67VXHQ6JrHcJDCGDFEFhDOgj2fYUBqcAix8BPXsUn3nEBf3bFe4lLZVLGkA73M46j/EoFAZl0mp19/fzVD1YctuRUkjk2Al09ef5p3XZKHgSJTbom5UdGVlgT3fzi5QtKtLcbZP+K34Hm5OCfa+0wdLa5+Z2rf8UE6dvxbjqqUnYqZzRl+NqvnsKsmtRonfz+LuxJOK5QLnPjXfdx0533QLm854eMgWKJWYvm890bPso75s8hdp7gKHYwryacDeQL/O63vsddD/83xPEYhZAFnAOUay99D9/+xPVkU28eGdyd+FuGy/z+ypd5trdI2qKIURHzmivmW1+4/tyf066G5bJ7nurBRnOI0Eqd043I/MWrPyo2/KagahU7GDsum9nAV8+bSWPKHpQJqtg5NMx3VnaxbXBwj2GKCD52zGxu4jfOXsKspsY9/OhHC6pJs6SN23fyn48/SakcIWPAALtnO4bW8rkPvJeaN2GA3YnfW4753JOvct9rA9QHFic4RUTj8sdf6P6vW7jxxur+eUjy5TAjLSqIKKoy/461X7Zh6nMalX1oMP1lz/VzmvjLs6ZRn7I45bipkD0QjgVjHS4OJf21OpeDZceXntnK9zb10piyRB4vYcq4cvkrL1yz5H9WaXOoxIe9ooFvChEFFQTCtUu+EHWvOc1mc21RqagNoZHvbewhFxj+ZMkU6sODM4FXJd5P94/dP27EHNNE32ovwN2JMJbuInMIfZB2J/7/XreNWzf10RRayh6VdMb4UuG2UM9oTz59eMSHt+qESriMRXd2NzkXd9hM7l2USwR4HYxVPrmwmU+fPpmGQ5AEOvrPviMbq7V5wDEca7zJM1bnbqDs+Fr3dr6xfif1KdFYRUilccX8T60N257/UEtvUgh4+PXcb32OK6Jm4b3PnepHog6bzp1HVNIAx2BZ5RMLmvmDxVMSnYADF3r8Evuiqr0ZoK8c8y/d2/nmhp3UB6KxWAhT4kv5J8Wmrl3/m6e/dLhif3e8da1HRGlvN+svO/0lm/Yfd6X804RpcVipTxn9/zfs5CtrXqe3FGOohD/f8s1OHlS3IwP0lmK+suZ1/s/zO6kPjMZihTAtrlB4Wjy/t/43T3+pYu695ak98kXZ2mrp7HTzO9YuNUa/bdLZpb5cJBS0rxTL5TMb+PzZU5lVk97jAX+JfbH73LwyXOKLz7zBilf7aUwFGqmIpNLEhfzqFO5j3W1nr67O/ZHc8+jQQtUg4ufc1r3E2uhbNpM7TxMmoL/sOac5xxfOms7Sidm3TZftYwWnyjM9BW5cvZVf7MzTmDJECpLO4Ir5p5wLP77pupa11Tk/0vsdHcO34i3cdF3LWhOk2lyp8FMJ0xp5fH1oWNNT4PdXvkzn5j78+ERmj3uoJi7ezs19fGrlyzzbW2BCQnwvqbS6Uv6nJk61bbquZe2hevkOBUfbuyKI6AX3bWnqHe7/uoTp64jKDjBOkbSFy2c18PmzplNbPZzyqA7gxEN1LQxFni89s5UVr/RTcmClct5lmLIalW5rqm349MpLZ/ceicK3Pxz9+a+0Ip/Z8Xg2E074vHH+zytFPs5DICgLJmT53JlTeMeUutH4wTg10Rg3eK1YRgKPbhviq2u2sWGggCaneMYiWAW8NV8pRgNffLXtwkJ1bo8mjum0z+x4PJszddcAN9lMdqoWRkqxSsqBTMoEvG/GBP70jCk0hHa0jOrtriPs/pz9kePv12zjwa0D7CjGWNBAtCzZ2rQrjrwO8rm8H7r91bYLC8dqPMduttvbDcuXe1TNvDvXnC/YLwS5usv8yGCkqqbs1WYDy5y6FB+ZN5EPn9YEJD37hcRL9nZhhSSEm+QjVMPPt27u5bsv9rBpqEwhdqSMiUVQW1sfuvzQCu/kr1+4puWpqn7F8uXHpDDv2M5xEpXyAHNvfXKWTeV+V9T/lUnnUloqlCLvw7LHTM+FLG7Mct2cJt47vR6oHN6gYM2JywhKko6G7CL8g1uHuG1TD+v6CmzNR6QNPjAmknQ27UuFsop8yZXz/77xw+e/Auwxh8cCY1HsV6Gg6LwVG9K2VHqfev7Q5urfo8U8+Lhcin0QK2ZufZpzmnJcPmsC764wAspof50TQSpUV7vT5KDG6o724NZB7n1lgKd7C2wcLBIIPh2YGBOkTDaHyw89iNd/cbn0Ay9evqA0Gn09igrf/jB289mhlrakKHHB7WsWqYs/ZGxwQ1DXONvnh8C5aCR2VhGzoD7NOc05LpxSx3un11NTKUb0muyhxxszVInuNdnbqwptPvbcv3WQJ7YN0bUzz4bBIgZ8TWhjjE2ZbB3xcN8WD/9mgvDOpFsbe8zVscbYzmF7u2ldvFg6K8mKC29/9v1e3TUi9tqgrqGOwjDq4/JI2duS93ZOXYbzJtVwdlOW8yfV0NKQ3eNyUaUfr0HeNP/gaKIan/fJIcz7VPh29xd4cscIT/cWeGrHCJuGimSMcbmUcWLCFNka4qH+IbzeJkbuWH/NGfcDtHao7Vx3ox6r/X5/GJdFdHH7Q8EjN77TIaIzH9+Szb7W1ybIlRh5f1jbUEupgEbluOScDJS9mZAK5LxJOc5qytHSkKGlIcPc+sw+g48r+20SeKruPG/9IatRwiQ4o6P/D/YiuAIvDhZ5rr9Id3+RZ3rzPLUjT3851oaU8WlrVVKpgFSWaKhvGOV+RX9Ybj7z+y9fIkVUpbUT0zlGq353jJ8UVZVl3+gKum44NwKYfc+axkxJP4ya9wtygc3lJguCKY4QeY2HYmdKsZfJ2VDOn1TDGY1Z5k/IMDuXZlpNyOTMgVMbvO5qm/am2ciVnwY5qG9iezHm9XzEKyNlNgwUeLavwJM78mwvRJoOROsC60Mjgc/UoKq4Qn67oivx7r5iZG7b8pEz+wCW3bwq7PrksvhY7/UHwvhvo+3tpmVxa9DdtqQMMO3u13L1Uc+14vUDGLtElYW2th4TFZE4InLejcReCrGTXGBlSWOOlsYMixoyzK4JmZgOaUhZ6kNLTWjIHmEuYdF5hiPPYOToLzt6ShGvDEc8P1BkXX+RZ3vzFGKn2cBqTWA0tMZqEOLDDG54EBHW49xaRX40mJn4/devmpEHaOlYm+pe1xmPpbjfH8afAaqoMgLdrlrQMP/2Z34dzIcFzhevMyUIJptsDhNHiIvxLtaiUy05LyWnWCMyuzbF3LoMs2pDpudCpmZDmjMh9YElbZNTUaqKWrVlg0dHFczYQ8kpg7FjZzFiWyHitXxC9I1DRV4eTrpup42QtkYzVsTYQNQG+CDAFwqoi7eryKsKT4K/9YVrzvoZAB0dtoUWezwQvorjhwGqqGwNtQuW6SOXSAzQ0rG21itXq/GXepGzjWqTIo02kwmMCOIdoh71ntg5LXsl9okpFidEFSswIWWpCy250BDKroOUY1UiVfKRZyhyDJQdTiEwqBUhkGTfD40QGitiTNJ12yTlZb5YjBH6PPQa/DN47rX03t7ddskwwMUPaTC8oUvGU9QfCMcfA+yCVJjBdm1d5qrOkGV3v5YbKvVcAbxbhIuASQpZVDImsIHYoKIAVtrFVX3n1eqgikdu75rEaqHKaBFJhTm08rtiEkXQxWjsYhUtChQQ2Y7Tx9XITzJ1zfesef+0EQDa1Syb3mW7PrnMVYh+XBG+iuOZAXaDCu1I62Kkcx26u2ds0Z0bFzjNXyBOlinxMmPsKQo5AauoFbAoRkWMoIbRjPW9gw5ajUupIl5UPUkbBCeKV5FYlDz4lyF42ltdZTFPPP+hlg2jl2hXs2uM6N7dOI5HnCAMsDd011Eme4vUjg57SjRnfjaVblHMHBU/G3SGeqaATDTGN6hSo5AxmmRFeyEWKIow4r3pN2iPCm+Af12wLyPlzUXCdS/7518YLbgcHUrVYwcnAsH3xv8FqGayYlJOMlYAAAAASUVORK5CYII=".into()
    }
}
