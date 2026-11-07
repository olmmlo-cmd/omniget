//! Per-domain cookie files shared by the desktop Cookie Manager and the CLI.
//!
//! The on-disk layout matches the GUI:
//! `cookies/<registrable-domain>/_default.txt` plus optional named accounts
//! `cookies/<registrable-domain>/<slug>.txt`. A resolver always materializes
//! a session file that contains only fresh cookies for the requested site.
//! Cookie values are never included in errors or file names.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const MISSING_SESSION_MESSAGE: &str =
    "No managed session is stored for this site. Save it from the paired browser extension.";
pub const EXPIRED_SESSION_MESSAGE: &str =
    "The managed session for this site has expired. Save a fresh session from the paired browser extension.";

const META_FILE: &str = "_meta.json";
const DEFAULT_SLUG: &str = "_default";
const LEGACY_FILE: &str = "cookies.txt";

/// Second-level public suffixes. Kept in one place so the CLI and the Cookie
/// Manager file `app.rocketseat.com.br` under the same bucket.
const TWO_LABEL_PUBLIC_SUFFIXES: &[&str] = &[
    "com.br", "net.br", "org.br", "edu.br", "gov.br", "art.br", "eng.br", "co.uk", "org.uk",
    "ac.uk", "gov.uk", "me.uk", "com.au", "net.au", "org.au", "edu.au", "co.jp", "ne.jp", "or.jp",
    "ac.jp", "com.mx", "org.mx", "com.ar", "com.co", "com.pe", "com.cl", "com.ve", "com.uy",
    "co.za", "co.nz", "co.in", "co.kr", "co.id", "com.sg", "com.hk", "com.tw", "com.my", "com.tr",
    "com.pt", "com.es", "com.pl", "com.ua", "com.ru", "com.cn", "com.ph",
];

pub fn root_domain_of(host: &str) -> String {
    let h = host
        .trim()
        .trim_start_matches("#HttpOnly_")
        .trim_start_matches('.')
        .to_lowercase();
    let parts: Vec<&str> = h.split('.').filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 {
        return h;
    }
    let last_two = parts[parts.len() - 2..].join(".");
    if parts.len() >= 3 && TWO_LABEL_PUBLIC_SUFFIXES.contains(&last_two.as_str()) {
        return parts[parts.len() - 3..].join(".");
    }
    last_two
}

/// Account slug used by `import-cookies --name`. Empty input stays on `_default`.
pub fn account_slug(name: Option<&str>) -> String {
    let Some(name) = name.map(str::trim).filter(|s| !s.is_empty()) else {
        return DEFAULT_SLUG.to_string();
    };
    let cleaned: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        DEFAULT_SLUG.to_string()
    } else {
        cleaned.chars().take(40).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    /// Filtered Netscape file containing only fresh cookies for this site.
    Ready {
        path: PathBuf,
    },
    Missing,
    Expired,
}

impl SessionStatus {
    pub fn message(&self) -> &'static str {
        match self {
            SessionStatus::Ready { .. } => "",
            SessionStatus::Missing => MISSING_SESSION_MESSAGE,
            SessionStatus::Expired => EXPIRED_SESSION_MESSAGE,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            SessionStatus::Ready { path } => Some(path),
            SessionStatus::Missing | SessionStatus::Expired => None,
        }
    }
}

pub fn resolve_for_url(cookies_dir: &Path, url: &str, slug: Option<&str>) -> SessionStatus {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    resolve_for_url_at(cookies_dir, url, slug, now)
}

pub fn resolve_for_url_at(
    cookies_dir: &Path,
    url: &str,
    slug: Option<&str>,
    now: u64,
) -> SessionStatus {
    let Some(root) = url_root(url) else {
        return SessionStatus::Missing;
    };
    if domain_disabled(cookies_dir, &root) {
        return SessionStatus::Missing;
    }
    let slug = slug
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_SLUG);
    let slug = account_slug(Some(slug));

    let managed = account_file(cookies_dir, &root, &slug);
    if managed.exists() {
        return classify_file(cookies_dir, &root, &slug, &managed, now);
    }
    if slug != DEFAULT_SLUG {
        return SessionStatus::Missing;
    }
    let legacy = cookies_dir.join(LEGACY_FILE);
    if legacy.exists() {
        return classify_file(cookies_dir, &root, &slug, &legacy, now);
    }
    SessionStatus::Missing
}

/// Split a Netscape export into per-domain account files. Named imports use
/// `slug` and do not replace `_default`.
pub fn import_netscape_split(
    cookies_dir: &Path,
    content: &str,
    slug: &str,
) -> Result<Vec<(String, usize)>, String> {
    let slug = account_slug(Some(slug));
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in content.lines() {
        let Some(parsed) = parse_cookie_line(line) else {
            continue;
        };
        let root = root_domain_of(&parsed.domain);
        if root.is_empty() || root.contains('/') || root.contains('\\') {
            continue;
        }
        groups
            .entry(root)
            .or_default()
            .push(line.trim_end().to_string());
    }
    if groups.is_empty() {
        return Err("No valid cookies found in file (expected Netscape format)".to_string());
    }
    let registry_path = meta_path(cookies_dir);
    if registry_path.exists() && load_registry(cookies_dir).is_none() {
        return Err("Cookie registry is unreadable; the import was not recorded".to_string());
    }

    let mut written = Vec::new();
    for (root, lines) in &groups {
        let path = account_file(cookies_dir, root, &slug);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|_| "Could not create the cookie directory".to_string())?;
        }
        let mut body = String::from("# Netscape HTTP Cookie File\n");
        for line in lines {
            body.push_str(line);
            body.push('\n');
        }
        write_private(&path, &body)?;
        written.push((root.clone(), lines.len()));
    }
    upsert_registry(cookies_dir, &written, &slug)?;
    Ok(written)
}

fn url_root(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let root = root_domain_of(host);
    if root.is_empty() {
        None
    } else {
        Some(root)
    }
}

fn classify_file(
    cookies_dir: &Path,
    root: &str,
    slug: &str,
    source: &Path,
    now: u64,
) -> SessionStatus {
    let content = match fs::read_to_string(source) {
        Ok(content) => content,
        Err(_) => return SessionStatus::Missing,
    };
    let mut fresh = Vec::new();
    let mut saw_match = false;
    let mut saw_expired = false;
    for line in content.lines() {
        let Some(parsed) = parse_cookie_line(line) else {
            continue;
        };
        if !domain_belongs(&parsed.domain, root) {
            continue;
        }
        saw_match = true;
        if parsed.expires != 0 && parsed.expires <= now {
            saw_expired = true;
            continue;
        }
        fresh.push(line.trim_end().to_string());
    }
    let session = session_file(cookies_dir, root, slug);
    if fresh.is_empty() {
        let _ = fs::remove_file(&session);
        if saw_match && saw_expired {
            SessionStatus::Expired
        } else {
            SessionStatus::Missing
        }
    } else {
        if let Some(parent) = session.parent() {
            if fs::create_dir_all(parent).is_err() {
                return SessionStatus::Missing;
            }
        }
        let mut body = String::from("# Netscape HTTP Cookie File\n");
        for line in &fresh {
            body.push_str(line);
            body.push('\n');
        }
        if write_private(&session, &body).is_err() {
            return SessionStatus::Missing;
        }
        SessionStatus::Ready { path: session }
    }
}

struct ParsedCookie<'a> {
    domain: &'a str,
    expires: u64,
}

fn parse_cookie_line(line: &str) -> Option<ParsedCookie<'_>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("#HttpOnly_") {
        return None;
    }
    let normalized = trimmed.strip_prefix("#HttpOnly_").unwrap_or(trimmed);
    let mut parts = normalized.split('\t');
    let domain = parts.next()?;
    let _include_sub = parts.next()?;
    let _path = parts.next()?;
    let _secure = parts.next()?;
    let expires = parts.next()?.parse::<u64>().unwrap_or(0);
    let name = parts.next()?;
    let value = parts.next()?;
    if name.is_empty() || value.is_empty() || domain.is_empty() {
        return None;
    }
    Some(ParsedCookie { domain, expires })
}

fn domain_belongs(cookie_domain: &str, root: &str) -> bool {
    let cookie = root_domain_of(cookie_domain);
    !cookie.is_empty() && cookie == root
}

fn account_file(cookies_dir: &Path, root: &str, slug: &str) -> PathBuf {
    cookies_dir
        .join(safe_domain_segment(root))
        .join(format!("{slug}.txt"))
}

fn session_file(cookies_dir: &Path, root: &str, slug: &str) -> PathBuf {
    cookies_dir
        .join(safe_domain_segment(root))
        .join(format!(".{slug}.session.txt"))
}

fn safe_domain_segment(domain: &str) -> String {
    domain
        .trim_start_matches('.')
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn write_private(path: &Path, body: &str) -> Result<(), String> {
    fs::write(path, body).map_err(|_| "Could not write the cookie file".to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| "Could not restrict the cookie file".to_string())?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AccountEntry {
    slug: String,
    alias: String,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    source_label: Option<String>,
    captured_at_ms: i64,
    cookie_count: usize,
    #[serde(default)]
    last_used_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BucketEntry {
    platform_kind: String,
    accounts: Vec<AccountEntry>,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Registry {
    #[serde(default)]
    buckets: BTreeMap<String, BucketEntry>,
    #[serde(default)]
    disabled_domains: BTreeSet<String>,
}

fn meta_path(cookies_dir: &Path) -> PathBuf {
    cookies_dir.join(META_FILE)
}

fn load_registry(cookies_dir: &Path) -> Option<Registry> {
    let path = meta_path(cookies_dir);
    if !path.exists() {
        return Some(Registry::default());
    }
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn domain_disabled(cookies_dir: &Path, root: &str) -> bool {
    let Some(registry) = load_registry(cookies_dir) else {
        return false;
    };
    if registry.disabled_domains.contains(root) {
        return true;
    }
    registry
        .buckets
        .get(root)
        .is_some_and(|bucket| !bucket.enabled)
}

fn platform_kind_for(root: &str) -> String {
    match root {
        "youtube.com" | "youtu.be" => "youtube",
        "soundcloud.com" => "soundcloud",
        "spotify.com" => "spotify",
        "twitch.tv" => "twitch",
        "instagram.com" => "instagram",
        "x.com" | "twitter.com" => "x_twitter",
        "vimeo.com" => "vimeo",
        "tiktok.com" => "tiktok",
        "bilibili.com" => "bilibili",
        "douyin.com" | "iesdouyin.com" => "douyin",
        "reddit.com" => "reddit",
        "pinterest.com" => "pinterest",
        "bsky.app" | "bsky.social" => "bluesky",
        _ => "generic",
    }
    .to_string()
}

fn upsert_registry(
    cookies_dir: &Path,
    written: &[(String, usize)],
    slug: &str,
) -> Result<(), String> {
    let path = meta_path(cookies_dir);
    if path.exists() && load_registry(cookies_dir).is_none() {
        return Err("Cookie registry is unreadable; the import was not recorded".to_string());
    }
    let mut registry = load_registry(cookies_dir).unwrap_or_default();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    for (root, count) in written {
        let disabled = registry.disabled_domains.contains(root);
        let bucket = registry
            .buckets
            .entry(root.clone())
            .or_insert_with(|| BucketEntry {
                platform_kind: platform_kind_for(root),
                accounts: Vec::new(),
                enabled: !disabled,
            });
        bucket.platform_kind = platform_kind_for(root);
        if disabled {
            bucket.enabled = false;
        }
        if let Some(existing) = bucket.accounts.iter_mut().find(|a| a.slug == slug) {
            existing.cookie_count = *count;
            existing.captured_at_ms = now;
            existing.source_label = Some("CLI import-cookies".to_string());
        } else {
            bucket.accounts.push(AccountEntry {
                slug: slug.to_string(),
                alias: if slug == DEFAULT_SLUG {
                    format!("{} import", platform_kind_for(root))
                } else {
                    slug.to_string()
                },
                source_url: None,
                source_label: Some("CLI import-cookies".to_string()),
                captured_at_ms: now,
                cookie_count: *count,
                last_used_at_ms: None,
            });
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|_| "Could not create the cookie directory".to_string())?;
    }
    let serialized = serde_json::to_string_pretty(&registry)
        .map_err(|_| "Could not record the cookie import".to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serialized).map_err(|_| "Could not record the cookie import".to_string())?;
    fs::rename(&tmp, &path).map_err(|_| "Could not record the cookie import".to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;
    const DOUYIN_SECRET: &str = "dy-synthetic-session";
    const BILI_SECRET: &str = "bi-synthetic-session";

    fn netscape(domain: &str, name: &str, value: &str, expires: u64) -> String {
        format!(
            "# Netscape HTTP Cookie File\n{domain}\tTRUE\t/\tTRUE\t{expires}\t{name}\t{value}\n"
        )
    }

    fn write_account(dir: &Path, domain: &str, slug: &str, body: &str) {
        let path = account_file(dir, domain, slug);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "omniget-cookies-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn root_domain_matches_cookie_manager_buckets() {
        assert_eq!(root_domain_of("www.douyin.com"), "douyin.com");
        assert_eq!(root_domain_of(".douyin.com"), "douyin.com");
        assert_eq!(root_domain_of("www.bilibili.com"), "bilibili.com");
        assert_eq!(root_domain_of("app.rocketseat.com.br"), "rocketseat.com.br");
        assert_eq!(root_domain_of("www.bbc.co.uk"), "bbc.co.uk");
    }

    #[test]
    fn douyin_session_is_not_sent_to_bilibili() {
        let dir = temp_dir();
        write_account(
            &dir,
            "douyin.com",
            "_default",
            &netscape(".douyin.com", "ttwid", DOUYIN_SECRET, NOW + 3600),
        );
        write_account(
            &dir,
            "bilibili.com",
            "_default",
            &netscape(".bilibili.com", "SESSDATA", BILI_SECRET, NOW + 3600),
        );

        let douyin = resolve_for_url_at(&dir, "https://www.douyin.com/video/123", None, NOW);
        let bili = resolve_for_url_at(&dir, "https://www.bilibili.com/video/BV1", None, NOW);
        let douyin_body = fs::read_to_string(douyin.path().unwrap()).unwrap();
        let bili_body = fs::read_to_string(bili.path().unwrap()).unwrap();
        assert!(douyin_body.contains(DOUYIN_SECRET));
        assert!(!douyin_body.contains(BILI_SECRET));
        assert!(bili_body.contains(BILI_SECRET));
        assert!(!bili_body.contains(DOUYIN_SECRET));
        assert!(!douyin
            .path()
            .unwrap()
            .to_string_lossy()
            .contains(DOUYIN_SECRET));
        assert!(!douyin.message().contains(DOUYIN_SECRET));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mixed_legacy_file_is_filtered_to_the_requested_site() {
        let dir = temp_dir();
        let mut mixed = netscape(".douyin.com", "ttwid", DOUYIN_SECRET, NOW + 3600);
        mixed.push_str(&format!(
            ".bilibili.com\tTRUE\t/\tTRUE\t{}\tSESSDATA\t{}\n",
            NOW + 3600,
            BILI_SECRET
        ));
        fs::write(dir.join(LEGACY_FILE), mixed).unwrap();

        let status = resolve_for_url_at(&dir, "https://www.douyin.com/video/1", None, NOW);
        let body = fs::read_to_string(status.path().unwrap()).unwrap();
        assert!(body.contains(DOUYIN_SECRET));
        assert!(!body.contains(BILI_SECRET));
        assert!(matches!(status, SessionStatus::Ready { .. }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn managed_account_wins_over_the_legacy_file() {
        let dir = temp_dir();
        write_account(
            &dir,
            "douyin.com",
            "_default",
            &netscape(".douyin.com", "ttwid", "managed-dy", NOW + 3600),
        );
        fs::write(
            dir.join(LEGACY_FILE),
            netscape(".douyin.com", "ttwid", "legacy-dy", NOW + 3600),
        )
        .unwrap();
        let status = resolve_for_url_at(&dir, "https://v.douyin.com/abc/", None, NOW);
        let body = fs::read_to_string(status.path().unwrap()).unwrap();
        assert!(body.contains("managed-dy"));
        assert!(!body.contains("legacy-dy"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_and_expired_sessions_are_distinct_and_do_not_leak() {
        let dir = temp_dir();
        let missing = resolve_for_url_at(&dir, "https://www.douyin.com/video/1", None, NOW);
        assert_eq!(missing, SessionStatus::Missing);
        assert_eq!(missing.message(), MISSING_SESSION_MESSAGE);
        assert!(!missing.message().contains(DOUYIN_SECRET));

        write_account(
            &dir,
            "douyin.com",
            "_default",
            &netscape(".douyin.com", "ttwid", DOUYIN_SECRET, NOW - 10),
        );
        let expired = resolve_for_url_at(&dir, "https://www.douyin.com/video/1", None, NOW);
        assert_eq!(expired, SessionStatus::Expired);
        assert_eq!(expired.message(), EXPIRED_SESSION_MESSAGE);
        assert!(!expired.message().contains(DOUYIN_SECRET));
        assert!(!session_file(&dir, "douyin.com", "_default").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_cookie_is_kept_when_an_older_one_expired() {
        let dir = temp_dir();
        let mut body = netscape(".douyin.com", "ttwid", DOUYIN_SECRET, NOW + 60);
        body.push_str(&format!(
            ".douyin.com\tTRUE\t/\tTRUE\t{}\tstale\t{}\n",
            NOW - 5,
            "stale-secret"
        ));
        write_account(&dir, "douyin.com", "_default", &body);
        let status = resolve_for_url_at(&dir, "https://www.douyin.com/video/1", None, NOW);
        let out = fs::read_to_string(status.path().unwrap()).unwrap();
        assert!(out.contains(DOUYIN_SECRET));
        assert!(!out.contains("stale-secret"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_domain_is_not_resolved() {
        let dir = temp_dir();
        write_account(
            &dir,
            "douyin.com",
            "_default",
            &netscape(".douyin.com", "ttwid", DOUYIN_SECRET, NOW + 60),
        );
        let registry = Registry {
            buckets: BTreeMap::from([(
                "douyin.com".to_string(),
                BucketEntry {
                    platform_kind: "douyin".to_string(),
                    accounts: Vec::new(),
                    enabled: false,
                },
            )]),
            disabled_domains: BTreeSet::from(["douyin.com".to_string()]),
        };
        fs::write(meta_path(&dir), serde_json::to_string(&registry).unwrap()).unwrap();
        let status = resolve_for_url_at(&dir, "https://www.douyin.com/video/1", None, NOW);
        assert_eq!(status, SessionStatus::Missing);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn named_import_does_not_replace_or_leak_the_default_account() {
        let dir = temp_dir();
        let default_body = netscape(".douyin.com", "ttwid", "default-dy", NOW + 60);
        let named_body = format!(
            "{}\n.bilibili.com\tTRUE\t/\tTRUE\t{}\tSESSDATA\t{}\n",
            netscape(".douyin.com", "ttwid", "named-dy", NOW + 60),
            NOW + 60,
            BILI_SECRET
        );
        import_netscape_split(&dir, &default_body, "_default").unwrap();
        import_netscape_split(&dir, &named_body, "firefox").unwrap();

        let default_status = resolve_for_url_at(&dir, "https://www.douyin.com/video/1", None, NOW);
        let default_out = fs::read_to_string(default_status.path().unwrap()).unwrap();
        assert!(default_out.contains("default-dy"));
        assert!(!default_out.contains("named-dy"));
        assert!(!default_out.contains(BILI_SECRET));

        let named =
            resolve_for_url_at(&dir, "https://www.douyin.com/video/1", Some("firefox"), NOW);
        let named_out = fs::read_to_string(named.path().unwrap()).unwrap();
        assert!(named_out.contains("named-dy"));
        assert!(!named_out.contains(BILI_SECRET));
        assert!(!named_out.contains("default-dy"));

        let named_bili = resolve_for_url_at(
            &dir,
            "https://www.bilibili.com/video/BV1",
            Some("firefox"),
            NOW,
        );
        let named_bili_out = fs::read_to_string(named_bili.path().unwrap()).unwrap();
        assert!(named_bili_out.contains(BILI_SECRET));
        assert!(!named_bili_out.contains("named-dy"));

        let raw = fs::read_to_string(meta_path(&dir)).unwrap();
        assert!(raw.contains("douyin"));
        assert!(!raw.contains("named-dy"));
        assert!(!raw.contains(BILI_SECRET));
        assert!(!raw.contains(DOUYIN_SECRET));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_zero_expiry_is_kept() {
        let dir = temp_dir();
        write_account(
            &dir,
            "douyin.com",
            "_default",
            &netscape(".douyin.com", "ttwid", DOUYIN_SECRET, 0),
        );
        let status = resolve_for_url_at(&dir, "https://www.douyin.com/video/1", None, NOW);
        assert!(matches!(status, SessionStatus::Ready { .. }));
        let _ = fs::remove_dir_all(&dir);
    }
}
