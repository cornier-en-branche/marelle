// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! The HTTP client: TLS, proxy, timeouts and one chat request.
//!
//! Decisions that matter:
//!
//! * TLS is rustls with the `ring` provider and the bundled Mozilla roots, so
//!   it behaves the same on Linux, Termux and anywhere else, with no system
//!   library involved. Extra roots come from `network.ca_bundle` and from
//!   `SSL_CERT_FILE`, which is how the certificate of a corporate proxy is
//!   trusted.
//! * Redirects are never followed. A redirect could carry the API key to a
//!   host the user never named.
//! * A key is never sent over plain HTTP to another machine unless the user
//!   allows it with `network.allow_insecure_http`.
//! * The key lives in a [`Secret`] and in a sensitive header value. Whatever a
//!   server sends back is scrubbed of the key before it becomes an error.

use std::error::Error as _;
use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use marelle_config::{Config, Expected, Layer, SettingError};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Response, StatusCode, Url};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use serde_json::Value;

use crate::chat::{ChatOutcome, ChatRequest, Sink};
use crate::config::{AuthStyle, ProviderConfig, ProviderKind, expand_home, text_setting};
use crate::decode::{Decode, error_text, snippet};
use crate::error::{ProviderError, protocol};
use crate::secret::Secret;
use crate::sse::SseParser;
use crate::{anthropic, openai};

const CONNECT_TIMEOUT_KEY: &str = "network.connect_timeout_secs";
const IDLE_TIMEOUT_KEY: &str = "network.idle_timeout_secs";
const CA_BUNDLE_KEY: &str = "network.ca_bundle";
const PROXY_KEY: &str = "network.proxy";
const INSECURE_HTTP_KEY: &str = "network.allow_insecure_http";

const DEFAULT_CONNECT_SECS: u64 = 15;
const DEFAULT_IDLE_SECS: u64 = 300;
/// No timeout above a day makes sense, and absurd values could overflow a
/// clock somewhere.
const MAX_TIMEOUT_SECS: u64 = 86_400;

/// How much of an error response is read.
const ERROR_BODY_LIMIT: usize = 16 * 1024;
/// The largest complete answer accepted when it is not streamed.
const RESPONSE_LIMIT: usize = 64 * 1024 * 1024;

const USER_AGENT: &str = concat!("marelle/", env!("CARGO_PKG_VERSION"));

/// How to reach the network.
#[derive(Clone, PartialEq, Eq)]
pub enum ProxySetting {
    /// Follow `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY`.
    System,
    /// Connect directly, whatever the environment says.
    None,
    /// Use this proxy.
    Url(String),
}

impl fmt::Debug for ProxySetting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProxySetting::System => f.write_str("System"),
            ProxySetting::None => f.write_str("None"),
            // The URL may carry a password.
            ProxySetting::Url(_) => f.write_str("Url(***)"),
        }
    }
}

/// Settings that apply to every provider.
#[derive(Clone, PartialEq, Eq)]
pub struct NetworkSettings {
    /// How long to wait for a connection.
    pub connect_timeout: Duration,
    /// How long the provider may stay silent, before the answer starts and
    /// between the pieces of a stream.
    pub idle_timeout: Duration,
    /// Proxy behaviour.
    pub proxy: ProxySetting,
    /// Extra trusted CA certificates, DER encoded.
    pub extra_roots: Vec<Vec<u8>>,
    /// Whether a key may travel over plain HTTP to another machine.
    pub allow_insecure_http: bool,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(DEFAULT_CONNECT_SECS),
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_SECS),
            proxy: ProxySetting::System,
            extra_roots: Vec::new(),
            allow_insecure_http: false,
        }
    }
}

impl fmt::Debug for NetworkSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkSettings")
            .field("connect_timeout", &self.connect_timeout)
            .field("idle_timeout", &self.idle_timeout)
            .field("proxy", &self.proxy)
            .field(
                "extra_roots",
                &format_args!("{} certificate(s)", self.extra_roots.len()),
            )
            .field("allow_insecure_http", &self.allow_insecure_http)
            .finish()
    }
}

impl NetworkSettings {
    /// Registers the defaults, the lowest layer of settings.
    pub fn register_defaults(config: &mut Config) {
        config.set(
            Layer::Default,
            CONNECT_TIMEOUT_KEY,
            DEFAULT_CONNECT_SECS.to_string(),
        );
        config.set(
            Layer::Default,
            IDLE_TIMEOUT_KEY,
            DEFAULT_IDLE_SECS.to_string(),
        );
        config.set(Layer::Default, PROXY_KEY, "system");
        config.set(Layer::Default, INSECURE_HTTP_KEY, "false");
    }

    /// Reads the `network.*` settings.
    ///
    /// `lookup` reads environment variables. It supplies `SSL_CERT_FILE`, and
    /// `HOME` for `~/` in paths. A bundle named by `network.ca_bundle` must be
    /// readable and hold at least one certificate. A bad `SSL_CERT_FILE` is
    /// ignored, because it is usually set for other programs.
    pub fn from_config(
        config: &Config,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ProviderError> {
        let seconds = |key: &str, default: u64| -> Result<Duration, SettingError> {
            let value = config
                .get_u64(key)
                .map_err(|_| SettingError::new(key, Expected::PositiveWholeNumber))?;
            match value {
                None => Ok(Duration::from_secs(default)),
                Some(0) => Err(SettingError::new(key, Expected::PositiveWholeNumber)),
                Some(value) => Ok(Duration::from_secs(value.min(MAX_TIMEOUT_SECS))),
            }
        };
        let connect_timeout = seconds(CONNECT_TIMEOUT_KEY, DEFAULT_CONNECT_SECS)?;
        let idle_timeout = seconds(IDLE_TIMEOUT_KEY, DEFAULT_IDLE_SECS)?;

        let proxy = match text_setting(config, PROXY_KEY) {
            None => ProxySetting::System,
            Some(text) => {
                parse_proxy(&text).ok_or_else(|| SettingError::new(PROXY_KEY, Expected::Proxy))?
            }
        };

        let allow_insecure_http = config.get_bool(INSECURE_HTTP_KEY)?.unwrap_or(false);

        let mut extra_roots = Vec::new();
        if let Some(path) = text_setting(config, CA_BUNDLE_KEY) {
            let path = expand_home(&path, &lookup);
            let shown = path.display().to_string();
            let pem = std::fs::read(&path).map_err(|error| ProviderError::CaBundleFile {
                path: shown.clone(),
                reason: error.to_string(),
            })?;
            let certificates = usable_certificates(&pem);
            if certificates.is_empty() {
                return Err(ProviderError::CaBundle { source: shown });
            }
            extra_roots.extend(certificates);
        }
        if let Some(path) = lookup("SSL_CERT_FILE").filter(|path| !path.trim().is_empty()) {
            if let Ok(pem) = std::fs::read(path.trim()) {
                extra_roots.extend(usable_certificates(&pem));
            }
        }

        Ok(Self {
            connect_timeout,
            idle_timeout,
            proxy,
            extra_roots,
            allow_insecure_http,
        })
    }
}

fn parse_proxy(text: &str) -> Option<ProxySetting> {
    match text.to_ascii_lowercase().as_str() {
        "system" => Some(ProxySetting::System),
        "none" => Some(ProxySetting::None),
        _ => {
            let url = Url::parse(text).ok()?;
            let usable = matches!(url.scheme(), "http" | "https") && url.host_str().is_some();
            usable.then(|| ProxySetting::Url(text.to_owned()))
        }
    }
}

/// The certificates in a PEM file that rustls accepts as trust anchors, DER
/// encoded. Other sections, such as private keys, are skipped.
fn usable_certificates(pem: &[u8]) -> Vec<Vec<u8>> {
    let mut probe = rustls::RootCertStore::empty();
    CertificateDer::pem_slice_iter(pem)
        .filter_map(Result::ok)
        .filter(|certificate| probe.add(certificate.clone()).is_ok())
        .map(|certificate| certificate.as_ref().to_vec())
        .collect()
}

fn tls_config(extra_roots: &[Vec<u8>]) -> Result<rustls::ClientConfig, ProviderError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for der in extra_roots {
        // The certificates were checked when the settings were read.
        let _ = roots.add(CertificateDer::from(der.clone()));
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| ProviderError::Transport {
            detail: format!("cannot set up TLS: {error}"),
        })?
        .with_root_certificates(roots)
        .with_no_client_auth();
    // Only HTTP/1.1 is built in, so say so instead of letting a server pick h2.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// Whether the URL points at this machine.
fn is_local(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn auth_header(
    provider: &ProviderConfig,
    key: &Secret,
) -> Result<Option<(HeaderName, HeaderValue)>, ProviderError> {
    let (name, text) = match provider.auth {
        AuthStyle::Bearer => (AUTHORIZATION, format!("Bearer {}", key.expose())),
        AuthStyle::XApiKey => (
            HeaderName::from_static("x-api-key"),
            key.expose().to_owned(),
        ),
        AuthStyle::ApiKey => (HeaderName::from_static("api-key"), key.expose().to_owned()),
        AuthStyle::None => return Ok(None),
    };
    let mut value = HeaderValue::from_str(&text).map_err(|_| ProviderError::InvalidApiKey)?;
    // Keeps the value out of debug output and out of HTTP/2 header compression.
    value.set_sensitive(true);
    Ok(Some((name, value)))
}

/// Talks to one provider.
pub struct Client {
    http: reqwest::Client,
    provider: ProviderConfig,
    key: Option<Secret>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("provider", &self.provider.name)
            .field("base_url", &self.provider.base_url)
            .field("key", &self.key)
            .finish()
    }
}

impl Client {
    /// Prepares a client for `provider`.
    ///
    /// `key` is `None` for providers that need none. Nothing goes over the
    /// network until [`Client::chat`] is called.
    pub fn new(
        provider: &ProviderConfig,
        key: Option<Secret>,
        network: &NetworkSettings,
    ) -> Result<Self, ProviderError> {
        let url = Url::parse(&provider.base_url).map_err(|_| {
            SettingError::new(
                format!("providers.{}.base_url", provider.name),
                Expected::Url,
            )
        })?;

        let sends_key = key.is_some() && provider.auth != AuthStyle::None;
        if sends_key && url.scheme() == "http" && !is_local(&url) && !network.allow_insecure_http {
            return Err(ProviderError::InsecureTransport {
                host: url.host_str().unwrap_or_default().to_owned(),
            });
        }

        let mut headers = HeaderMap::new();
        if provider.kind == ProviderKind::Anthropic {
            headers.insert(
                HeaderName::from_static("anthropic-version"),
                HeaderValue::from_static(anthropic::VERSION),
            );
        }
        if let Some((name, value)) = key
            .as_ref()
            .map(|key| auth_header(provider, key))
            .transpose()?
            .flatten()
        {
            headers.insert(name, value);
        }

        let mut builder = reqwest::Client::builder()
            .tls_backend_preconfigured(tls_config(&network.extra_roots)?)
            .http1_only()
            // A redirect could take the key to a host the user never named.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(network.connect_timeout)
            .read_timeout(network.idle_timeout)
            .user_agent(USER_AGENT)
            .default_headers(headers);
        builder = match &network.proxy {
            ProxySetting::System => builder,
            ProxySetting::None => builder.no_proxy(),
            ProxySetting::Url(text) => {
                let proxy = reqwest::Proxy::all(text.as_str())
                    .map_err(|_| SettingError::new(PROXY_KEY, Expected::Proxy))?;
                builder.proxy(proxy)
            }
        };
        let http = builder.build().map_err(transport)?;

        Ok(Self {
            http,
            provider: provider.clone(),
            key,
        })
    }

    /// Sends the request and waits for the whole answer.
    ///
    /// Pieces of the answer are passed to `sink` as they arrive. A sink that
    /// returns [`std::ops::ControlFlow::Break`] ends the request early; the
    /// outcome then reports [`crate::FinishReason::Interrupted`].
    pub async fn chat(
        &self,
        request: &ChatRequest,
        sink: &mut Sink<'_>,
    ) -> Result<ChatOutcome, ProviderError> {
        self.exchange(request, sink)
            .await
            .map_err(|error| self.scrubbed(error))
    }

    fn prepare(&self, request: &ChatRequest) -> Result<Prepared, ProviderError> {
        let provider = &self.provider;
        let (url, body, decoder): (String, Value, Box<dyn Decode>) = match provider.kind {
            ProviderKind::OpenAiCompatible => (
                openai::endpoint(&provider.base_url),
                openai::build_body(provider, request)?,
                Box::new(openai::Decoder::new()),
            ),
            ProviderKind::Anthropic => (
                anthropic::endpoint(&provider.base_url),
                anthropic::build_body(provider, request)?,
                Box::new(anthropic::Decoder::new()),
            ),
        };
        let bytes = serde_json::to_vec(&body)
            .map_err(|error| protocol(format!("cannot encode the request: {error}")))?;
        Ok(Prepared {
            url,
            body: bytes,
            decoder,
        })
    }

    async fn exchange(
        &self,
        request: &ChatRequest,
        sink: &mut Sink<'_>,
    ) -> Result<ChatOutcome, ProviderError> {
        let Prepared { url, body, decoder } = self.prepare(request)?;
        let accept = if request.stream {
            "text/event-stream"
        } else {
            "application/json"
        };
        let response = self
            .http
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, accept)
            .body(body)
            .send()
            .await
            .map_err(transport)?;

        if !response.status().is_success() {
            return Err(http_error(response).await);
        }
        // Some servers ignore `stream: true` and answer with plain JSON. That
        // is read as a complete answer instead of failing.
        if request.stream && !is_json(&response) {
            read_stream(response, decoder, sink).await
        } else {
            let body = read_whole(response).await?;
            decoder.complete(&body, sink)
        }
    }

    /// Removes the key from text that came from the server.
    fn scrubbed(&self, error: ProviderError) -> ProviderError {
        let Some(key) = &self.key else {
            return error;
        };
        match error {
            ProviderError::Http { status, message } => ProviderError::Http {
                status,
                message: key.scrub(&message),
            },
            ProviderError::Api { message } => ProviderError::Api {
                message: key.scrub(&message),
            },
            ProviderError::Transport { detail } => ProviderError::Transport {
                detail: key.scrub(&detail),
            },
            ProviderError::Protocol { detail } => ProviderError::Protocol {
                detail: key.scrub(&detail),
            },
            other => other,
        }
    }
}

/// A request ready to send, with the decoder that will read its answer.
struct Prepared {
    url: String,
    body: Vec<u8>,
    decoder: Box<dyn Decode>,
}

fn is_json(response: &Response) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("json"))
}

async fn read_stream(
    mut response: Response,
    mut decoder: Box<dyn Decode>,
    sink: &mut Sink<'_>,
) -> Result<ChatOutcome, ProviderError> {
    let mut parser = SseParser::new();
    while let Some(chunk) = response.chunk().await.map_err(transport)? {
        for event in parser.push(&chunk)? {
            if decoder.feed(&event, sink)?.is_break() {
                return Ok(decoder.interrupted());
            }
        }
        // Once the provider says it is done there is nothing left to wait for.
        if decoder.done() {
            return decoder.finish();
        }
    }
    for event in parser.finish() {
        if decoder.feed(&event, sink)?.is_break() {
            return Ok(decoder.interrupted());
        }
    }
    decoder.finish()
}

async fn read_whole(mut response: Response) -> Result<Vec<u8>, ProviderError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport)? {
        if body.len() + chunk.len() > RESPONSE_LIMIT {
            return Err(protocol("the response is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn http_error(mut response: Response) -> ProviderError {
    let status = response.status();
    let mut body = Vec::new();
    while body.len() < ERROR_BODY_LIMIT {
        match response.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            // The status is what matters. A broken body adds nothing.
            _ => break,
        }
    }
    body.truncate(ERROR_BODY_LIMIT);
    ProviderError::Http {
        status: status.as_u16(),
        message: describe_error_body(&body, status),
    }
}

/// The useful part of an error response: the provider's message when the
/// body is JSON, the start of the text when it is not, and the standard
/// reason phrase when there is nothing readable.
fn describe_error_body(body: &[u8], status: StatusCode) -> String {
    let text = String::from_utf8_lossy(body);
    if let Ok(value) = serde_json::from_str::<Value>(&text) {
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            return error_text(error);
        }
        if value.get("message").or_else(|| value.get("msg")).is_some() {
            return error_text(&value);
        }
    }
    let trimmed = text.trim();
    // An HTML error page from a proxy or a CDN is noise.
    let looks_like_html = trimmed.starts_with('<');
    if trimmed.is_empty() || looks_like_html {
        return status
            .canonical_reason()
            .unwrap_or("no details were given")
            .to_owned();
    }
    snippet(trimmed)
}

fn transport(error: reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        return ProviderError::Timeout;
    }
    ProviderError::Transport {
        detail: describe(error),
    }
}

/// The whole chain of causes, without the URL.
fn describe(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut parts = vec![error.to_string()];
    let mut cause = error.source();
    while let Some(current) = cause {
        let text = current.to_string();
        // Some layers repeat their cause in their own message.
        if !parts.iter().any(|part| part.contains(&text)) {
            parts.push(text);
        }
        cause = current.source();
    }
    parts.join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MaxTokensField;
    use std::path::PathBuf;

    /// A throwaway certificate, made for these tests. It authenticates nothing.
    const TEST_CA: &str = "-----BEGIN CERTIFICATE-----
MIIByjCCAW+gAwIBAgIUIZNQ1AOpipwLBmoQZGiMfMAK2fQwCgYIKoZIzj0EAwIw
MTEvMC0GA1UEAwwmbWFyZWxsZSB0ZXN0IENBIChub3QgYSByZWFsIGF1dGhvcml0
eSkwIBcNMjYxMTA3MTk0MDEyWhgPMjEyNjEwMTQxOTQwMTJaMDExLzAtBgNVBAMM
Jm1hcmVsbGUgdGVzdCBDQSAobm90IGEgcmVhbCBhdXRob3JpdHkpMFkwEwYHKoZI
zj0CAQYIKoZIzj0DAQcDQgAEtLrgEMruFjh5l19cResYkVR4k3q9WQpGLhb3N5KT
rjEg3/HEReHYwkE+A0qkE8pnpG+mVCOgYkOMJKaMsBk7g6NjMGEwHQYDVR0OBBYE
FOaYcHto3L2bN/9ExeMoB1BX656ZMB8GA1UdIwQYMBaAFOaYcHto3L2bN/9ExeMo
B1BX656ZMA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0PAQH/BAQDAgEGMAoGCCqGSM49
BAMCA0kAMEYCIQDmxxbLFrVSrxAe2UrVRQso2S+2DK+o/L4IPSGVORhevgIhAPtg
6LYv4QFbo25XpaUkmdqVkOOWHUat4mN7YdfvgXe3
-----END CERTIFICATE-----
";

    const KEY: &str = "sk-test-0123456789abcdef";

    fn provider(base_url: &str) -> ProviderConfig {
        ProviderConfig {
            name: "t".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: base_url.into(),
            api_key_env: Some("T_KEY".into()),
            api_key_file: None,
            auth: AuthStyle::Bearer,
            max_tokens_field: MaxTokensField::MaxTokens,
            stream_usage: true,
            extra_body: serde_json::Map::new(),
        }
    }

    fn key() -> Option<Secret> {
        Some(Secret::new(KEY))
    }

    fn nothing(_: &str) -> Option<String> {
        None
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("marelle-net-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_tls_configuration_builds_and_speaks_http_1_1() {
        let config = tls_config(&[]).unwrap();
        assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);
        let extra = usable_certificates(TEST_CA.as_bytes());
        assert_eq!(extra.len(), 1);
        tls_config(&extra).unwrap();
    }

    #[test]
    fn only_real_certificates_count() {
        assert!(usable_certificates(b"").is_empty());
        assert!(usable_certificates(b"not a certificate at all").is_empty());
        let broken = TEST_CA.replace("MIIByjCC", "AAAA!!!!");
        assert!(usable_certificates(broken.as_bytes()).is_empty());
        let two = format!("{TEST_CA}\n{TEST_CA}");
        assert_eq!(usable_certificates(two.as_bytes()).len(), 2);
        let with_junk = format!("junk before\n{TEST_CA}junk after");
        assert_eq!(usable_certificates(with_junk.as_bytes()).len(), 1);
    }

    #[test]
    fn defaults_apply_without_settings() {
        let settings = NetworkSettings::from_config(&Config::new(), nothing).unwrap();
        assert_eq!(settings, NetworkSettings::default());
        let mut config = Config::new();
        NetworkSettings::register_defaults(&mut config);
        let registered = NetworkSettings::from_config(&config, nothing).unwrap();
        assert_eq!(registered, NetworkSettings::default());
    }

    #[test]
    fn settings_override_the_defaults() {
        let mut config = Config::new();
        NetworkSettings::register_defaults(&mut config);
        config.set(Layer::Env, CONNECT_TIMEOUT_KEY, "3");
        config.set(Layer::Env, IDLE_TIMEOUT_KEY, "90");
        config.set(Layer::Env, PROXY_KEY, "http://user:pw@proxy.example:3128");
        config.set(Layer::Env, INSECURE_HTTP_KEY, "yes");
        let settings = NetworkSettings::from_config(&config, nothing).unwrap();
        assert_eq!(settings.connect_timeout, Duration::from_secs(3));
        assert_eq!(settings.idle_timeout, Duration::from_secs(90));
        assert_eq!(
            settings.proxy,
            ProxySetting::Url("http://user:pw@proxy.example:3128".into())
        );
        assert!(settings.allow_insecure_http);

        config.set(Layer::Flag, PROXY_KEY, "None");
        let settings = NetworkSettings::from_config(&config, nothing).unwrap();
        assert_eq!(settings.proxy, ProxySetting::None);
        config.set(Layer::Flag, PROXY_KEY, "SYSTEM");
        let settings = NetworkSettings::from_config(&config, nothing).unwrap();
        assert_eq!(settings.proxy, ProxySetting::System);
    }

    #[test]
    fn absurd_timeouts_are_capped() {
        let mut config = Config::new();
        config.set(Layer::Env, IDLE_TIMEOUT_KEY, "18446744073709551615");
        let settings = NetworkSettings::from_config(&config, nothing).unwrap();
        assert_eq!(settings.idle_timeout, Duration::from_secs(MAX_TIMEOUT_SECS));
    }

    #[test]
    fn bad_values_name_the_setting_and_do_not_echo_the_value() {
        for (key, value, expected) in [
            (CONNECT_TIMEOUT_KEY, "0", Expected::PositiveWholeNumber),
            (
                CONNECT_TIMEOUT_KEY,
                "quickly please",
                Expected::PositiveWholeNumber,
            ),
            (IDLE_TIMEOUT_KEY, "-5", Expected::PositiveWholeNumber),
            (
                PROXY_KEY,
                "socks5://secret-user:secret-pw@host:1080",
                Expected::Proxy,
            ),
            (
                PROXY_KEY,
                "secret-pw-typed-in-the-wrong-place",
                Expected::Proxy,
            ),
            (INSECURE_HTTP_KEY, "perhaps", Expected::Bool),
        ] {
            let mut config = Config::new();
            config.set(Layer::Env, key, value);
            let error = NetworkSettings::from_config(&config, nothing).unwrap_err();
            assert_eq!(
                error,
                ProviderError::Setting(SettingError::new(key, expected))
            );
            if value.len() > 3 {
                assert!(!error.to_string().contains(value), "{key}: {error}");
            }
        }
    }

    #[test]
    fn a_ca_bundle_is_read_and_checked() {
        let dir = scratch("bundle");
        let good = dir.join("good.pem");
        let junk = dir.join("junk.pem");
        std::fs::write(&good, TEST_CA).unwrap();
        std::fs::write(&junk, "not a certificate").unwrap();

        let mut config = Config::new();
        config.set(Layer::Env, CA_BUNDLE_KEY, good.display().to_string());
        let settings = NetworkSettings::from_config(&config, nothing).unwrap();
        assert_eq!(settings.extra_roots.len(), 1);

        config.set(Layer::Flag, CA_BUNDLE_KEY, junk.display().to_string());
        assert!(matches!(
            NetworkSettings::from_config(&config, nothing).unwrap_err(),
            ProviderError::CaBundle { .. }
        ));

        config.set(
            Layer::Flag,
            CA_BUNDLE_KEY,
            dir.join("missing.pem").display().to_string(),
        );
        assert!(matches!(
            NetworkSettings::from_config(&config, nothing).unwrap_err(),
            ProviderError::CaBundleFile { .. }
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ssl_cert_file_adds_roots_but_never_fails() {
        let dir = scratch("sslcertfile");
        let good = dir.join("good.pem");
        let junk = dir.join("junk.pem");
        std::fs::write(&good, TEST_CA).unwrap();
        std::fs::write(&junk, "nope").unwrap();

        let with = |path: PathBuf| {
            let value = path.display().to_string();
            move |name: &str| (name == "SSL_CERT_FILE").then(|| value.clone())
        };
        let config = Config::new();
        let roots = |lookup| {
            NetworkSettings::from_config(&config, lookup)
                .unwrap()
                .extra_roots
        };
        assert_eq!(roots(with(good)).len(), 1);
        assert!(roots(with(junk)).is_empty());
        assert!(roots(with(dir.join("missing.pem"))).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_key_is_not_sent_over_plain_http_to_other_machines() {
        let strict = NetworkSettings::default();
        for url in [
            "http://example.com/v1",
            "http://10.0.0.5:8080",
            "http://192.168.1.2",
        ] {
            let error = Client::new(&provider(url), key(), &strict).unwrap_err();
            assert!(
                matches!(error, ProviderError::InsecureTransport { .. }),
                "{url}"
            );
        }
        for url in [
            "http://localhost:11434/v1",
            "http://LOCALHOST",
            "http://127.0.0.1:1",
            "http://127.1",
            "http://[::1]:8080",
            "http://dev.localhost/v1",
            "https://example.com",
        ] {
            assert!(Client::new(&provider(url), key(), &strict).is_ok(), "{url}");
        }

        let lax = NetworkSettings {
            allow_insecure_http: true,
            ..NetworkSettings::default()
        };
        assert!(Client::new(&provider("http://example.com"), key(), &lax).is_ok());
        assert!(Client::new(&provider("http://example.com"), None, &strict).is_ok());
        let mut open = provider("http://example.com");
        open.auth = AuthStyle::None;
        assert!(Client::new(&open, key(), &strict).is_ok(), "no key is sent");
    }

    #[test]
    fn the_refusal_names_the_host_and_not_the_key() {
        let error = Client::new(
            &provider("http://example.com/v1"),
            key(),
            &NetworkSettings::default(),
        )
        .unwrap_err();
        assert_eq!(
            error,
            ProviderError::InsecureTransport {
                host: "example.com".into()
            }
        );
        assert!(!error.to_string().contains(KEY));
    }

    #[test]
    fn a_key_that_cannot_be_a_header_is_refused_without_echoing_it() {
        let secret = Secret::new("sk-bad\nkey-0123456789");
        let error = Client::new(
            &provider("https://example.com"),
            Some(secret),
            &NetworkSettings::default(),
        )
        .unwrap_err();
        assert_eq!(error, ProviderError::InvalidApiKey);
        assert!(!error.to_string().contains("sk-bad"));
    }

    #[test]
    fn a_client_builds_with_every_proxy_setting() {
        for proxy in [
            ProxySetting::System,
            ProxySetting::None,
            ProxySetting::Url("http://user:hunter2@proxy.example:3128".into()),
        ] {
            let network = NetworkSettings {
                proxy,
                ..NetworkSettings::default()
            };
            Client::new(&provider("https://example.com"), key(), &network).unwrap();
        }
    }

    #[test]
    fn nothing_secret_shows_up_in_debug_output() {
        let network = NetworkSettings {
            proxy: ProxySetting::Url("http://user:hunter2@proxy.example:3128".into()),
            ..NetworkSettings::default()
        };
        let client = Client::new(&provider("https://example.com"), key(), &network).unwrap();
        let shown = format!("{client:?} {network:?}");
        assert!(!shown.contains(KEY), "{shown}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("example.com"));
    }

    #[test]
    fn server_text_is_scrubbed_of_the_key() {
        let client = Client::new(
            &provider("https://example.com"),
            key(),
            &NetworkSettings::default(),
        )
        .unwrap();
        let echo = format!("invalid key {KEY}!");
        for error in [
            ProviderError::Http {
                status: 401,
                message: echo.clone(),
            },
            ProviderError::Api {
                message: echo.clone(),
            },
            ProviderError::Transport {
                detail: echo.clone(),
            },
            ProviderError::Protocol {
                detail: echo.clone(),
            },
        ] {
            let shown = client.scrubbed(error).to_string();
            assert!(!shown.contains(KEY), "{shown}");
            assert!(shown.contains("invalid key ***!"), "{shown}");
        }
        assert_eq!(
            client.scrubbed(ProviderError::Timeout),
            ProviderError::Timeout
        );
    }

    #[test]
    fn error_bodies_are_summarised() {
        let status = StatusCode::UNAUTHORIZED;
        let openai = br#"{"error":{"message":"Incorrect API key","code":"invalid_api_key"}}"#;
        assert_eq!(
            describe_error_body(openai, status),
            "Incorrect API key (invalid_api_key)"
        );
        let anthropic = br#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        assert_eq!(
            describe_error_body(anthropic, status),
            "invalid x-api-key (authentication_error)"
        );
        let old_zhipu = br#"{"code":401,"msg":"token expired","success":false}"#;
        assert_eq!(
            describe_error_body(old_zhipu, status),
            "token expired (401)"
        );
        assert_eq!(
            describe_error_body(b"upstream is down\n", status),
            "upstream is down"
        );
        assert_eq!(describe_error_body(b"", status), "Unauthorized");
        assert_eq!(
            describe_error_body(b"<html><h1>502</h1></html>", status),
            "Unauthorized"
        );
        assert_eq!(
            describe_error_body(br#"{"error":null,"x":1}"#, status),
            r#"{"error":null,"x":1}"#
        );
    }

    #[test]
    fn loopback_detection() {
        for (url, local) in [
            ("http://localhost", true),
            ("http://127.0.0.1:9", true),
            ("http://[::1]", true),
            ("http://foo.localhost", true),
            ("http://localhost.evil.example", false),
            ("http://127.0.0.1.evil.example", false),
            ("http://0.0.0.0", false),
            ("http://example.com", false),
        ] {
            assert_eq!(is_local(&Url::parse(url).unwrap()), local, "{url}");
        }
    }
}
