use worker::{Env, Error, Result};

const DEFAULT_SERVER: &str = "koukoku.shadan.open.ad.jp";
const DEFAULT_PORT: u16 = 992;
const MAX_HOSTNAME_LEN: usize = 253;

pub fn default_target() -> (String, u16) {
    (DEFAULT_SERVER.to_string(), DEFAULT_PORT)
}

pub fn allowed_targets(env: &Env) -> Vec<(String, u16)> {
    let Ok(value) = env.var("ALLOWED_TARGETS") else {
        return vec![default_target()];
    };

    let targets = value
        .to_string()
        .split(',')
        .filter_map(parse_target_entry)
        .collect::<Vec<_>>();

    if targets.is_empty() {
        vec![default_target()]
    } else {
        targets
    }
}

pub fn resolve_target(env: &Env, server: &str, port: u16) -> Result<(String, u16)> {
    let normalized = normalize_hostname(server)?;
    validate_target(&normalized, port, &allowed_targets(env))
}

fn parse_target_entry(entry: &str) -> Option<(String, u16)> {
    let entry = entry.trim();
    if entry.is_empty() {
        return None;
    }

    let (host, port) = entry.rsplit_once(':')?;
    let host = normalize_hostname(host).ok()?;
    let port = port.parse().ok()?;
    Some((host, port))
}

fn normalize_hostname(server: &str) -> Result<String> {
    let server = server.trim().trim_end_matches('.');
    if server.is_empty() || server.len() > MAX_HOSTNAME_LEN {
        return Err(Error::RustError("invalid server hostname".into()));
    }

    if server.parse::<std::net::IpAddr>().is_ok() {
        return Err(Error::RustError("direct IP connections are not allowed".into()));
    }

    if server.contains("..") || server.starts_with('-') || server.ends_with('-') {
        return Err(Error::RustError("invalid server hostname".into()));
    }

    let valid = server
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-');
    if !valid {
        return Err(Error::RustError("invalid server hostname".into()));
    }

    let labels = server.split('.');
    for label in labels.clone() {
        if label.is_empty() || label.len() > 63 {
            return Err(Error::RustError("invalid server hostname".into()));
        }
    }

    if labels.count() < 2 {
        return Err(Error::RustError("server hostname must be a FQDN".into()));
    }

    Ok(server.to_ascii_lowercase())
}

fn validate_target(server: &str, port: u16, allowed: &[(String, u16)]) -> Result<(String, u16)> {
    if port == 0 {
        return Err(Error::RustError("invalid port".into()));
    }

    if is_blocked_port(port) {
        return Err(Error::RustError("port is not allowed".into()));
    }

    if allowed.iter().any(|(host, allowed_port)| host == server && *allowed_port == port) {
        Ok((server.to_string(), port))
    } else {
        Err(Error::RustError("target is not in the allowlist".into()))
    }
}

fn is_blocked_port(port: u16) -> bool {
    matches!(port, 22 | 23 | 25 | 53 | 80 | 110 | 143 | 443 | 445 | 3306 | 5432 | 6379 | 11211)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ip_literals() {
        assert!(normalize_hostname("127.0.0.1").is_err());
        assert!(normalize_hostname("10.0.0.5").is_err());
    }

    #[test]
    fn accepts_fqdn() {
        assert_eq!(
            normalize_hostname("koukoku.shadan.open.ad.jp").unwrap(),
            "koukoku.shadan.open.ad.jp"
        );
    }

    #[test]
    fn blocks_sensitive_ports() {
        assert!(is_blocked_port(25));
        assert!(is_blocked_port(443));
        assert!(!is_blocked_port(992));
    }
}