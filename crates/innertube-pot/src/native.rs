//! In-process PO tokens: BotGuard runs in QuickJS (`botguard-rs`), so no sidecar is needed.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{PageAttestation, PoTokenProvider, PotError, PotFuture};

/// Re-attest this long before GenerateIT's stated TTL runs out.
const TTL_MARGIN: Duration = Duration::from_secs(300);

struct Attestation {
    expires: Option<Instant>,
    interpreters: HashMap<String, String>,
}

/// Mints GVS PO tokens in-process; one attestation serves every binding until its TTL.
#[derive(Clone)]
pub struct NativeProvider {
    bg: botguard::BotGuard,
    http: reqwest::Client,
    state: Arc<tokio::sync::Mutex<Attestation>>,
}

impl NativeProvider {
    /// `user_agent` goes on the interpreter fetch; use the WEB client's.
    pub fn new(user_agent: &str) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
            .build()
            .expect("reqwest client builds");
        Self {
            bg: botguard::BotGuard::new(),
            http,
            state: Arc::new(tokio::sync::Mutex::new(Attestation { expires: None, interpreters: HashMap::new() })),
        }
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(botguard::BotGuard) -> Result<T, botguard::Error> + Send + 'static,
    ) -> Result<T, PotError> {
        let bg = self.bg.clone();
        tokio::task::spawn_blocking(move || f(bg))
            .await
            .map_err(|e| PotError::Native(e.to_string()))?
            .map_err(|e| PotError::Native(e.to_string()))
    }

    async fn attest(&self, state: &mut Attestation, attest: &PageAttestation) -> Result<(), PotError> {
        let t0 = Instant::now();
        let challenge = botguard::Challenge::parse(&attest.challenge).map_err(|e| PotError::Native(e.to_string()))?;
        let interpreter = match state.interpreters.get(&challenge.interpreter_url) {
            Some(js) => js.clone(),
            None => {
                let js = self.http.get(&challenge.interpreter_url).send().await?.error_for_status()?.text().await?;
                state.interpreters.insert(challenge.interpreter_url.clone(), js.clone());
                js
            }
        };
        tracing::debug!(
            "[pot] native challenge: interpreter {} program {} chars",
            challenge.interpreter_url,
            challenge.program.len()
        );
        let capture = (attest.challenge.clone(), interpreter.clone(), attest.yt_config.clone());
        let yt_config = attest.yt_config.clone();
        let snapshot = self.blocking(move |bg| bg.snapshot(interpreter, challenge, Some(yt_config))).await?;
        let t_snap = t0.elapsed();

        let mut req = self.http.post(botguard::GENERATE_IT_URL);
        for (k, v) in botguard::GENERATE_IT_HEADERS {
            req = req.header(k, v);
        }
        // The snapshot describes a Node process, so GenerateIT must see Node's fetch user agent.
        req = req.header(reqwest::header::USER_AGENT, "node");
        let body = req.body(botguard::generate_it_request(&snapshot)).send().await?.error_for_status()?.text().await?;
        tracing::trace!("[pot] native GenerateIT: snapshot {} chars -> {}", snapshot.len(), body.chars().take(160).collect::<String>());
        let it = match botguard::parse_generate_it(&body) {
            Ok(it) => it,
            Err(e) => {
                save_refusal(&capture, &snapshot, &body);
                return Err(PotError::Refused(e.to_string()));
            }
        };
        let ttl = it.ttl;
        self.blocking(move |bg| bg.install(it.token)).await?;
        state.expires = Some(Instant::now() + ttl.saturating_sub(TTL_MARGIN));
        tracing::debug!(
            "[pot] native attestation: snapshot {:.0} ms, total {:.0} ms, ttl {} s",
            t_snap.as_secs_f64() * 1000.0,
            t0.elapsed().as_secs_f64() * 1000.0,
            ttl.as_secs()
        );
        Ok(())
    }
}

/// Under `INNERTUBE_DUMP=raw:<dir>`, a refused attestation's inputs and outputs are written
/// to `<dir>/raw/pot/` so the run can be replayed offline.
fn save_refusal(capture: &(String, String, String), snapshot: &str, response: &str) {
    let Some(base) = std::env::var("INNERTUBE_DUMP").ok().and_then(|v| v.strip_prefix("raw:").map(str::to_string)) else {
        return;
    };
    let dir = std::path::Path::new(&base).join("raw").join("pot").join(format!(
        "refused-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
    ));
    let (challenge, interpreter, yt_config) = capture;
    let files = [
        ("challenge.json", challenge.as_str()),
        ("interpreter.js", interpreter.as_str()),
        ("yt_config.json", yt_config.as_str()),
        ("snapshot.txt", snapshot),
        ("generate_it_response.txt", response),
    ];
    let saved = std::fs::create_dir_all(&dir).is_ok()
        && files.iter().all(|(name, body)| std::fs::write(dir.join(name), body).is_ok());
    tracing::warn!("[pot] refused attestation {} {}", if saved { "saved to" } else { "could not be saved to" }, dir.display());
}

impl NativeProvider {
    async fn mint(&self, content_binding: &str, attest: Option<&PageAttestation>) -> Result<String, PotError> {
        let mut state = self.state.lock().await;
        let live = state.expires.is_some_and(|t| Instant::now() < t);
        if !live {
            let attest = attest.ok_or_else(|| {
                PotError::Native("no attestation yet and no page challenge to attest with".into())
            })?;
            self.attest(&mut state, attest).await?;
        }
        let id = content_binding.to_string();
        let r = self.blocking(move |bg| bg.mint(id)).await;
        let shown: String = content_binding.chars().take(16).collect();
        match &r {
            Ok(t) => tracing::debug!("[pot] native mint {shown}: {} chars", t.len()),
            Err(e) => tracing::warn!("[pot] native mint {shown} failed: {e}"),
        }
        r
    }
}

impl PoTokenProvider for NativeProvider {
    fn get_po_token<'a>(
        &'a self,
        content_binding: &'a str,
        attest: Option<&'a PageAttestation>,
    ) -> PotFuture<'a, Result<String, PotError>> {
        Box::pin(self.mint(content_binding, attest))
    }

    fn invalidate_attestation(&self) -> PotFuture<'_, ()> {
        Box::pin(async move {
            self.state.lock().await.expires = None;
        })
    }
}
