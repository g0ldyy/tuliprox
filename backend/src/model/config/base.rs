use arc_swap::{ArcSwapOption};
use std::collections::{HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::{debug, error};
use path_clean::PathClean;
use rand::Rng;

use crate::model::{ApiProxyConfig, ApiProxyServerInfo, CustomStreamResponse, Mappings, ProxyUserCredentials, ReverseProxyConfig, ScheduleConfig, SourcesConfig};
use crate::model::{ConfigInput, ConfigInputOptions, ConfigTarget, HdHomeRunConfig, IpCheckConfig, LogConfig, MessagingConfig, ProxyConfig, TargetOutput, VideoConfig, WebUiConfig};
use shared::error::{create_tuliprox_error_result, TuliproxError, TuliproxErrorKind};
use shared::utils::{default_connect_timeout_secs};

const CHANNEL_UNAVAILABLE: &str = "channel_unavailable.ts";
const USER_CONNECTIONS_EXHAUSTED: &str = "user_connections_exhausted.ts";
const PROVIDER_CONNECTIONS_EXHAUSTED: &str = "provider_connections_exhausted.ts";
const USER_ACCOUNT_EXPIRED: &str = "user_account_expired.ts";

fn generate_secret() -> [u8; 32] {
    let mut rng = rand::rng();
    let mut secret = [0u8; 32];
    rng.fill(&mut secret);
    secret
}

#[macro_export]
macro_rules! valid_property {
  ($key:expr, $array:expr) => {{
        $array.contains(&$key)
    }};
}
pub use valid_property;
use crate::api::model::streams::transport_stream_buffer::TransportStreamBuffer;
use crate::utils;


#[derive(Debug, Copy, Clone, serde::Serialize, serde::Deserialize)]
pub enum FilterMode {
    #[serde(rename = "discard")]
    Discard,
    #[serde(rename = "include")]
    Include,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ConfigApi {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub web_root: String,
}

impl ConfigApi {
    pub fn prepare(&mut self) {
        if self.web_root.is_empty() {
            self.web_root = String::from("./web");
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub threads: u8,
    pub api: ConfigApi,
    pub working_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_config_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_stream_response_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedules: Option<Vec<ScheduleConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<LogConfig>,
    #[serde(default)]
    pub user_access_control: bool,
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep_timer_mins: Option<u32>,
    #[serde(default)]
    pub update_on_boot: bool,
    #[serde(default)]
    pub config_hot_reload: bool,
    #[serde(default)]
    pub web_ui: Option<WebUiConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging: Option<MessagingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverse_proxy: Option<ReverseProxyConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdhomerun: Option<HdHomeRunConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipcheck: Option<IpCheckConfig>,
    #[serde(skip)]
    pub sources: SourcesConfig,
    #[serde(skip)]
    pub t_hdhomerun: Arc<ArcSwapOption<HdHomeRunConfig>>,
    #[serde(skip)]
    pub t_api_proxy: Arc<ArcSwapOption<ApiProxyConfig>>,
    #[serde(skip)]
    pub t_config_path: String,
    #[serde(skip)]
    pub t_config_file_path: String,
    #[serde(skip)]
    pub t_sources_file_path: String,
    #[serde(skip)]
    pub t_mapping_file_path: String,
    #[serde(skip)]
    pub t_api_proxy_file_path: String,
    #[serde(skip)]
    pub t_custom_stream_response_path: Option<String>,
    #[serde(skip)]
    pub file_locks: Arc<utils::FileLockManager>,
    #[serde(skip)]
    pub t_custom_stream_response: Option<CustomStreamResponse>,
    #[serde(skip)]
    pub t_access_token_secret: [u8; 32],
    #[serde(skip)]
    pub t_encrypt_secret: [u8; 16],
}

impl Config {
    pub fn set_api_proxy(&self, api_proxy: Option<Arc<ApiProxyConfig>>) -> Result<(), TuliproxError> {
        self.t_api_proxy.store(api_proxy);
        self.check_target_user()
    }

    fn check_username(&self, output_username: Option<&str>, target_name: &str) -> Result<(), TuliproxError> {
        if let Some(username) = output_username {
            if let Some((_, config_target)) = self.get_target_for_username(username) {
                if config_target.name != target_name {
                    return create_tuliprox_error_result!(TuliproxErrorKind::Info, "User:{username} does not belong to target: {}", target_name);
                }
            } else {
                return create_tuliprox_error_result!(TuliproxErrorKind::Info, "User: {username} does not exist");
            }
            Ok(())
        } else {
            Ok(())
        }
    }
    fn check_target_user(&self) -> Result<(), TuliproxError> {
        let check_homerun = self.t_hdhomerun.load().as_ref().is_some_and(|h| h.enabled);
        for source in &self.sources.sources {
            for target in &source.targets {
                for output in &target.output {
                    match output {
                        TargetOutput::Xtream(_) | TargetOutput::M3u(_) => {}
                        TargetOutput::Strm(strm_output) => {
                            self.check_username(strm_output.username.as_deref(), &target.name)?;
                        }
                        TargetOutput::HdHomeRun(hdhomerun_output) => {
                            if check_homerun {
                                let hdhr_name = &hdhomerun_output.device;
                                self.check_username(Some(&hdhomerun_output.username), &target.name)?;
                                if let Some(old_hdhomerun) = self.t_hdhomerun.load().clone() {
                                    let mut hdhomerun = (*old_hdhomerun).clone();
                                    for device in &mut hdhomerun.devices {
                                        if &device.name == hdhr_name {
                                            device.t_username.clone_from(&hdhomerun_output.username);
                                            device.t_enabled = true;
                                        }
                                    }
                                    self.t_hdhomerun.store(Some(Arc::new(hdhomerun)));
                                }
                            }
                        }
                    }
                }
            }
        }

        let guard = self.t_hdhomerun.load();
        if let Some(hdhomerun) = &*guard {
            for device in &hdhomerun.devices {
                if !device.t_enabled {
                    debug!("HdHomeRun device '{}' has no username and will be disabled", device.name);
                }
            }
        }
        Ok(())
    }

    pub fn is_reverse_proxy_resource_rewrite_enabled(&self) -> bool {
        self.reverse_proxy.as_ref().is_none_or(|r| !r.resource_rewrite_disabled)
    }

    fn intern_get_target_for_user(&self, user_target: Option<(ProxyUserCredentials, String)>) -> Option<(ProxyUserCredentials, &ConfigTarget)> {
        match user_target {
            Some((user, target_name)) => {
                for source in &self.sources.sources {
                    for target in &source.targets {
                        if target_name.eq_ignore_ascii_case(&target.name) {
                            return Some((user, target));
                        }
                    }
                }
                None
            }
            None => None
        }
    }

    pub fn get_inputs_for_target(&self, target_name: &str) -> Option<Vec<&ConfigInput>> {
        for source in &self.sources.sources {
            if let Some(cfg) = source.get_inputs_for_target(target_name) {
                return Some(cfg);
            }
        }
        None
    }

    pub fn get_target_for_username(&self, username: &str) -> Option<(ProxyUserCredentials, &ConfigTarget)> {
        if let Some(credentials) = self.get_user_credentials(username) {
            return self.t_api_proxy.load().as_ref()
                .and_then(|api_proxy| self.intern_get_target_for_user(api_proxy.get_target_name(&credentials.username, &credentials.password)));
        }
        None
    }

    pub fn get_target_for_user(&self, username: &str, password: &str) -> Option<(ProxyUserCredentials, &ConfigTarget)> {
        self.t_api_proxy.load().as_ref().and_then(|api_proxy| self.intern_get_target_for_user(api_proxy.get_target_name(username, password)))
    }

    pub fn get_target_for_user_by_token(&self, token: &str) -> Option<(ProxyUserCredentials, &ConfigTarget)> {
        self.t_api_proxy.load().as_ref().as_ref().and_then(|api_proxy| self.intern_get_target_for_user(api_proxy.get_target_name_by_token(token)))
    }

    pub fn get_user_credentials(&self, username: &str) -> Option<ProxyUserCredentials> {
        self.t_api_proxy.load().as_ref().as_ref().and_then(|api_proxy| api_proxy.get_user_credentials(username))
    }

    pub fn get_input_by_name(&self, input_name: &str) -> Option<&ConfigInput> {
        for source in &self.sources.sources {
            for input in &source.inputs {
                if input.name == input_name {
                    return Some(input);
                }
            }
        }
        None
    }

    pub fn get_input_options_by_name(&self, input_name: &str) -> Option<&ConfigInputOptions> {
        for source in &self.sources.sources {
            for input in &source.inputs {
                if input.name == input_name {
                    return input.options.as_ref();
                }
            }
        }
        None
    }

    pub fn get_input_by_id(&self, input_id: u16) -> Option<&ConfigInput> {
        for source in &self.sources.sources {
            for input in &source.inputs {
                if input.id == input_id {
                    return Some(input);
                }
            }
        }
        None
    }

    pub fn get_target_by_id(&self, target_id: u16) -> Option<&ConfigTarget> {
        self.sources.get_target_by_id(target_id)
    }

    pub fn set_mappings(&self, mappings_cfg: &Mappings) {
        for source in &self.sources.sources {
            for target in &source.targets {
                if let Some(mapping_ids) = &target.mapping {
                    let mut target_mappings = Vec::with_capacity(128);
                    for mapping_id in mapping_ids {
                        let mapping = mappings_cfg.get_mapping(mapping_id);
                        if let Some(mappings) = mapping {
                            target_mappings.push(mappings);
                        }
                    }
                    target.t_mapping.store(if target_mappings.is_empty() { None } else { Some(Arc::new(target_mappings)) });
                }
            }
        }
    }

    fn check_unique_input_names(&mut self) -> Result<(), TuliproxError> {
        let mut seen_names = HashSet::new();
        for source in &mut self.sources.sources {
            for input in &source.inputs {
                let input_name = input.name.trim().to_string();
                if input_name.is_empty() {
                    return create_tuliprox_error_result!(TuliproxErrorKind::Info, "input name required");
                }
                if seen_names.contains(input_name.as_str()) {
                    return create_tuliprox_error_result!(TuliproxErrorKind::Info, "input names should be unique: {}", input_name);
                }
                seen_names.insert(input_name);
                if let Some(aliases) = &input.aliases {
                    for alias in aliases {
                        let input_name = alias.name.trim().to_string();
                        if input_name.is_empty() {
                            return create_tuliprox_error_result!(TuliproxErrorKind::Info, "input name required");
                        }
                        if seen_names.contains(input_name.as_str()) {
                            return create_tuliprox_error_result!(TuliproxErrorKind::Info, "input names should be unique: {}", input_name);
                        }
                        seen_names.insert(input_name);
                    }
                }
            }
        }
        Ok(())
    }


    fn check_scheduled_targets(&mut self, target_names: &HashSet<String>) -> Result<(), TuliproxError> {
        if let Some(schedules) = &self.schedules {
            for schedule in schedules {
                if let Some(targets) = &schedule.targets {
                    for target_name in targets {
                        if !target_names.contains(target_name) {
                            return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Unknown target name in scheduler: {}", target_name);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /**
    *  if `include_computed` set to true for `app_state`
    */
    pub fn prepare(&mut self, include_computed: bool) -> Result<(), TuliproxError> {
        let work_dir = &self.working_dir;
        self.working_dir = utils::resolve_directory_path(work_dir);
        if let Some(mapping_path) = &self.mapping_path {
            self.t_mapping_file_path = mapping_path.to_string();
        }

        if let Some(mins) = self.sleep_timer_mins {
            if mins == 0 {
                return Err(TuliproxError::new(TuliproxErrorKind::Info, "`sleep_timer_mins` must be > 0 when specified".to_string()));
            }
        }

        if include_computed {
            self.t_access_token_secret = generate_secret();
            self.t_encrypt_secret = <&[u8] as TryInto<[u8; 16]>>::try_into(&generate_secret()[0..16]).map_err(|err| TuliproxError::new(TuliproxErrorKind::Info, err.to_string()))?;
            self.prepare_custom_stream_response();
        }
        self.prepare_directories();
        if let Some(reverse_proxy) = self.reverse_proxy.as_mut() {
            reverse_proxy.prepare(&self.working_dir)?;
        }
        if let Some(proxy) = &mut self.proxy {
            proxy.prepare()?;
        }
        if let Some(ipcheck) = self.ipcheck.as_mut() {
            ipcheck.prepare()?;
        }
        self.prepare_hdhomerun()?;
        self.api.prepare();
        self.prepare_api_web_root();
        self.sources.prepare(include_computed)?;
        let target_names = self.sources.check_unique_target_names()?;
        self.check_scheduled_targets(&target_names)?;
        self.check_unique_input_names()?;
        self.prepare_video_config()?;
        self.prepare_web()?;

        Ok(())
    }

    fn prepare_directories(&mut self) {
        fn set_directory(path: &mut Option<String>, default_subdir: &str, working_dir: &str) {
            *path = Some(match path.as_ref() {
                Some(existing) => existing.to_owned(),
                None => PathBuf::from(working_dir).join(default_subdir).clean().to_string_lossy().to_string(),
            });
        }

        set_directory(&mut self.backup_dir, "backup", &self.working_dir);
        set_directory(&mut self.user_config_dir, "user_config", &self.working_dir);
    }

    fn prepare_hdhomerun(&mut self) -> Result<(), TuliproxError> {
        if let Some(old_hdhomerun) = &self.hdhomerun {
            let mut hdhomerun = (*old_hdhomerun).clone();
            if hdhomerun.enabled {
                hdhomerun.prepare(self.api.port)?;
            }
            self.t_hdhomerun.store(Some(Arc::new(hdhomerun)));
        }
        Ok(())
    }

    fn prepare_web(&mut self) -> Result<(), TuliproxError> {
        if let Some(web_ui_config) = self.web_ui.as_mut() {
            web_ui_config.prepare(&self.t_config_path)?;
        }
        Ok(())
    }

    fn prepare_video_config(&mut self) -> Result<(), TuliproxError> {
        match &mut self.video {
            None => {
                self.video = Some(VideoConfig {
                    extensions: vec!["mkv".to_string(), "avi".to_string(), "mp4".to_string()],
                    download: None,
                    web_search: None,
                });
            }
            Some(video) => {
                match video.prepare() {
                    Ok(()) => {}
                    Err(err) => return Err(err)
                }
            }
        }
        Ok(())
    }

    fn prepare_custom_stream_response(&mut self) {
        if let Some(custom_stream_response_path) = self.custom_stream_response_path.as_ref() {
            fn load_and_set_file(file_path: &Path) -> Option<TransportStreamBuffer> {
                if file_path.exists() {
                    // Enforce maximum file size (10 MB)
                    if let Ok(meta) = std::fs::metadata(file_path) {
                        const MAX_RESPONSE_SIZE: u64 = 10 * 1024 * 1024;
                        if meta.len() > MAX_RESPONSE_SIZE {
                            error!("Custom stream response file too large ({} bytes): {}",
                                   meta.len(), file_path.display());
                            return None;
                        }
                    }
                    // Quick MPEG-TS sync-byte check (0x47)
                    if let Ok(mut f) = File::open(file_path) {
                        let mut buf = [0u8; 1];
                        if f.read_exact(&mut buf).is_err() || buf[0] != 0x47 {
                            error!("Invalid MPEG-TS file: {}", file_path.display());
                            return None;
                        }
                    }

                    match utils::read_file_as_bytes(&PathBuf::from(&file_path)) {
                        Ok(data) => Some(TransportStreamBuffer::new(data, )),
                        Err(err) => {
                            error!("Failed to load a resource file: {} {err}", file_path.display());
                            None
                        }
                    }
                } else {
                    None
                }
            }

            let path = PathBuf::from(custom_stream_response_path);
            let path = utils::make_path_absolute(&path, &self.working_dir);
            self.t_custom_stream_response_path = Some(path.to_string_lossy().to_string());
            let channel_unavailable = load_and_set_file(&path.join(CHANNEL_UNAVAILABLE));
            let user_connections_exhausted = load_and_set_file(&path.join(USER_CONNECTIONS_EXHAUSTED));
            let provider_connections_exhausted = load_and_set_file(&path.join(PROVIDER_CONNECTIONS_EXHAUSTED));
            let user_account_expired = load_and_set_file(&path.join(USER_ACCOUNT_EXPIRED));
            self.t_custom_stream_response = Some(CustomStreamResponse {
                channel_unavailable,
                user_connections_exhausted,
                provider_connections_exhausted,
                user_account_expired,
            });
        }
    }

    fn prepare_api_web_root(&mut self) {
        if !self.api.web_root.is_empty() {
            self.api.web_root = utils::make_absolute_path(&self.api.web_root, &self.working_dir);
        }
    }

    /// # Panics
    ///
    /// Will panic if default server invalid
    pub fn get_server_info(&self, server_info_name: &str) -> ApiProxyServerInfo {
        let guard = self.t_api_proxy.load();
        if let Ok(api_proxy) = guard.as_ref().ok_or_else(|| {
            TuliproxError::new(TuliproxErrorKind::Info, "API proxy config not loaded".to_string())
        }) {
            let server_info_list = api_proxy.server.clone();
            server_info_list.iter().find(|c| c.name.eq(server_info_name)).map_or_else(|| server_info_list.first().unwrap().clone(), Clone::clone)
        } else {
            panic!("ApiProxyServer info not found");
        }
    }

    pub fn get_user_server_info(&self, user: &ProxyUserCredentials) -> ApiProxyServerInfo {
        let server_info_name = user.server.as_ref().map_or("default", |server_name| server_name.as_str());
        self.get_server_info(server_info_name)
    }

}


