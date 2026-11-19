//! Updates over Matter: the OTA Software Update Requestor cluster on the
//! root endpoint. A controller's OTA provider (Home Assistant's Matter
//! server, say) is written to DefaultOTAProviders or announces itself; the
//! unit asks it for an image newer than the unfr0zen it runs (Basic
//! Information's SoftwareVersion, update::matter_version), downloads it over
//! BDX and, once the provider says to apply it, gives this unit's bundle to
//! the update engine (update.rs), which checks it against its manifest and
//! the release key, then restarts. After the restart it tells the provider
//! the update was applied.
//!
//! The image is the release's package for this unit's device (package.rs),
//! the one MQTT's updates install too: each generation is a product of its
//! own, so the provider offers it its own device's.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use embassy_futures::select::{select, Either};
use rs_matter::bdx::BdxDownloadInitiator;
use rs_matter::crypto::Crypto;
use rs_matter::dm::clusters::ota_prov::{DownloadProtocolEnum, OtaApplyOutcome, StatusEnum};
use rs_matter::dm::clusters::ota_req::{parse_bdx_url, OtaState, Provider, Providers};
use rs_matter::dm::AttrChangeNotifier;
use rs_matter::error::{Error, ErrorCode};
use rs_matter::transport::exchange::Exchange;
use rs_matter::Matter;
use serde::{Deserialize, Serialize};

use crate::logf;
use crate::package::{Expected, Unwrap};
use crate::update::{self, Updates};

/// The endpoint the requestor is on.
pub const ENDPOINT: u16 = 0;
const PROTOCOLS: &[DownloadProtocolEnum] = &[DownloadProtocolEnum::BDXSynchronous];
/// The default providers are asked this long after a start, then this often.
const FIRST_QUERY: Duration = Duration::from_secs(60);
const QUERY_EVERY: Duration = Duration::from_secs(24 * 3600);
/// How many times a busy provider, or one that says to wait before applying,
/// is asked again; and the bounds of the waits it asks for.
const RETRIES: usize = 5;
const WAIT_MIN: Duration = Duration::from_secs(10);
const WAIT_MAX: Duration = Duration::from_secs(3600);
/// Where an applied update's provider and token wait for the restart.
const APPLIED: &str = "ota-applied.json";

static BUSY: AtomicBool = AtomicBool::new(false);

/// Whether an update is under way, which keeps the unit awake.
pub fn busy() -> bool {
    BUSY.load(Ordering::SeqCst)
}

/// The cluster's state, shared by its handler and the update loop.
pub struct Ota {
    pub providers: Providers,
    pub state: OtaState,
}

impl Ota {
    pub const fn new() -> Self {
        Ota { providers: Providers::new(), state: OtaState::new(ENDPOINT) }
    }
}

/// An update applied, kept across the restart to report it.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Applied {
    fabric: u8,
    node: u64,
    endpoint: u16,
    token: Vec<u8>,
    version: u32,
}

impl Applied {
    fn provider(&self) -> Option<Provider> {
        Some(Provider { fab_idx: std::num::NonZeroU8::new(self.fabric)?, node_id: self.node, endpoint: self.endpoint })
    }
}

/// Reports an update applied before this start, then asks the default
/// providers for updates now and then, and any provider that announces
/// itself at once.
pub async fn run<C: Crypto + Copy>(
    matter: &Matter<'_>,
    crypto: C,
    ota: &Ota,
    notifier: &dyn AttrChangeNotifier,
    updates: Option<Arc<Updates>>,
    storage: &Path,
) -> Result<(), Error> {
    let updates = updates.filter(|u| u.can_install());
    ota.state.set_update_possible(notifier, updates.is_some());
    let applied = storage.join(APPLIED);
    if let Some(done) = std::fs::read(&applied).ok().and_then(|data| serde_json::from_slice::<Applied>(&data).ok()) {
        let _ = std::fs::remove_file(&applied);
        report_applied(matter, crypto, &done).await;
    }
    let Some(updates) = updates else {
        // Taken, and refused, as UpdatePossible says.
        loop {
            ota.providers.wait_changed().await;
            ota.providers.clear_announced();
        }
    };
    let mut wait = FIRST_QUERY;
    loop {
        let defaults = match select(ota.providers.wait_changed(), async_io::Timer::after(wait)).await {
            Either::First(()) => false,
            Either::Second(_) => true,
        };
        wait = QUERY_EVERY;
        let announced = ota.providers.take_announced();
        let mut providers: Vec<Provider> = announced.iter().copied().collect();
        if defaults {
            providers.extend((0..ota.providers.len()).filter_map(|i| ota.providers.get(i)));
        }
        for provider in providers {
            BUSY.store(true, Ordering::SeqCst);
            let result = update_from(matter, crypto, ota, notifier, &updates, &provider, &applied).await;
            BUSY.store(false, Ordering::SeqCst);
            match result {
                Ok(true) => {
                    // Restarting into it.
                    loop {
                        async_io::Timer::after(WAIT_MAX).await;
                    }
                }
                Ok(false) => {}
                Err(e) => logf!("matter OTA: provider {:016X}: {e}", provider.node_id),
            }
        }
    }
}

/// Tells the provider of an update applied before the restart that the unit
/// now runs it, if it does.
async fn report_applied<C: Crypto + Copy>(matter: &Matter<'_>, crypto: C, done: &Applied) {
    let running = matter.dev_det().sw_ver;
    if running != done.version {
        return logf!("matter OTA: the update to {:#x} did not take: still {running:#x}", done.version);
    }
    let Some(provider) = done.provider() else { return };
    // The network, and the provider's address, may take a while after a start.
    for _ in 0..RETRIES {
        async_io::Timer::after(Duration::from_secs(30)).await;
        match provider.notify_applied(matter, crypto, &done.token, done.version).await {
            Ok(()) => return logf!("matter OTA: told provider {:016X} the update applied", done.node),
            Err(e) => logf!("matter OTA: telling provider {:016X} the update applied: {e:?}", done.node),
        }
    }
}

/// What a provider offers.
struct Offer {
    version: u32,
    version_string: String,
    uri: String,
    token: Vec<u8>,
}

/// Asks the provider for an update and installs it: whether the unit is
/// restarting into it.
async fn update_from<C: Crypto + Copy>(
    matter: &Matter<'_>,
    crypto: C,
    ota: &Ota,
    notifier: &dyn AttrChangeNotifier,
    updates: &Arc<Updates>,
    provider: &Provider,
    applied: &Path,
) -> Result<bool, String> {
    let session = ota.state.initiate_update(notifier);
    session.querying();
    let Some(offer) = query(matter, crypto, provider).await? else {
        return Ok(false);
    };
    if !updates.claim() {
        logf!("matter OTA: {} offered, but an update is already being installed", offer.version_string);
        return Ok(false);
    }
    let result = install(matter, crypto, &session, updates, provider, &offer, applied).await;
    match result {
        Ok(true) => {
            // Applying until the restart.
            std::mem::forget(session);
            Ok(true)
        }
        other => {
            updates.release();
            other
        }
    }
}

/// The provider's offer of an image newer than the unit runs, asking again
/// while it is busy.
async fn query<C: Crypto + Copy>(matter: &Matter<'_>, crypto: C, provider: &Provider) -> Result<Option<Offer>, String> {
    let running = matter.dev_det().sw_ver;
    for _ in 0..RETRIES {
        let answer = provider
            .query(matter, crypto, PROTOCOLS, None, false, |response| {
                let status = response.status()?;
                if status == StatusEnum::Busy {
                    return Ok(Err(response.delayed_action_time()?.unwrap_or(0)));
                }
                if status != StatusEnum::UpdateAvailable {
                    return Ok(Ok(None));
                }
                let version = response.software_version()?.ok_or(ErrorCode::InvalidData)?;
                Ok(Ok(Some(Offer {
                    version,
                    version_string: response.software_version_string()?.unwrap_or_default().to_string(),
                    uri: response.image_uri()?.ok_or(ErrorCode::InvalidData)?.to_string(),
                    token: response.update_token()?.ok_or(ErrorCode::InvalidData)?.0.to_vec(),
                })))
            })
            .await
            .map_err(|e| format!("query: {e:?}"))?;
        match answer {
            Ok(Some(offer)) if offer.version > running => return Ok(Some(offer)),
            Ok(Some(offer)) => {
                logf!("matter OTA: offered {}, which is not newer than {running:#x}", offer.version_string);
                return Ok(None);
            }
            Ok(None) => return Ok(None),
            Err(delay) => {
                async_io::Timer::after(bounded(delay)).await;
            }
        }
    }
    Err("busy".into())
}

fn bounded(secs: u32) -> Duration {
    Duration::from_secs(secs.into()).clamp(WAIT_MIN, WAIT_MAX)
}

/// Downloads the offer's image, keeps this unit's bundle, and once the
/// provider says to, applies it: whether the unit is restarting into it.
async fn install<C: Crypto + Copy>(
    matter: &Matter<'_>,
    crypto: C,
    session: &rs_matter::dm::clusters::ota_req::OtaUpdate<'_>,
    updates: &Arc<Updates>,
    provider: &Provider,
    offer: &Offer,
    applied: &Path,
) -> Result<bool, String> {
    let (node, file) = parse_bdx_url(&offer.uri).map_err(|e| format!("{}: {e:?}", offer.uri))?;
    logf!("matter OTA: downloading {} ({:#x}) from {}", offer.version_string, offer.version, offer.uri);
    session.downloading(Some(0));
    let bundle = std::fs::File::create(update::DOWNLOAD).map_err(|e| format!("{}: {e}", update::DOWNLOAD))?;
    let expected = Expected { product: matter.dev_det().pid, version: Some(offer.version), bare: false };
    let mut image = Unwrap::new(expected, std::io::BufWriter::new(bundle));
    let exchange =
        Exchange::initiate(matter, crypto, provider.fab_idx, node).await.map_err(|e| format!("BDX: {e:?}"))?;
    let mut reader = exchange.download(file.as_bytes(), None).await.map_err(|e| format!("BDX: {e:?}"))?;
    let mut buf = vec![0; 2048];
    let mut shown = 0;
    loop {
        let n = reader.read(&mut buf).await.map_err(|e| format!("BDX: {e:?}"))?;
        if n == 0 {
            break;
        }
        image.feed(&buf[..n])?;
        let percent = image.percent();
        if percent != shown {
            shown = percent;
            session.downloading(Some(percent));
        }
    }
    image.finish()?;
    logf!("matter OTA: downloaded {}", offer.version_string);

    for _ in 0..RETRIES {
        let outcome = provider
            .apply_update(matter, crypto, &offer.token, offer.version)
            .await
            .map_err(|e| format!("apply update request: {e:?}"))?;
        match outcome {
            OtaApplyOutcome::Proceed { delay_secs } => {
                if delay_secs > 0 {
                    async_io::Timer::after(Duration::from_secs(delay_secs.into()).min(WAIT_MAX)).await;
                }
                return apply(session, updates, provider, offer, applied).await.map(|()| true);
            }
            OtaApplyOutcome::Await { delay_secs } => {
                async_io::Timer::after(bounded(delay_secs)).await;
            }
            OtaApplyOutcome::Discontinue => {
                logf!("matter OTA: the provider withdrew {}", offer.version_string);
                return Ok(false);
            }
        }
    }
    Err("the provider never said to apply the update".into())
}

/// Runs the engine on the downloaded bundle, off Matter's thread, keeping
/// the provider and token for the report after the restart.
async fn apply(
    session: &rs_matter::dm::clusters::ota_req::OtaUpdate<'_>,
    updates: &Arc<Updates>,
    provider: &Provider,
    offer: &Offer,
    applied: &Path,
) -> Result<(), String> {
    session.applying();
    let done = Applied {
        fabric: provider.fab_idx.get(),
        node: provider.node_id,
        endpoint: provider.endpoint,
        token: offer.token.clone(),
        version: offer.version,
    };
    std::fs::write(applied, serde_json::to_vec(&done).expect("serializes")).map_err(|e| format!("{e}"))?;
    let (sent, result) = async_channel::bounded(1);
    let updates = updates.clone();
    let what = format!("{} over Matter", offer.version_string);
    std::thread::spawn(move || {
        let _ = sent.send_blocking(updates.apply(&what));
    });
    let result = result.recv().await.unwrap_or_else(|_| Err("the engine's thread stopped".into()));
    if result.is_err() {
        let _ = std::fs::remove_file(applied);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_applied_update_survives_the_restart_as_json() {
        let done = Applied { fabric: 1, node: 0x1B669, endpoint: 0, token: vec![1, 2, 3], version: 0x0006_0080 };
        let back: Applied = serde_json::from_slice(&serde_json::to_vec(&done).unwrap()).unwrap();
        assert_eq!(back, done);
        assert_eq!(back.provider().unwrap().node_id, 0x1B669);
    }
}
