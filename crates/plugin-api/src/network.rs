//! [`NetworkRule`]: an address an app may send HTTP requests to.

/// One entry of `permissions.network` in a manifest:
///
/// - `api.example.com` — that host over HTTPS (port 443);
/// - `*.example.com` — its subdomains, not `example.com` itself;
/// - `api.example.com:8443` — another port;
/// - `http://192.168.1.10:11434` — plain HTTP, e.g. a model server in the LAN.
///
/// Paths are not part of a rule: an app reaches the whole host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkRule {
    https: bool,
    /// Lower case, without `*.`.
    host: String,
    subdomains: bool,
    port: u16,
    /// Read from a setting the administrator filled in (`"{key}"`).
    from_setting: bool,
}

impl NetworkRule {
    /// Reads a rule, or says what is wrong with it.
    pub fn parse(rule: &str) -> Result<Self, String> {
        let (https, rest) = match rule.split_once("://") {
            Some(("https", rest)) => (true, rest),
            Some(("http", rest)) => (false, rest),
            Some(_) => return Err("only http:// and https:// are allowed".to_owned()),
            None => (true, rule),
        };
        if rest.contains(['/', '?', '#', '@', '[']) {
            return Err("only a host and a port, without a path".to_owned());
        }
        let (host, port) = match rest.rsplit_once(':') {
            Some((host, port)) => match port.parse::<u16>() {
                Ok(port) if port > 0 => (host, port),
                _ => return Err(format!("\"{port}\" is not a port")),
            },
            None => (rest, if https { 443 } else { 80 }),
        };
        let (subdomains, host) = match host.strip_prefix("*.") {
            Some(host) => (true, host),
            None => (false, host),
        };
        let host = host.to_ascii_lowercase();
        let valid_label = |label: &str| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        };
        if host.len() > 253 || !host.split('.').all(valid_label) {
            return Err(format!("\"{host}\" is not a host name"));
        }
        Ok(Self { https, host, subdomains, port, from_setting: false })
    }

    /// The rule for the server of a URL a setting holds:
    /// `https://office.example.com/` → `office.example.com:443`.
    pub fn from_url(url: &str) -> Result<Self, String> {
        let (scheme, rest) = url.split_once("://").ok_or("needs http:// or https://")?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let authority = authority.rsplit('@').next().unwrap_or_default();
        if authority.starts_with("*.") {
            return Err("a setting names one server".to_owned());
        }
        let rule = Self::parse(&format!("{scheme}://{authority}"))?;
        Ok(Self { from_setting: true, ..rule })
    }

    /// Whether the host may be in a private network (the LAN, this machine):
    /// only when an IP address is named, or the administrator gave the
    /// address in a setting. A name chosen by the plugin's author must lead
    /// into the internet: its DNS could point anywhere.
    pub fn reaches_private(&self) -> bool {
        self.from_setting || self.host.parse::<std::net::IpAddr>().is_ok()
    }

    /// The origin it allows, for a browser: `https://office.example.com`,
    /// with the port only if it isn't the scheme's own.
    pub fn origin(&self) -> String {
        let (scheme, default_port) = if self.https { ("https", 443) } else { ("http", 80) };
        let host = if self.subdomains { format!("*.{}", self.host) } else { self.host.clone() };
        match self.port == default_port {
            true => format!("{scheme}://{host}"),
            false => format!("{scheme}://{host}:{}", self.port),
        }
    }

    /// Whether a request to `scheme://host:port` is allowed.
    pub fn allows(&self, scheme: &str, host: &str, port: u16) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let host_matches = if self.subdomains {
            host.strip_suffix(&self.host).is_some_and(|sub| sub.len() > 1 && sub.ends_with('.'))
        } else {
            host == self.host
        };
        scheme == if self.https { "https" } else { "http" } && port == self.port && host_matches
    }
}

/// Whether `rules` allow a request to `url` (`scheme://host[:port]/…`).
pub fn url_allowed(rules: &[NetworkRule], url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains(['@', '[']) {
        return false;
    }
    let default_port = match scheme {
        "https" => 443,
        "http" => 80,
        _ => return false,
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => match port.parse::<u16>() {
            Ok(port) => (host, port),
            Err(_) => return false,
        },
        None => (authority, default_port),
    };
    rules.iter().any(|rule| rule.allows(scheme, host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_checked_against_the_rules() {
        let rules = [NetworkRule::parse("*.huggingface.co").unwrap(), NetworkRule::parse("http://lan:8080").unwrap()];
        assert!(url_allowed(&rules, "https://cdn.huggingface.co/models/face.onnx?download=1"));
        assert!(url_allowed(&rules, "http://lan:8080/model.bin"));
        assert!(!url_allowed(&rules, "https://huggingface.co/x"));
        assert!(!url_allowed(&rules, "https://user@cdn.huggingface.co/x"));
        assert!(!url_allowed(&rules, "ftp://cdn.huggingface.co/x"));
        assert!(!url_allowed(&rules, "cdn.huggingface.co/x"));
    }

    fn allows(rule: &str, scheme: &str, host: &str, port: u16) -> bool {
        NetworkRule::parse(rule).expect("valid rule").allows(scheme, host, port)
    }

    #[test]
    fn only_ip_addresses_and_settings_may_lead_into_private_networks() {
        assert!(!NetworkRule::parse("api.example.com").unwrap().reaches_private());
        assert!(NetworkRule::parse("http://192.168.1.10:11434").unwrap().reaches_private());
        assert!(NetworkRule::from_url("http://collabora:9980/").unwrap().reaches_private());
    }

    #[test]
    fn a_host_is_reached_over_https_by_default() {
        assert!(allows("api.example.com", "https", "api.example.com", 443));
        assert!(allows("API.example.com", "https", "api.EXAMPLE.com.", 443));
        assert!(!allows("api.example.com", "http", "api.example.com", 80), "plain http only when named");
        assert!(!allows("api.example.com", "https", "api.example.com", 8443));
        assert!(!allows("api.example.com", "https", "example.com", 443));
        assert!(!allows("api.example.com", "https", "api.example.com.evil.org", 443));
    }

    #[test]
    fn ports_plain_http_and_subdomains_are_named_explicitly() {
        assert!(allows("api.example.com:8443", "https", "api.example.com", 8443));
        assert!(allows("http://192.168.1.10:11434", "http", "192.168.1.10", 11434));
        assert!(!allows("http://192.168.1.10:11434", "https", "192.168.1.10", 11434));
        assert!(allows("*.example.com", "https", "cdn.eu.example.com", 443));
        assert!(!allows("*.example.com", "https", "example.com", 443));
        assert!(!allows("*.example.com", "https", "badexample.com", 443));
    }

    #[test]
    fn bad_rules_are_refused() {
        for rule in [
            "",
            "*",
            "*.",
            "https://",
            "ftp://example.com",
            "example.com/api",
            "example.com:0",
            "a..b",
            "-a.com",
            "[::1]:80",
            "user@example.com",
        ] {
            assert!(NetworkRule::parse(rule).is_err(), "{rule:?} must be refused");
        }
    }
}
