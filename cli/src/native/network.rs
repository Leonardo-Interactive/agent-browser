use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::RwLock;

use super::cdp::client::CdpClient;

pub async fn set_extra_headers(
    client: &CdpClient,
    session_id: &str,
    headers: &HashMap<String, String>,
) -> Result<(), String> {
    let headers_value: Value = headers
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect::<serde_json::Map<String, Value>>()
        .into();

    client
        .send_command(
            "Network.setExtraHTTPHeaders",
            Some(json!({ "headers": headers_value })),
            Some(session_id),
        )
        .await?;

    Ok(())
}

pub async fn set_offline(
    client: &CdpClient,
    session_id: &str,
    offline: bool,
) -> Result<(), String> {
    client
        .send_command(
            "Network.emulateNetworkConditions",
            Some(json!({
                "offline": offline,
                "latency": 0,
                "downloadThroughput": -1,
                "uploadThroughput": -1,
            })),
            Some(session_id),
        )
        .await?;
    Ok(())
}

pub async fn set_content(client: &CdpClient, session_id: &str, html: &str) -> Result<(), String> {
    // Get current frame ID
    let tree_result = client
        .send_command_no_params("Page.getFrameTree", Some(session_id))
        .await?;

    let frame_id = tree_result
        .get("frameTree")
        .and_then(|t| t.get("frame"))
        .and_then(|f| f.get("id"))
        .and_then(|id| id.as_str())
        .ok_or("Could not determine frame ID")?;

    client
        .send_command(
            "Page.setDocumentContent",
            Some(json!({
                "frameId": frame_id,
                "html": html,
            })),
            Some(session_id),
        )
        .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Domain filter
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DomainFilter {
    /// Domains the agent is allowed to navigate to (open, click, form submit).
    /// When empty, all navigation is allowed.
    pub navigation_domains: Vec<String>,
    /// Domains the page is allowed to load sub-resources from (fetch, XHR,
    /// images, scripts, WebSocket, etc.). When empty, all resources are allowed.
    pub resource_domains: Vec<String>,
    /// Legacy unified list. Kept for backwards compatibility: if
    /// `navigation_domains` or `resource_domains` is empty, the corresponding
    /// check falls back to this list.
    pub allowed_domains: Vec<String>,
}

impl DomainFilter {
    pub fn new(domains: &str) -> Self {
        let allowed = parse_domain_list(domains);
        Self {
            navigation_domains: Vec::new(),
            resource_domains: Vec::new(),
            allowed_domains: allowed,
        }
    }

    pub fn with_split(
        allowed: &str,
        navigation: Option<&str>,
        resource: Option<&str>,
    ) -> Self {
        Self {
            allowed_domains: parse_domain_list(allowed),
            navigation_domains: navigation
                .map(|s| parse_domain_list(s))
                .unwrap_or_default(),
            resource_domains: resource
                .map(|s| parse_domain_list(s))
                .unwrap_or_default(),
        }
    }

    /// Check whether any filtering is active at all.
    pub fn is_active(&self) -> bool {
        !self.allowed_domains.is_empty()
            || !self.navigation_domains.is_empty()
            || !self.resource_domains.is_empty()
    }

    /// Returns the effective domain list for navigation checks.
    fn effective_navigation_domains(&self) -> &[String] {
        if !self.navigation_domains.is_empty() {
            &self.navigation_domains
        } else {
            &self.allowed_domains
        }
    }

    /// Returns the effective domain list for resource checks.
    fn effective_resource_domains(&self) -> &[String] {
        if !self.resource_domains.is_empty() {
            &self.resource_domains
        } else {
            &self.allowed_domains
        }
    }

    pub(crate) fn matches_domain_list(domains: &[String], hostname: &str) -> bool {
        if domains.is_empty() {
            return true;
        }
        let hostname = hostname.to_lowercase();
        for pattern in domains {
            if glob_match_domain(pattern, &hostname) {
                return true;
            }
        }
        false
    }

    /// Check if a hostname is allowed for agent-initiated navigation.
    pub fn is_navigation_allowed(&self, hostname: &str) -> bool {
        Self::matches_domain_list(self.effective_navigation_domains(), hostname)
    }

    /// Check if a hostname is allowed for page-initiated sub-resource requests.
    pub fn is_resource_allowed(&self, hostname: &str) -> bool {
        Self::matches_domain_list(self.effective_resource_domains(), hostname)
    }

    /// Legacy: check if a hostname is allowed (uses `allowed_domains`).
    pub fn is_allowed(&self, hostname: &str) -> bool {
        Self::matches_domain_list(&self.allowed_domains, hostname)
    }

    /// Check a URL against the navigation domain filter.
    pub fn check_navigation_url(&self, url: &str) -> Result<(), String> {
        let domains = self.effective_navigation_domains();
        if domains.is_empty() {
            return Ok(());
        }
        let parsed = url::Url::parse(url).map_err(|_| format!("Invalid URL: {}", url))?;
        let hostname = parsed
            .host_str()
            .ok_or_else(|| format!("No hostname in URL: {}", url))?;
        if Self::matches_domain_list(domains, hostname) {
            Ok(())
        } else {
            Err(format!(
                "Domain '{}' is not in the allowed navigation domains list",
                hostname
            ))
        }
    }

    /// Legacy check_url for backwards compatibility (uses allowed_domains).
    pub fn check_url(&self, url: &str) -> Result<(), String> {
        if self.allowed_domains.is_empty() {
            return Ok(());
        }
        let parsed = url::Url::parse(url).map_err(|_| format!("Invalid URL: {}", url))?;
        let hostname = parsed
            .host_str()
            .ok_or_else(|| format!("No hostname in URL: {}", url))?;
        if self.is_allowed(hostname) {
            Ok(())
        } else {
            Err(format!(
                "Domain '{}' is not in the allowed domains list",
                hostname
            ))
        }
    }
}

/// Match a hostname against a single domain pattern.
/// Supports `*` as a wildcard anywhere in the pattern:
///  - `*.example.com` matches `example.com` and `sub.example.com`
///  - `prefix-*.example.com` matches `prefix-abc.example.com`
///  - `example.com` matches only `example.com`
fn glob_match_domain(pattern: &str, hostname: &str) -> bool {
    // Fast path: leading wildcard (most common case, preserves existing semantics)
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return hostname == suffix || hostname.ends_with(&format!(".{}", suffix));
    }

    // No wildcard → exact match
    if !pattern.contains('*') {
        return hostname == pattern;
    }

    // General glob: split on `*` and verify fragments appear in order
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut remaining = hostname.as_bytes();

    for (i, part) in parts.iter().enumerate() {
        let fragment = part.as_bytes();
        if fragment.is_empty() {
            continue;
        }
        if i == 0 {
            // First fragment must be a prefix
            if !remaining.starts_with(fragment) {
                return false;
            }
            remaining = &remaining[fragment.len()..];
        } else if i == parts.len() - 1 {
            // Last fragment must be a suffix
            if !remaining.ends_with(fragment) {
                return false;
            }
            remaining = &remaining[..remaining.len() - fragment.len()];
        } else {
            // Middle fragments: find next occurrence
            if let Some(pos) = remaining
                .windows(fragment.len())
                .position(|w| w == fragment)
            {
                remaining = &remaining[pos + fragment.len()..];
            } else {
                return false;
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Navigation domain ceiling (compiled into binary)
// ---------------------------------------------------------------------------

/// Maximum allowed navigation domains. Config-file entries that don't match
/// any ceiling pattern are rejected at load time.
pub(crate) const NAVIGATION_DOMAIN_CEILING: &[&str] = &[
    "leonardo.ai",
    "*.leonardo.ai",
    "leonardo-platform-*.vercel.app",
    "localhost",
];

/// Filter navigation domains against the compiled ceiling.
/// Keeps entries that match at least one ceiling pattern; emits a warning to
/// stderr for each rejected entry.
pub(crate) fn filter_by_ceiling(domains: Vec<String>) -> Vec<String> {
    let ceiling: Vec<String> = NAVIGATION_DOMAIN_CEILING
        .iter()
        .map(|s| s.to_lowercase())
        .collect();

    domains
        .into_iter()
        .filter(|domain| {
            // For config entries that contain wildcards (e.g. `*.leonardo.ai`),
            // generate a representative hostname by replacing `*` with a test
            // label, then check that representative against the ceiling.
            let representative = domain.replace('*', "__ceil_test__");
            let matched = DomainFilter::matches_domain_list(&ceiling, &representative);
            if !matched {
                eprintln!(
                    "[agent-browser] domain \"{}\" not in approved ceiling, ignored",
                    domain
                );
            }
            matched
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Localhost application handshake (compiled into binary)
// ---------------------------------------------------------------------------

// TODO: Extend the handshake to Vercel preview environments as well
// (leonardo-platform-*.vercel.app). This would prevent arbitrary Vercel apps
// from exploiting the mid-segment wildcard in the ceiling to bypass domain
// restrictions. Not implemented yet because the leonardo-platform project has
// not added the handshake endpoint to preview deployments — enabling it now
// would block navigation to Vercel previews during the proposal demo.

pub(crate) const LOCALHOST_HANDSHAKE_PATH: &str = "/api/__agent-browser-handshake";
pub(crate) const LOCALHOST_HANDSHAKE_EXPECT: &str = "leonardo-platform";

/// Returns `true` if the hostname is a localhost-family address.
pub(crate) fn is_localhost(hostname: &str) -> bool {
    matches!(
        hostname,
        "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]" | "::1"
    )
}

/// Perform a handshake request to a localhost service.
async fn check_localhost_handshake(host: &str, port: u16) -> Result<(), String> {
    let url = format!("http://{}:{}{}", host, port, LOCALHOST_HANDSHAKE_PATH);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| format!("[agent-browser] HTTP client error: {}", e))?;

    let resp = client.get(&url).send().await.map_err(|_| {
        format!(
            "[agent-browser] Could not reach {}:{} — is the dev server running?",
            host, port
        )
    })?;

    let json: serde_json::Value = resp.json().await.map_err(|_| {
        format!(
            "[agent-browser] localhost:{} did not pass application handshake, navigation blocked",
            port
        )
    })?;

    let app = json.get("app").and_then(|v| v.as_str()).unwrap_or("");
    if app == LOCALHOST_HANDSHAKE_EXPECT {
        Ok(())
    } else {
        Err(format!(
            "[agent-browser] localhost:{} did not pass application handshake, navigation blocked",
            port
        ))
    }
}

/// Check the localhost handshake with per-session caching.
/// After the first check for a given host:port, the cached result is returned.
pub(crate) async fn check_cached_handshake(
    cache: &RwLock<HashMap<String, bool>>,
    host: &str,
    port: u16,
) -> Result<(), String> {
    let key = format!("{}:{}", host, port);

    // Read lock — fast path for cached results
    {
        let c = cache.read().await;
        if let Some(&passed) = c.get(&key) {
            return if passed {
                Ok(())
            } else {
                Err(format!(
                    "[agent-browser] localhost:{} did not pass application handshake, navigation blocked",
                    port
                ))
            };
        }
    }

    // Cache miss — perform the handshake
    let result = check_localhost_handshake(host, port).await;
    let passed = result.is_ok();

    {
        let mut c = cache.write().await;
        c.insert(key, passed);
    }

    result
}

fn parse_domain_list(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

pub async fn sanitize_existing_pages(
    client: &CdpClient,
    pages: &[super::browser::PageInfo],
    filter: &DomainFilter,
) {
    for page in pages {
        if page.url.is_empty() || page.url == "about:blank" {
            continue;
        }
        if let Ok(parsed) = url::Url::parse(&page.url) {
            if let Some(hostname) = parsed.host_str() {
                if !filter.is_navigation_allowed(hostname) {
                    let _ = client
                        .send_command(
                            "Page.navigate",
                            Some(json!({ "url": "about:blank" })),
                            Some(&page.session_id),
                        )
                        .await;
                }
            }
        }
    }
}

pub async fn install_domain_filter_script(
    client: &CdpClient,
    session_id: &str,
    filter: &DomainFilter,
) -> Result<(), String> {
    let resource_domains = filter.effective_resource_domains();
    if resource_domains.is_empty() {
        return Ok(());
    }

    let domains_json = serde_json::to_string(resource_domains).unwrap_or("[]".to_string());
    let script = format!(
        r#"(() => {{
            const _allowed = {};
            function _isDomainAllowed(hostname) {{
                hostname = hostname.toLowerCase();
                for (const p of _allowed) {{
                    if (p.startsWith('*.')) {{
                        const suffix = p.slice(2);
                        if (hostname === suffix || hostname.endsWith('.' + suffix)) return true;
                    }} else if (hostname === p) return true;
                }}
                return false;
            }}
            const OrigWS = window.WebSocket;
            window.WebSocket = function(url, protocols) {{
                try {{
                    const u = new URL(url, location.href);
                    if (!_isDomainAllowed(u.hostname)) throw new DOMException('WebSocket blocked: ' + u.hostname, 'SecurityError');
                }} catch(e) {{ if (e instanceof DOMException) throw e; }}
                return new OrigWS(url, protocols);
            }};
            window.WebSocket.prototype = OrigWS.prototype;
            const OrigES = window.EventSource;
            if (OrigES) {{
                window.EventSource = function(url, opts) {{
                    try {{
                        const u = new URL(url, location.href);
                        if (!_isDomainAllowed(u.hostname)) throw new DOMException('EventSource blocked: ' + u.hostname, 'SecurityError');
                    }} catch(e) {{ if (e instanceof DOMException) throw e; }}
                    return new OrigES(url, opts);
                }};
                window.EventSource.prototype = OrigES.prototype;
            }}
            const origBeacon = navigator.sendBeacon;
            if (origBeacon) {{
                navigator.sendBeacon = function(url, data) {{
                    try {{
                        const u = new URL(url, location.href);
                        if (!_isDomainAllowed(u.hostname)) return false;
                    }} catch(e) {{ return false; }}
                    return origBeacon.call(navigator, url, data);
                }};
            }}
        }})()"#,
        domains_json,
    );

    client
        .send_command(
            "Page.addScriptToEvaluateOnNewDocument",
            Some(json!({ "source": script })),
            Some(session_id),
        )
        .await?;

    Ok(())
}

/// Enable Fetch-based network interception for domain filtering.
/// This intercepts all requests and checks them against the allowed domains list.
/// The actual handling of `Fetch.requestPaused` events happens in
/// `resolve_fetch_paused` in the actions module.
pub async fn install_domain_filter_fetch(
    client: &CdpClient,
    session_id: &str,
    handle_auth_requests: bool,
) -> Result<(), String> {
    let mut params = json!({
        "patterns": [{ "urlPattern": "*" }]
    });
    if handle_auth_requests {
        params["handleAuthRequests"] = json!(true);
    }
    client
        .send_command("Fetch.enable", Some(params), Some(session_id))
        .await?;
    Ok(())
}

/// Install both layers of domain filtering on a session:
/// 1. JS patching (WebSocket, EventSource, sendBeacon)
/// 2. Fetch-based network interception
pub async fn install_domain_filter(
    client: &CdpClient,
    session_id: &str,
    filter: &DomainFilter,
    handle_auth_requests: bool,
) -> Result<(), String> {
    install_domain_filter_script(client, session_id, filter).await?;
    install_domain_filter_fetch(client, session_id, handle_auth_requests).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Console arg formatting (CDP RemoteObject → human-readable string)
// ---------------------------------------------------------------------------

/// Format a single CDP RemoteObject arg into a human-readable string.
/// Priority: value → preview → description.
pub fn format_console_arg(arg: &Value) -> Option<String> {
    let obj_type = arg.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let subtype = arg.get("subtype").and_then(|v| v.as_str());

    if obj_type == "undefined" {
        return Some("undefined".to_string());
    }

    if subtype == Some("null") {
        return Some("null".to_string());
    }

    // Primitive value
    if let Some(v) = arg.get("value") {
        return Some(match v {
            Value::String(s) => s.clone(),
            Value::Null => "null".to_string(),
            other => other.to_string(),
        });
    }

    // Skip preview for Map/Set — their description ("Map(1)", "Set(3)") is more useful
    // than their preview properties (which only show "size")
    if let Some(preview) = arg.get("preview") {
        let preview_subtype = preview.get("subtype").and_then(|v| v.as_str());
        if matches!(preview_subtype, Some("map" | "set" | "weakmap" | "weakset")) {
            return arg
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
        let is_array = subtype == Some("array") || preview_subtype == Some("array");
        if let Some(props) = preview.get("properties").and_then(|v| v.as_array()) {
            let overflow = preview
                .get("overflow")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let formatted_props: Vec<String> = props
                .iter()
                .filter_map(|p| {
                    let value_str = p.get("value").and_then(|v| v.as_str())?;
                    let prop_type = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    let formatted_value = if prop_type == "string" {
                        format!("\"{}\"", value_str)
                    } else {
                        value_str.to_string()
                    };
                    if is_array {
                        Some(formatted_value)
                    } else {
                        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        Some(format!("{}: {}", name, formatted_value))
                    }
                })
                .collect();

            let inner = if overflow {
                format!("{}, ...", formatted_props.join(", "))
            } else {
                formatted_props.join(", ")
            };

            return if is_array {
                Some(format!("[{}]", inner))
            } else {
                Some(format!("{{{}}}", inner))
            };
        }
    }

    // Fallback to description
    arg.get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Format an array of CDP RemoteObject args into a single space-separated string.
pub fn format_console_args(args: &[Value]) -> String {
    args.iter()
        .filter_map(format_console_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Console and error tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ConsoleEntry {
    pub level: String,
    pub text: String,
    pub args: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ErrorEntry {
    pub text: String,
    pub url: Option<String>,
    pub line: Option<i64>,
    pub column: Option<i64>,
}

pub struct EventTracker {
    pub console_entries: Vec<ConsoleEntry>,
    pub error_entries: Vec<ErrorEntry>,
    pub max_entries: usize,
}

impl EventTracker {
    pub fn new() -> Self {
        Self {
            console_entries: Vec::new(),
            error_entries: Vec::new(),
            max_entries: 1000,
        }
    }

    pub fn add_console(&mut self, level: &str, text: &str, args: Vec<Value>) {
        if self.console_entries.len() >= self.max_entries {
            self.console_entries.remove(0);
        }
        self.console_entries.push(ConsoleEntry {
            level: level.to_string(),
            text: text.to_string(),
            args,
        });
    }

    pub fn add_error(
        &mut self,
        text: &str,
        url: Option<&str>,
        line: Option<i64>,
        col: Option<i64>,
    ) {
        if self.error_entries.len() >= self.max_entries {
            self.error_entries.remove(0);
        }
        self.error_entries.push(ErrorEntry {
            text: text.to_string(),
            url: url.map(String::from),
            line,
            column: col,
        });
    }

    pub fn clear_console(&mut self) {
        self.console_entries.clear();
    }

    pub fn get_console_json(&self) -> Value {
        let messages: Vec<Value> = self
            .console_entries
            .iter()
            .map(|e| {
                let mut msg = json!({ "type": e.level, "text": e.text });
                if !e.args.is_empty() {
                    msg.as_object_mut()
                        .unwrap()
                        .insert("args".to_string(), Value::Array(e.args.clone()));
                }
                msg
            })
            .collect();
        json!({ "messages": messages })
    }

    pub fn get_errors_json(&self) -> Value {
        let entries: Vec<Value> = self
            .error_entries
            .iter()
            .map(|e| {
                json!({
                    "text": e.text,
                    "url": e.url,
                    "line": e.line,
                    "column": e.column,
                })
            })
            .collect();
        json!({ "errors": entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_filter_exact() {
        let filter = DomainFilter::new("example.com");
        assert!(filter.is_allowed("example.com"));
        assert!(!filter.is_allowed("other.com"));
    }

    #[test]
    fn test_domain_filter_wildcard() {
        let filter = DomainFilter::new("*.example.com");
        assert!(filter.is_allowed("example.com"));
        assert!(filter.is_allowed("api.example.com"));
        assert!(filter.is_allowed("sub.api.example.com"));
        assert!(!filter.is_allowed("other.com"));
    }

    #[test]
    fn test_domain_filter_empty() {
        let filter = DomainFilter::new("");
        assert!(filter.is_allowed("anything.com"));
    }

    #[test]
    fn test_domain_filter_multiple() {
        let filter = DomainFilter::new("example.com, *.api.io");
        assert!(filter.is_allowed("example.com"));
        assert!(filter.is_allowed("api.io"));
        assert!(filter.is_allowed("v1.api.io"));
        assert!(!filter.is_allowed("other.com"));
    }

    #[test]
    fn test_parse_domain_list() {
        let domains = parse_domain_list("A.com, B.com , *.C.com");
        assert_eq!(domains, vec!["a.com", "b.com", "*.c.com"]);
    }

    #[test]
    fn test_split_filter_navigation_only() {
        let filter = DomainFilter::with_split("", Some("myapp.com"), None);
        // Navigation restricted to myapp.com
        assert!(filter.is_navigation_allowed("myapp.com"));
        assert!(!filter.is_navigation_allowed("evil.com"));
        // Resources unrestricted (no resource_domains, no allowed_domains)
        assert!(filter.is_resource_allowed("anything.com"));
        assert!(filter.is_resource_allowed("cdn.example.com"));
    }

    #[test]
    fn test_split_filter_resource_only() {
        let filter = DomainFilter::with_split("", None, Some("cdn.example.com"));
        // Navigation unrestricted (no navigation_domains, no allowed_domains)
        assert!(filter.is_navigation_allowed("anywhere.com"));
        // Resources restricted to cdn.example.com
        assert!(filter.is_resource_allowed("cdn.example.com"));
        assert!(!filter.is_resource_allowed("other.com"));
    }

    #[test]
    fn test_split_filter_both() {
        let filter = DomainFilter::with_split(
            "",
            Some("myapp.com, *.myapp.com"),
            Some("*.cdn.net, *.api.io"),
        );
        // Navigation: only myapp.com
        assert!(filter.is_navigation_allowed("myapp.com"));
        assert!(filter.is_navigation_allowed("sub.myapp.com"));
        assert!(!filter.is_navigation_allowed("evil.com"));
        // Resources: only cdn.net and api.io
        assert!(filter.is_resource_allowed("img.cdn.net"));
        assert!(filter.is_resource_allowed("v1.api.io"));
        assert!(!filter.is_resource_allowed("evil.com"));
    }

    #[test]
    fn test_split_filter_fallback_to_allowed_domains() {
        // When split fields are empty, falls back to allowed_domains
        let filter = DomainFilter::with_split("example.com, *.example.com", None, None);
        assert!(filter.is_navigation_allowed("example.com"));
        assert!(!filter.is_navigation_allowed("other.com"));
        assert!(filter.is_resource_allowed("sub.example.com"));
        assert!(!filter.is_resource_allowed("other.com"));
    }

    #[test]
    fn test_split_filter_navigation_overrides_allowed() {
        // navigation_domains takes priority over allowed_domains for navigation
        let filter = DomainFilter::with_split(
            "legacy.com",
            Some("myapp.com"),
            None,
        );
        // Navigation uses navigation_domains, not allowed_domains
        assert!(filter.is_navigation_allowed("myapp.com"));
        assert!(!filter.is_navigation_allowed("legacy.com"));
        // Resources fall back to allowed_domains
        assert!(filter.is_resource_allowed("legacy.com"));
        assert!(!filter.is_resource_allowed("other.com"));
    }

    #[test]
    fn test_split_filter_check_navigation_url() {
        let filter = DomainFilter::with_split("", Some("myapp.com"), None);
        assert!(filter.check_navigation_url("https://myapp.com/page").is_ok());
        assert!(filter.check_navigation_url("https://evil.com/page").is_err());
        // Resources still unrestricted
        assert!(filter.is_resource_allowed("evil.com"));
    }

    #[test]
    fn test_split_filter_is_active() {
        assert!(!DomainFilter::with_split("", None, None).is_active());
        assert!(DomainFilter::with_split("example.com", None, None).is_active());
        assert!(DomainFilter::with_split("", Some("example.com"), None).is_active());
        assert!(DomainFilter::with_split("", None, Some("example.com")).is_active());
    }

    // -- glob_match_domain: mid-segment wildcards --

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match_domain("example.com", "example.com"));
        assert!(!glob_match_domain("example.com", "other.com"));
    }

    #[test]
    fn test_glob_match_leading_wildcard() {
        assert!(glob_match_domain("*.example.com", "example.com"));
        assert!(glob_match_domain("*.example.com", "sub.example.com"));
        assert!(glob_match_domain("*.example.com", "deep.sub.example.com"));
        assert!(!glob_match_domain("*.example.com", "other.com"));
    }

    #[test]
    fn test_glob_match_mid_segment_wildcard() {
        assert!(glob_match_domain(
            "leonardo-platform-*.vercel.app",
            "leonardo-platform-abc.vercel.app"
        ));
        assert!(glob_match_domain(
            "leonardo-platform-*.vercel.app",
            "leonardo-platform-git-feat-xyz-leonardo-ai.vercel.app"
        ));
        assert!(!glob_match_domain(
            "leonardo-platform-*.vercel.app",
            "other-platform-abc.vercel.app"
        ));
        assert!(!glob_match_domain(
            "leonardo-platform-*.vercel.app",
            "leonardo-platform.vercel.app" // missing the dash after platform
        ));
    }

    // -- ceiling filter --

    #[test]
    fn test_ceiling_accepts_matching_domain() {
        let result = filter_by_ceiling(vec!["leonardo.ai".to_string()]);
        assert_eq!(result, vec!["leonardo.ai"]);
    }

    #[test]
    fn test_ceiling_rejects_unknown_domain() {
        let result = filter_by_ceiling(vec!["evil.com".to_string()]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_ceiling_wildcard_subdomain() {
        let result = filter_by_ceiling(vec!["app.leonardo.ai".to_string()]);
        assert_eq!(result, vec!["app.leonardo.ai"]);
    }

    #[test]
    fn test_ceiling_midlabel_wildcard() {
        let result = filter_by_ceiling(vec![
            "leonardo-platform-git-feat-xyz.vercel.app".to_string(),
        ]);
        assert_eq!(
            result,
            vec!["leonardo-platform-git-feat-xyz.vercel.app"]
        );
    }

    #[test]
    fn test_ceiling_empty_config_stays_empty() {
        let result = filter_by_ceiling(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_ceiling_config_wildcard_covered() {
        // Config entry `*.leonardo.ai` is covered by ceiling `*.leonardo.ai`
        let result = filter_by_ceiling(vec!["*.leonardo.ai".to_string()]);
        assert_eq!(result, vec!["*.leonardo.ai"]);
    }

    #[test]
    fn test_ceiling_localhost_accepted() {
        let result = filter_by_ceiling(vec!["localhost".to_string()]);
        assert_eq!(result, vec!["localhost"]);
    }

    #[test]
    fn test_ceiling_mixed_accept_reject() {
        let result = filter_by_ceiling(vec![
            "leonardo.ai".to_string(),
            "evil.com".to_string(),
            "app.leonardo.ai".to_string(),
        ]);
        assert_eq!(result, vec!["leonardo.ai", "app.leonardo.ai"]);
    }

    #[test]
    fn test_ceiling_does_not_affect_resource_domains() {
        // Ceiling only applies via filter_by_ceiling on navigation domains.
        // Resource domains are never passed through the ceiling filter,
        // so any domain works in resource_domains.
        let filter = DomainFilter::with_split("", None, Some("cdn.evil.com"));
        assert!(filter.is_resource_allowed("cdn.evil.com"));
    }

    // -- is_localhost --

    #[test]
    fn test_is_localhost() {
        assert!(is_localhost("localhost"));
        assert!(is_localhost("127.0.0.1"));
        assert!(is_localhost("0.0.0.0"));
        assert!(is_localhost("[::1]"));
        assert!(is_localhost("::1"));
    }

    #[test]
    fn test_is_localhost_rejects_remote() {
        assert!(!is_localhost("example.com"));
        assert!(!is_localhost("leonardo.ai"));
        assert!(!is_localhost("192.168.1.1"));
    }

    #[test]
    fn test_event_tracker() {
        let mut tracker = EventTracker::new();
        tracker.add_console("log", "hello", vec![]);
        tracker.add_error("oops", Some("test.js"), Some(1), Some(5));

        assert_eq!(tracker.console_entries.len(), 1);
        assert_eq!(tracker.error_entries.len(), 1);
    }

    #[test]
    fn test_console_json_includes_args() {
        let mut tracker = EventTracker::new();
        let raw_args = vec![
            json!({"type": "string", "value": "hello"}),
            json!({"type": "number", "value": 42}),
        ];
        tracker.add_console("log", "hello 42", raw_args);

        let result = tracker.get_console_json();
        let messages = result.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].get("text").unwrap(), "hello 42");
        let args = messages[0].get("args").unwrap().as_array().unwrap();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], json!({"type": "string", "value": "hello"}));
        assert_eq!(args[1], json!({"type": "number", "value": 42}));
    }

    #[test]
    fn test_console_json_empty_args_omits_field() {
        let mut tracker = EventTracker::new();
        tracker.add_console("log", "text only", vec![]);

        let result = tracker.get_console_json();
        let messages = result.get("messages").unwrap().as_array().unwrap();
        assert!(messages[0].get("args").is_none());
    }

    // -- format_console_arg: primitives --

    #[test]
    fn test_format_arg_string() {
        let arg = json!({"type": "string", "value": "hello"});
        assert_eq!(format_console_arg(&arg), Some("hello".to_string()));
    }

    #[test]
    fn test_format_arg_number() {
        let arg = json!({"type": "number", "value": 42});
        assert_eq!(format_console_arg(&arg), Some("42".to_string()));
    }

    #[test]
    fn test_format_arg_null() {
        let arg = json!({"type": "object", "subtype": "null", "value": null});
        assert_eq!(format_console_arg(&arg), Some("null".to_string()));
    }

    #[test]
    fn test_format_arg_undefined() {
        let arg = json!({"type": "undefined"});
        assert_eq!(format_console_arg(&arg), Some("undefined".to_string()));
    }

    // -- format_console_arg: objects with preview --

    #[test]
    fn test_format_arg_object_preview() {
        let arg = json!({
            "type": "object",
            "preview": {
                "properties": [
                    {"name": "userId", "type": "string", "value": "abc123"},
                    {"name": "count", "type": "number", "value": "42"}
                ],
                "overflow": false
            }
        });
        assert_eq!(
            format_console_arg(&arg),
            Some("{userId: \"abc123\", count: 42}".to_string())
        );
    }

    #[test]
    fn test_format_arg_object_preview_overflow() {
        let arg = json!({
            "type": "object",
            "preview": {
                "properties": [
                    {"name": "a", "type": "number", "value": "1"}
                ],
                "overflow": true
            }
        });
        assert_eq!(format_console_arg(&arg), Some("{a: 1, ...}".to_string()));
    }

    // -- format_console_arg: arrays with preview --

    #[test]
    fn test_format_arg_array_preview() {
        let arg = json!({
            "type": "object",
            "subtype": "array",
            "preview": {
                "subtype": "array",
                "properties": [
                    {"name": "0", "type": "number", "value": "1"},
                    {"name": "1", "type": "number", "value": "2"},
                    {"name": "2", "type": "number", "value": "3"}
                ],
                "overflow": false
            }
        });
        assert_eq!(format_console_arg(&arg), Some("[1, 2, 3]".to_string()));
    }

    // -- format_console_arg: map/set use description --

    #[test]
    fn test_format_arg_map_uses_description() {
        let arg = json!({
            "type": "object",
            "subtype": "map",
            "description": "Map(1)",
            "preview": {
                "subtype": "map",
                "properties": [{"name": "size", "type": "number", "value": "1"}]
            }
        });
        assert_eq!(format_console_arg(&arg), Some("Map(1)".to_string()));
    }

    // -- format_console_arg: fallback --

    #[test]
    fn test_format_arg_description_fallback() {
        let arg = json!({"type": "object", "description": "RegExp"});
        assert_eq!(format_console_arg(&arg), Some("RegExp".to_string()));
    }

    #[test]
    fn test_format_arg_no_value_no_preview_no_description() {
        let arg = json!({"type": "object"});
        assert_eq!(format_console_arg(&arg), None);
    }

    // -- format_console_args --

    #[test]
    fn test_format_console_args_join() {
        let args = vec![
            json!({"type": "string", "value": "user"}),
            json!({
                "type": "object",
                "preview": {
                    "properties": [{"name": "id", "type": "number", "value": "1"}],
                    "overflow": false
                }
            }),
        ];
        assert_eq!(format_console_args(&args), "user {id: 1}");
    }

    #[test]
    fn test_format_console_args_filters_none() {
        // An arg that returns None should be skipped, not produce empty string
        let args = vec![
            json!({"type": "string", "value": "before"}),
            json!({"type": "object"}), // no value, preview, or description → None
            json!({"type": "string", "value": "after"}),
        ];
        assert_eq!(format_console_args(&args), "before after");
    }
}
