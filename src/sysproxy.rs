//! Модуль управления системным прокси Windows (Internet Settings) и переменными окружения.

#[cfg(windows)]
pub mod sysproxy {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x08000000;

    #[link(name = "wininet")]
    extern "system" {
        fn InternetSetOptionW(h_internet: usize, dw_option: u32, lp_buffer: *const u8, dw_buffer_length: u32) -> i32;
    }

    #[link(name = "user32")]
    extern "system" {
        fn SendMessageTimeoutW(
            h_wnd: usize,
            msg: u32,
            w_param: usize,
            l_param: *const u16,
            fu_flags: u32,
            u_timeout: u32,
            lpdw_result: *mut usize,
        ) -> isize;
    }

    const INTERNET_OPTION_SETTINGS_CHANGED: u32 = 39;
    const INTERNET_OPTION_REFRESH: u32 = 37;
    const HWND_BROADCAST: usize = 0xffff;
    const WM_SETTINGCHANGE: u32 = 0x001A;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;

    fn notify_changes() {
        unsafe {
            InternetSetOptionW(0, INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null(), 0);
            InternetSetOptionW(0, INTERNET_OPTION_REFRESH, std::ptr::null(), 0);
            let env_str: Vec<u16> = "Environment\0".encode_utf16().collect();
            let mut res = 0usize;
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                env_str.as_ptr(),
                SMTO_ABORTIFHUNG,
                1000,
                &mut res,
            );
        }
    }

    /// Устанавливает системный прокси Windows (127.0.0.1:2080) и переменные окружения
    pub fn enable(proxy_addr: &str) -> Result<(), String> {
        let mut cmd = Command::new("reg");
        cmd.args(&[
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v", "ProxyEnable",
            "/t", "REG_DWORD",
            "/d", "1",
            "/f",
        ]);
        cmd.creation_flags(CREATE_NO_WINDOW);
        let _ = cmd.status();

        let mut cmd2 = Command::new("reg");
        cmd2.args(&[
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v", "ProxyServer",
            "/t", "REG_SZ",
            "/d", proxy_addr,
            "/f",
        ]);
        cmd2.creation_flags(CREATE_NO_WINDOW);
        let _ = cmd2.status();

        let mut cmd3 = Command::new("reg");
        cmd3.args(&[
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v", "ProxyOverride",
            "/t", "REG_SZ",
            "/d", "<local>;localhost;127.*;10.*;172.16.*;192.168.*",
            "/f",
        ]);
        cmd3.creation_flags(CREATE_NO_WINDOW);
        let _ = cmd3.status();

        let proxy_http = format!("http://{proxy_addr}");
        let proxy_socks = format!("socks5://{proxy_addr}");

        let vars = [
            ("HTTP_PROXY", proxy_http.as_str()),
            ("HTTPS_PROXY", proxy_http.as_str()),
            ("ALL_PROXY", proxy_socks.as_str()),
            ("AG_LS_PROXY", proxy_http.as_str()),
        ];
        for (var, val) in vars {
            let mut reg_env = Command::new("reg");
            reg_env.args(&[
                "add",
                r"HKCU\Environment",
                "/v", var,
                "/t", "REG_SZ",
                "/d", val,
                "/f",
            ]);
            reg_env.creation_flags(CREATE_NO_WINDOW);
            let _ = reg_env.status();
        }

        notify_changes();
        Ok(())
    }

    /// Сбрасывает системный прокси Windows и очищает переменные окружения
    pub fn disable() -> Result<(), String> {
        let mut cmd = Command::new("reg");
        cmd.args(&[
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v", "ProxyEnable",
            "/t", "REG_DWORD",
            "/d", "0",
            "/f",
        ]);
        cmd.creation_flags(CREATE_NO_WINDOW);
        let _ = cmd.status();

        for var in &["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "AG_LS_PROXY"] {
            let mut reg_env = Command::new("reg");
            reg_env.args(&[
                "delete",
                r"HKCU\Environment",
                "/v", var,
                "/f",
            ]);
            reg_env.creation_flags(CREATE_NO_WINDOW);
            let _ = reg_env.status();
        }

        notify_changes();
        Ok(())
    }

    /// Проверяет, включён ли системный прокси в реестре Windows
    pub fn is_enabled() -> bool {
        let mut cmd = Command::new("reg");
        cmd.args(&[
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v", "ProxyEnable",
        ]);
        cmd.creation_flags(CREATE_NO_WINDOW);
        if let Ok(out) = cmd.output() {
            let text = String::from_utf8_lossy(&out.stdout);
            return text.contains("0x1");
        }
        false
    }
}

#[cfg(not(windows))]
pub mod sysproxy {
    pub fn enable(_proxy_addr: &str) -> Result<(), String> { Ok(()) }
    pub fn disable() -> Result<(), String> { Ok(()) }
    pub fn is_enabled() -> bool { false }
}
