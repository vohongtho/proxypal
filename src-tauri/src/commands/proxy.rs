use env_proxy;
use sysproxy::Sysproxy;
use url::Url;

const DEFAULT_PROXY_CHECK_URL: &str = "https://example.com";

fn env_proxy_for_url(target_url: &str) -> Option<String> {
    let parsed = Url::parse(target_url).ok()?;
    let proxy = env_proxy::for_url(&parsed);
    let (host, port) = proxy.host_port()?;
    Some(format!("http://{}:{}", host, port))
}

fn normalize_system_proxy(host: &str, port: u16) -> String {
    let protocol = if host.to_ascii_lowercase().contains("socks") {
        "socks5"
    } else {
        "http"
    };
    format!("{}://{}:{}", protocol, host, port)
}

#[tauri::command]
pub fn get_system_proxy() -> Result<Option<String>, String> {
    // 1. Check environment variables first (common in Linux/Dev environments)
    // We use a neutral URL to avoid region-specific assumptions.
    if let Some(proxy) = env_proxy_for_url(DEFAULT_PROXY_CHECK_URL) {
        return Ok(Some(proxy));
    }

    // 2. Check OS-level system proxy settings
    let sys_proxy = Sysproxy::get_system_proxy();

    match sys_proxy {
        Ok(proxy) if proxy.enable => Ok(Some(normalize_system_proxy(&proxy.host, proxy.port))),
        Ok(_) => Ok(None),
        Err(e) => Err(format!("Failed to detect system proxy: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_var_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[test]
    fn env_proxy_for_url_returns_none_for_invalid_target() {
        assert!(env_proxy_for_url("not-a-url").is_none());
    }

    #[test]
    fn env_proxy_for_url_returns_none_when_env_is_missing() {
        let _guard = env_var_lock().lock().unwrap();
        let old_http_upper = std::env::var_os("HTTP_PROXY");
        let old_https_upper = std::env::var_os("HTTPS_PROXY");
        let old_http_lower = std::env::var_os("http_proxy");
        let old_https_lower = std::env::var_os("https_proxy");

        std::env::remove_var("HTTP_PROXY");
        std::env::remove_var("HTTPS_PROXY");
        std::env::remove_var("http_proxy");
        std::env::remove_var("https_proxy");

        let detected = env_proxy_for_url(DEFAULT_PROXY_CHECK_URL);

        if let Some(value) = old_http_upper {
            std::env::set_var("HTTP_PROXY", value);
        }
        if let Some(value) = old_https_upper {
            std::env::set_var("HTTPS_PROXY", value);
        }
        if let Some(value) = old_http_lower {
            std::env::set_var("http_proxy", value);
        }
        if let Some(value) = old_https_lower {
            std::env::set_var("https_proxy", value);
        }

        assert!(detected.is_none());
    }

    #[test]
    fn normalize_system_proxy_uses_http_for_regular_hosts() {
        assert_eq!(
            normalize_system_proxy("127.0.0.1", 8080),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn normalize_system_proxy_uses_socks5_for_socks_hosts() {
        assert_eq!(
            normalize_system_proxy("socks-proxy.local", 1080),
            "socks5://socks-proxy.local:1080"
        );
    }
}
