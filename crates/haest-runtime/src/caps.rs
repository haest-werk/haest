//! The `[timeouts]` and `[limits]` settings as what each crate takes: the
//! caps of each kind of JavaScript run ([`code_caps`], [`sandbox_caps`],
//! [`mod_file_caps`]) and the built-in tools' timeouts ([`builtin_options`]).
//! The crates never read settings; a Rust host that builds its own sets them
//! in code, and these are the runtime's. The hook, ask, sandbox-shell,
//! model-headers and MCP timeouts are one setter each, at the place the
//! runtime builds their object.

use haest_code::VmCaps;
use haest_settings::Settings;
use haest_tools::{BashOptions, BuiltinOptions, JobHub, WebFetchNet};

/// The Bash tool's: `bash` when a call names no timeout, and the most it
/// may ask for ([`bash_ceiling`](haest_settings::TimeoutSettings::bash_ceiling)).
pub(crate) fn bash_options(s: &Settings) -> BashOptions {
    BashOptions::default()
        .with_default_timeout(s.timeouts.bash)
        .with_max_timeout(s.timeouts.bash_ceiling())
}

/// The built-ins' options before the task list and Skill: Bash on
/// [`bash_options`], leaving background jobs to `jobs` when there is a hub,
/// and WebFetch on the real network held to `web_fetch`.
pub(crate) fn builtin_options(s: &Settings, jobs: Option<JobHub>) -> BuiltinOptions {
    let bash = jobs
        .into_iter()
        .fold(bash_options(s), BashOptions::with_jobs);
    BuiltinOptions::new()
        .with_bash(bash)
        .with_web_fetch(WebFetchNet::default().with_timeout(s.timeouts.web_fetch))
}

/// The Code tool's: `javascript.code` (the default deadline and the most
/// the model may ask for) and `code_memory`.
pub(crate) fn code_caps(s: &Settings) -> VmCaps {
    VmCaps::default()
        .with_timeout(s.timeouts.javascript.code)
        .with_memory_bytes(s.limits.javascript.code_memory)
}

/// The sandbox `js` command's: `javascript.sandbox` (the shell call's end
/// still cuts it shorter) and `sandbox_memory`.
pub(crate) fn sandbox_caps(s: &Settings) -> VmCaps {
    VmCaps::sandbox()
        .with_timeout(s.timeouts.javascript.sandbox)
        .with_memory_bytes(s.limits.javascript.sandbox_memory)
}

/// Each mod file's: `javascript.mod_load`, `mod_call`, `mod_slice` and
/// `mod_memory`.
pub(crate) fn mod_file_caps(s: &Settings) -> VmCaps {
    let js = s.timeouts.javascript;
    VmCaps::mod_file()
        .with_timeout(js.mod_load)
        .with_call_timeout(js.mod_call)
        .with_slice(js.mod_slice)
        .with_memory_bytes(s.limits.javascript.mod_memory)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use haest_mods::{ASK_TIMEOUT, DEFAULT_HOOK_TIMEOUT};
    use haest_settings::{JsLimits, JsTimeouts, LimitSettings, TimeoutSettings};

    use super::*;

    /// haest-settings cannot depend on haest-code or haest-mods, so each
    /// default is written twice: bare settings must build exactly the caps
    /// and timeouts a host that never heard of the settings gets.
    #[test]
    fn bare_settings_are_every_crates_own_defaults() {
        let s = Settings::bare("/w", "/h");
        assert_eq!(code_caps(&s), VmCaps::default());
        assert_eq!(sandbox_caps(&s), VmCaps::sandbox());
        assert_eq!(mod_file_caps(&s), VmCaps::mod_file());
        assert_eq!(s.timeouts.hook, DEFAULT_HOOK_TIMEOUT);
        assert_eq!(s.timeouts.ask, ASK_TIMEOUT);
        assert_eq!(s.limits.javascript.code_memory, haest_code::VM_MEMORY_BYTES);
        assert_eq!(s.timeouts.bash, haest_tools::BASH_TIMEOUT);
        assert_eq!(s.timeouts.bash_max, haest_tools::BASH_MAX_TIMEOUT);
        assert_eq!(
            s.timeouts.bash_ceiling(),
            haest_workspace::MAX_SHELL_TIMEOUT
        );
        assert_eq!(s.timeouts.model_headers, haest_providers::HEADERS_TIMEOUT);
        assert_eq!(s.timeouts.mcp_call, haest_mcp::CALL_TIMEOUT);
        assert_eq!(s.timeouts.mcp_connect, haest_mcp::CONNECT_TIMEOUT);
        assert_eq!(s.timeouts.web_fetch, haest_tools::WEB_FETCH_TIMEOUT);
        assert_eq!(
            s.timeouts.model_stall, None,
            "a provider has none by default"
        );
    }

    /// `web_fetch` reaches the net WebFetch is built on.
    #[test]
    fn web_fetch_is_held_to_its_setting() {
        let s = Settings {
            timeouts: TimeoutSettings {
                web_fetch: Duration::from_secs(45),
                ..TimeoutSettings::default()
            },
            ..Settings::bare("/w", "/h")
        };
        let shown = format!("{:?}", builtin_options(&s, None));
        assert!(shown.contains("timeout: 45s"), "{shown}");
    }

    #[test]
    fn each_setting_reaches_its_own_cap_and_no_other() {
        let mins = |m: u64| Duration::from_secs(m * 60);
        let s = Settings {
            timeouts: TimeoutSettings {
                javascript: JsTimeouts {
                    code: mins(20),
                    sandbox: mins(7),
                    mod_load: mins(1),
                    mod_call: mins(2),
                    mod_slice: Duration::from_secs(3),
                },
                ..TimeoutSettings::default()
            },
            limits: LimitSettings {
                javascript: JsLimits {
                    code_memory: 256 << 20,
                    sandbox_memory: 32 << 20,
                    mod_memory: 1 << 30,
                },
            },
            ..Settings::bare("/w", "/h")
        };
        let code = code_caps(&s);
        assert_eq!((code.timeout, code.memory_bytes), (mins(20), 256 << 20));
        assert_eq!(code.max_log_chars, VmCaps::default().max_log_chars);
        let sandbox = sandbox_caps(&s);
        assert_eq!((sandbox.timeout, sandbox.memory_bytes), (mins(7), 32 << 20));
        assert_eq!(sandbox.max_log_chars, VmCaps::sandbox().max_log_chars);
        let m = mod_file_caps(&s);
        assert_eq!(
            (m.timeout, m.call_timeout, m.slice, m.memory_bytes),
            (mins(1), mins(2), Duration::from_secs(3), 1 << 30)
        );
    }
}
