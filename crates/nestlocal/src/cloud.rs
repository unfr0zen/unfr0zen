//! The parts of Nest's service nlclient talks to, served on the unit
//! itself. cloudregisterurl (client.config) points at /entry here; its
//! answer points every other service back at this server.
//!
//!   POST /entry                  service discovery
//!   POST /transport/v?/subscribe  long poll: the device's bucket revisions
//!                                 in, newer buckets out
//!   POST /transport/v?/put        the device's changes
//!   GET  /passphrase, /weather/*, /pro_info/*, POST /upload
//!
//! Buckets are the cloud's object store: device.<serial>, shared.<serial>,
//! structure.<id>, schedule.<serial>, ... nlclient keeps the truth; the
//! server keeps a copy, and pushes a bucket back only when Home Assistant
//! changes it.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body::{Frame, SizeHint};
use http_body_util::BodyExt;
use hyper::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode, Uri};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::json::{decode_object, Fields};
use crate::liveness::{InFlight, Liveness};
use crate::logf;
use crate::store::{now_ms, Object, Store};
use crate::weather::Outdoor;

pub struct Cloud {
    pub store: Arc<Store>,
    pub outdoor: Arc<Outdoor>,
    pub live: Option<Arc<Liveness>>,
    pub origin: String, // https://127.0.0.1
    pub verbose: bool,
    pub hold: Duration, // how long a subscribe with nothing to send is held
}

/// An object as the device expects it: revision and timestamp ahead of
/// object_key (NoLongerEvil found the firmware can drop them otherwise),
/// then the value, left out when empty.
#[derive(Serialize)]
struct Wire<'a> {
    object_revision: i64,
    object_timestamp: i64,
    object_key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<&'a Fields>,
}

fn to_wire(o: &Object, with_value: bool) -> Wire<'_> {
    let value = (with_value && !o.value.is_empty()).then_some(&o.value);
    Wire { object_revision: o.revision, object_timestamp: o.timestamp, object_key: &o.key, value }
}

/// {"objects": [...]}, or {"objects": null} for none, as Go wrote a nil
/// slice.
fn objects_json(objects: &[(Object, bool)]) -> Vec<u8> {
    #[derive(Serialize)]
    struct Objects<'a> {
        objects: Option<Vec<Wire<'a>>>,
    }
    let wires = (!objects.is_empty()).then(|| objects.iter().map(|(o, v)| to_wire(o, *v)).collect());
    serde_json::to_vec(&Objects { objects: wires }).unwrap_or_default()
}

/// A response body: whole, or a held subscribe's, which comes later. It
/// carries the request's place in Liveness until it has been sent.
pub struct Body {
    full: Option<Bytes>,
    held: Option<mpsc::Receiver<Bytes>>,
    live: Option<InFlight>,
}

impl http_body::Body for Body {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        if let Some(b) = self.full.take() {
            return Poll::Ready(Some(Ok(Frame::data(b))));
        }
        match &mut self.held {
            Some(rx) => rx.poll_recv(cx).map(|b| b.map(|b| Ok(Frame::data(b)))),
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.full.is_none() && self.held.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        match (&self.full, &self.held) {
            (Some(b), None) => SizeHint::with_exact(b.len() as u64),
            (None, None) => SizeHint::with_exact(0),
            _ => SizeHint::default(),
        }
    }
}

fn empty(status: StatusCode) -> Response<Body> {
    let mut r = Response::new(Body { full: None, held: None, live: None });
    *r.status_mut() = status;
    r
}

/// The Basic auth user (it names the device), not its password.
fn redact_auth(v: &str) -> String {
    match base64_decode(v.trim_start_matches("Basic ")) {
        Some(b) => format!("Basic {}:<redacted>", String::from_utf8_lossy(&b).split(':').next().unwrap_or("")),
        None => "(unparsed)".into(),
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.chunks(4) {
        let v: Vec<u8> = chunk.iter().map(|&c| value(c)).collect::<Option<_>>()?;
        let n = v.iter().enumerate().fold(0u32, |n, (i, &b)| n | (u32::from(b) << (18 - 6 * i)));
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    Some(out)
}

/// url.ParseQuery's Get: the first value of name in a query string.
fn query_value(query: Option<&str>, name: &str) -> String {
    let decode = |s: &str| {
        let s = s.replace('+', " ");
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 2 < b.len() {
                if let Some(byte) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    query.unwrap_or("").split('&').filter_map(|pair| pair.split_once('=').or(Some((pair, ""))))
        .find(|(k, _)| decode(k) == name).map(|(_, v)| decode(v)).unwrap_or_default()
}

/// The objects of a subscribe or put body: an "objects" array, or buckets
/// as named members carrying object_key.
fn objects_of(body: &Fields) -> Vec<&Fields> {
    if let Some(Value::Array(objects)) = body.get("objects") {
        return objects.iter().filter_map(Value::as_object).collect();
    }
    body.values().filter_map(Value::as_object).filter(|m| m.get("object_key").is_some_and(|k| !k.is_null())).collect()
}

fn int(v: Option<&Value>) -> i64 {
    v.and_then(Value::as_f64).map_or(0, |f| f as i64)
}

impl Cloud {
    /// Serves nlclient over HTTPS on listen, for good; the error is why it
    /// stopped.
    pub async fn serve(self: Arc<Self>, listen: &str, tls: Arc<rustls::ServerConfig>) -> std::io::Error {
        logf!("nestlocal: serving {listen}");
        let listener = match tokio::net::TcpListener::bind(listen).await {
            Ok(l) => l,
            Err(e) => return e,
        };
        let acceptor = tokio_rustls::TlsAcceptor::from(tls);
        loop {
            let (tcp, peer) = match listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    logf!("http: accept: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let (acceptor, cloud) = (acceptor.clone(), self.clone());
            tokio::spawn(async move {
                let stream = match acceptor.accept(tcp).await {
                    Ok(s) => s,
                    Err(e) => return logf!("http: TLS handshake error from {peer}: {e}"),
                };
                let service = hyper::service::service_fn(move |req: Request<hyper::body::Incoming>| {
                    let cloud = cloud.clone();
                    async move { Ok::<_, Infallible>(cloud.handle(req).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .title_case_headers(true)
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
            });
        }
    }

    async fn handle(self: Arc<Self>, req: Request<hyper::body::Incoming>) -> Response<Body> {
        let live = self.live.as_ref().map(|l| l.request());
        let (parts, incoming) = req.into_parts();
        let body = incoming.collect().await.map(|c| c.to_bytes()).unwrap_or_default();
        let mut response = self.respond(&parts.method, &parts.uri, &parts.headers, &body).await;
        response.body_mut().live = live;
        response
    }

    pub async fn respond(&self, method: &Method, uri: &Uri, headers: &HeaderMap, body: &[u8]) -> Response<Body> {
        if self.verbose {
            let shown = if uri.path() == "/upload" { format!("({} bytes)", body.len()) } // compressed logs
                        else { String::from_utf8_lossy(body).into_owned() };
            let headers: Vec<String> = headers.iter().map(|(k, v)| {
                let v = v.to_str().unwrap_or("");
                format!("{k}: {}", if k == "authorization" { redact_auth(v) } else { v.to_string() })
            }).collect();
            logf!("> {method} {uri}\n  {}\n  {shown}", headers.join("\n  "));
        }
        let path = uri.path();
        match path {
            "/entry" => self.entry(),
            "/passphrase" => self.reply(&json!({"value": "LOCAL00", "expires": now_ms() + 3_600_000})),
            "/upload" => self.reply(&json!({"status": "ok"})), // logs and diagnostics: dropped
            _ if path.starts_with("/transport/") => self.transport(path, body),
            _ if path.starts_with("/weather/") => self.weather(uri.query()),
            _ if path.starts_with("/pro_info/") => empty(StatusCode::NOT_FOUND),
            _ => {
                logf!("cloud: unhandled {method} {uri}");
                empty(StatusCode::NOT_FOUND)
            }
        }
    }

    fn headers(&self, response: &mut Response<Body>) {
        let h = response.headers_mut();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert("x-nl-service-timestamp", HeaderValue::from(now_ms()));
        h.insert("x-nl-suspend-time-max", HeaderValue::from(self.hold.as_secs() + 10));
    }

    fn reply_bytes(&self, bytes: Vec<u8>) -> Response<Body> {
        if self.verbose {
            logf!("< {}", String::from_utf8_lossy(&bytes));
        }
        let mut r = Response::new(Body { full: Some(Bytes::from(bytes)), held: None, live: None });
        self.headers(&mut r);
        r
    }

    fn reply(&self, v: &Value) -> Response<Body> {
        self.reply_bytes(v.to_string().into_bytes())
    }

    fn entry(&self) -> Response<Body> {
        let o = &self.origin;
        self.reply(&json!({
            "czfe_url": format!("{o}/transport"),
            "transport_url": format!("{o}/transport"),
            "direct_transport_url": format!("{o}/transport"),
            "passphrase_url": format!("{o}/passphrase"),
            "ping_url": format!("{o}/transport"),
            "pro_info_url": format!("{o}/pro_info"),
            "weather_url": format!("{o}/weather/v1?query="),
            "upload_url": format!("{o}/upload"),
            "software_update_url": "",
            "server_version": "nestlocal",
            "tier_name": "local",
        }))
    }

    fn transport(&self, path: &str, body: &[u8]) -> Response<Body> {
        if path.ends_with("/subscribe") {
            self.subscribe(body)
        } else if path.ends_with("/put") {
            self.put(body)
        } else {
            // GET transport/device/<serial>: what the server holds.
            let all: Vec<_> = self.store.all().into_iter().map(|o| (o, false)).collect();
            self.reply_bytes(objects_json(&all))
        }
    }

    fn bad_json() -> Response<Body> {
        let mut r = Response::new(Body { full: Some(Bytes::from_static(b"bad json\n")), held: None, live: None });
        *r.status_mut() = StatusCode::BAD_REQUEST;
        r.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
        r.headers_mut().insert("x-content-type-options", HeaderValue::from_static("nosniff"));
        r
    }

    fn subscribe(&self, body: &[u8]) -> Response<Body> {
        let Some(body) = decode_object(body) else {
            return Self::bad_json();
        };
        let chunked = body.get("chunked").and_then(Value::as_bool).unwrap_or(false);
        let mut keys = BTreeSet::new();
        let mut out = Vec::new();
        for m in objects_of(&body) {
            let Some(key) = m.get("object_key").and_then(Value::as_str).filter(|k| !k.is_empty()) else {
                continue;
            };
            keys.insert(key.to_string());
            let (rev, ts) = (int(m.get("object_revision")), int(m.get("object_timestamp")));
            if let Some(Value::Object(v)) = m.get("value") {
                // A bucket the device sends whole: take it as it stands,
                // unless ours is newer (Home Assistant changed it since).
                if !v.is_empty() && self.store.get(key).is_none_or(|srv| srv.timestamp <= ts) {
                    self.store.merge(key, v, rev, ts, true);
                }
            }
            match self.store.get(key) {
                Some(srv) if srv.timestamp > 0 && (ts == 0 || srv.timestamp > ts) => out.push((srv, true)), // ours is newer
                // The device is ahead, or we never saw this bucket (a fresh
                // server): ask for it, by answering with what we have. A
                // bucket we lack goes back empty, at revision/timestamp 0.
                Some(srv) if srv.timestamp < ts => out.push((srv, false)),
                None => out.push((Object { key: key.to_string(), ..Default::default() }, false)),
                Some(_) => {}
            }
        }
        if !out.is_empty() || !chunked {
            return self.reply_bytes(objects_json(&out));
        }

        // Nothing to say: hold the connection until Home Assistant changes a
        // bucket the device watches, or the hold runs out (closing it is
        // the device's cue to subscribe again).
        let (mut changes, subscription) = self.store.subscribe();
        let (tx, rx) = mpsc::channel(1);
        let (hold, verbose) = (self.hold, self.verbose);
        tokio::spawn(async move {
            let _subscription = subscription;
            let deadline = tokio::time::sleep(hold);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    Some(objects) = changes.recv() => {
                        let push: Vec<_> = objects.into_iter().filter(|o| keys.contains(&o.key)).map(|o| (o, true)).collect();
                        if push.is_empty() {
                            continue;
                        }
                        let bytes = objects_json(&push);
                        if verbose {
                            logf!("< (push) {}", String::from_utf8_lossy(&bytes));
                        }
                        let _ = tx.send(Bytes::from(bytes)).await;
                        return;
                    }
                    _ = &mut deadline => return,
                    _ = tx.closed() => return, // the device went away
                }
            }
        });
        let mut r = Response::new(Body { full: None, held: Some(rx), live: None });
        self.headers(&mut r);
        r
    }

    fn put(&self, body: &[u8]) -> Response<Body> {
        let Some(body) = decode_object(body) else {
            return Self::bad_json();
        };
        let mut out = Vec::new();
        for m in objects_of(&body) {
            let Some(key) = m.get("object_key").and_then(Value::as_str).filter(|k| !k.is_empty()) else {
                continue;
            };
            let fields = match m.get("value") {
                Some(Value::Object(v)) => v.clone(),
                // Bucket-keyed form: the fields sit beside object_key.
                _ => m.iter().filter(|(k, _)| !matches!(k.as_str(), "object_key" | "object_revision" | "object_timestamp"
                                                            | "base_object_revision" | "if_object_revision" | "op"))
                    .map(|(k, v)| (k.clone(), v.clone())).collect(),
            };
            let (o, _) = self.store.merge(key, &fields, 0, 0, true);
            out.push((o, false));
        }
        self.reply_bytes(objects_json(&out))
    }

    /// Answers GET /weather/v1?query=..., which nlclient polls every
    /// weatherpollinterval (3900 s) and at start (weather.rs has what its
    /// nlWeatherParser reads). The cloud keyed the answer by the query:
    /// <postal code>,<country>, or ipv4 (nlgeoipfetch's "%sipv4": the
    /// weather where the unit's address is), whose answer goes through the
    /// same parser. With no outdoor reading, an empty answer: the unit then
    /// shows no outside temperature.
    fn weather(&self, query: Option<&str>) -> Response<Body> {
        match self.outdoor.answer() {
            None => self.reply(&json!({})),
            Some(a) => {
                let mut m = Fields::new();
                m.insert(query_value(query, "query"), Value::Object(a));
                self.reply(&Value::Object(m))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields;
    use crate::store::tests::temp_store;

    fn cloud() -> Cloud {
        Cloud { store: temp_store(), outdoor: Arc::new(Outdoor::default()), live: None,
                origin: "https://127.0.0.1".into(), verbose: false, hold: Duration::from_secs(290) }
    }

    async fn call(c: &Cloud, path: &str, body: &str) -> (StatusCode, HeaderMap, String) {
        let r = c.respond(&Method::POST, &path.parse().unwrap(), &HeaderMap::new(), body.as_bytes()).await;
        let (parts, body) = r.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        (parts.status, parts.headers, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn weather_by_query() {
        let c = cloud();
        c.outdoor.set(Some(-3.4), None);
        assert_eq!(call(&c, "/weather/v1?query=94304%2CUS", "").await.2, "{}");
        c.outdoor.set(None, Some(81.6));
        assert_eq!(call(&c, "/weather/v1?query=94304%2CUS", "").await.2,
                   r#"{"94304,US":{"current":{"humidity":82,"temp_c":-3.4}}}"#);
        // nlgeoipfetch's query: weather too.
        assert!(call(&c, "/weather/v1?query=ipv4", "").await.2.starts_with(r#"{"ipv4":"#));
    }

    #[tokio::test]
    async fn subscribe_and_put() {
        let c = cloud();
        // A fresh server asks for a bucket it lacks, at revision 0.
        let (status, headers, body) = call(&c, "/transport/v7/subscribe",
            r#"{"chunked":true,"objects":[{"object_key":"device.S","object_revision":5,"object_timestamp":100}]}"#).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, r#"{"objects":[{"object_revision":0,"object_timestamp":0,"object_key":"device.S"}]}"#);
        assert_eq!(headers["x-nl-suspend-time-max"], "300");
        // The device puts it; the answer carries the new revision, no value.
        let (_, _, body) = call(&c, "/transport/v7/put",
            r#"{"objects":[{"object_key":"device.S","op":"MERGE","value":{"t":20.0}}]}"#).await;
        assert!(body.starts_with(r#"{"objects":[{"object_revision":1,"object_timestamp":"#) && !body.contains("value"), "{body}");
        assert_eq!(c.store.get("device.S").unwrap().value["t"], serde_json::json!(20));
        // Nothing to say and not chunked: null objects.
        let ts = c.store.get("device.S").unwrap().timestamp;
        let (_, _, body) = call(&c, "/transport/v7/subscribe",
            &format!(r#"{{"objects":[{{"object_key":"device.S","object_revision":1,"object_timestamp":{ts}}}]}}"#)).await;
        assert_eq!(body, r#"{"objects":null}"#);
        assert_eq!(call(&c, "/transport/v7/put", "nope").await.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn held_subscribe_gets_a_server_change() {
        let c = cloud();
        c.store.merge("device.S", &fields!{"t" => 20}, 1, 100, true);
        let r = c.respond(&Method::POST, &"/transport/v7/subscribe".parse().unwrap(), &HeaderMap::new(),
            br#"{"chunked":true,"objects":[{"object_key":"device.S","object_revision":1,"object_timestamp":100}]}"#).await;
        let store = c.store.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            store.merge("shared.S", &fields!{"x" => 1}, 0, 0, false); // not watched: ignored
            store.merge("device.S", &fields!{"t" => 21}, 0, 0, false);
        });
        let body = r.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains(r#""object_key":"device.S","value":{"t":21}"#), "{body}");
    }

    #[test]
    fn queries_and_auth() {
        assert_eq!(query_value(Some("query=94304%2CUS"), "query"), "94304,US");
        assert_eq!(query_value(Some("a=1&query=ipv4"), "query"), "ipv4");
        assert_eq!(query_value(None, "query"), "");
        // A % that starts no escape stays as it is, whatever follows it.
        assert_eq!(query_value(Some("query=%a\u{e9}"), "query"), "%a\u{e9}");
        assert_eq!(query_value(Some("query=%\u{e9}x&b=%"), "query"), "%\u{e9}x");
        assert_eq!(query_value(Some("query=%zz%4"), "query"), "%zz%4");
        assert_eq!(redact_auth("Basic ZGV2aWNlOnBhc3M="), "Basic device:<redacted>"); // device:pass
        assert_eq!(redact_auth("Basic !!"), "(unparsed)");
    }

    #[tokio::test]
    async fn subscribe_answers_by_whose_copy_is_newer() {
        let c = cloud();
        for key in ["device.S", "shared.S", "schedule.S", "link.S"] {
            c.store.merge(key, &fields!{"k" => key}, 2, 500, true);
        }
        let (status, _, body) = call(&c, "/transport/v7/subscribe", r#"{"objects":[
            {"object_key":"device.S","object_revision":1,"object_timestamp":400},
            {"object_key":"shared.S","object_revision":3,"object_timestamp":900},
            {"object_key":"schedule.S","object_revision":2,"object_timestamp":500},
            {"object_key":"link.S"},
            {"object_key":"","object_timestamp":1},
            {"object_revision":1},
            "not an object"]}"#).await;
        assert_eq!(status, StatusCode::OK);
        // Ours newer: sent with its value. The device's newer: ours without
        // a value, which asks for its copy. Equal: nothing. No timestamp from
        // the device: ours.
        assert_eq!(body, concat!(r#"{"objects":["#,
            r#"{"object_revision":2,"object_timestamp":500,"object_key":"device.S","value":{"k":"device.S"}},"#,
            r#"{"object_revision":2,"object_timestamp":500,"object_key":"shared.S"},"#,
            r#"{"object_revision":2,"object_timestamp":500,"object_key":"link.S","value":{"k":"link.S"}}]}"#));
    }

    #[tokio::test]
    async fn subscribe_keeps_a_newer_server_change_over_the_devices_copy() {
        let c = cloud();
        c.store.merge("shared.S", &fields!{"target_temperature" => 20}, 3, 1000, true);
        c.store.merge("shared.S", &fields!{"target_temperature" => 22}, 0, 0, false); // Home Assistant
        let ours = c.store.get("shared.S").unwrap();
        assert_eq!(ours.revision, 4);
        let (_, _, body) = call(&c, "/transport/v7/subscribe",
            r#"{"objects":[{"object_key":"shared.S","object_revision":3,"object_timestamp":1000,
                "value":{"target_temperature":20}}]}"#).await;
        let o = c.store.get("shared.S").unwrap();
        assert_eq!((o.revision, o.timestamp, o.value["target_temperature"].clone()), (4, ours.timestamp, json!(22)));
        assert_eq!(body, format!(r#"{{"objects":[{{"object_revision":4,"object_timestamp":{},"object_key":"shared.S","value":{{"target_temperature":22}}}}]}}"#,
                                 ours.timestamp));
        // A newer copy from the device with an older revision: taken, and the
        // revision still moves on.
        let ts = ours.timestamp + 1;
        call(&c, "/transport/v7/subscribe", &format!(r#"{{"objects":[{{"object_key":"shared.S","object_revision":2,
            "object_timestamp":{ts},"value":{{"target_temperature":23}}}}]}}"#)).await;
        let o = c.store.get("shared.S").unwrap();
        assert_eq!((o.revision, o.timestamp, o.value["target_temperature"].clone()), (5, ts, json!(23)));
    }

    #[tokio::test]
    async fn subscribe_takes_a_bucket_the_device_sends_whole() {
        let c = cloud();
        // Bucket-keyed form, with the device's own revision and timestamp.
        let (_, _, body) = call(&c, "/transport/v7/subscribe",
            r#"{"device.S":{"object_key":"device.S","object_revision":9,"object_timestamp":777,"value":{"t":20.0}},
                "session":"x"}"#).await;
        assert_eq!(body, r#"{"objects":null}"#, "the device's copy is the server's now: nothing to send back");
        let o = c.store.get("device.S").unwrap();
        assert_eq!((o.revision, o.timestamp, o.value["t"].clone()), (9, 777, json!(20)));
        // An empty value is not a bucket sent whole: a fresh server asks.
        let (_, _, body) = call(&c, "/transport/v7/subscribe",
            r#"{"objects":[{"object_key":"shared.S","object_revision":1,"object_timestamp":5,"value":{}}]}"#).await;
        assert_eq!(body, r#"{"objects":[{"object_revision":0,"object_timestamp":0,"object_key":"shared.S"}]}"#);
        assert!(c.store.get("shared.S").is_none());
    }

    #[tokio::test]
    async fn held_subscribe_ends_empty_when_the_hold_runs_out() {
        let mut c = cloud();
        c.hold = Duration::from_millis(50);
        c.store.merge("device.S", &fields!{"t" => 20}, 1, 100, true);
        let r = c.respond(&Method::POST, &"/transport/v7/subscribe".parse().unwrap(), &HeaderMap::new(),
            br#"{"chunked":true,"objects":[{"object_key":"device.S","object_revision":1,"object_timestamp":100}]}"#).await;
        assert_eq!(r.headers()["x-nl-suspend-time-max"], "10");
        // The device's own change is not pushed back to it.
        c.store.merge("device.S", &fields!{"t" => 21}, 0, 0, true);
        let started = std::time::Instant::now();
        let body = r.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty(), "{body:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn put_in_bucket_keyed_form_keeps_only_the_fields() {
        let c = cloud();
        let (_, _, body) = call(&c, "/transport/v7/put",
            r#"{"shared.S":{"object_key":"shared.S","op":"MERGE","base_object_revision":4,"if_object_revision":4,
                "target_temperature":21.5}}"#).await;
        assert!(body.starts_with(r#"{"objects":[{"object_revision":1,"#), "{body}");
        let o = c.store.get("shared.S").unwrap();
        assert_eq!(Value::Object(o.value.clone()), json!({"target_temperature": 21.5}));
        // The same put again changes nothing: same revision and timestamp.
        let (_, _, again) = call(&c, "/transport/v7/put",
            r#"{"objects":[{"object_key":"shared.S","value":{"target_temperature":21.5}}]}"#).await;
        assert_eq!(again, format!(r#"{{"objects":[{{"object_revision":1,"object_timestamp":{},"object_key":"shared.S"}}]}}"#, o.timestamp));
        // The bucket's revision and timestamp are no fields of it.
        call(&c, "/transport/v7/put", r#"{"shared.S":{"object_key":"shared.S","object_revision":7,"object_timestamp":1234,
            "target_temperature":22}}"#).await;
        assert_eq!(Value::Object(c.store.get("shared.S").unwrap().value), json!({"target_temperature": 22}));
        // A put naming no bucket answers with none.
        assert_eq!(call(&c, "/transport/v7/put", r#"{"objects":[{"value":{"a":1}}]}"#).await.2, r#"{"objects":null}"#);
    }

    #[tokio::test]
    async fn malformed_transport_bodies() {
        let c = cloud();
        for body in ["", "nope", "[1]", "\"objects\""] {
            let (status, headers, text) = call(&c, "/transport/v7/subscribe", body).await;
            assert_eq!((status, text.as_str()), (StatusCode::BAD_REQUEST, "bad json\n"), "{body:?}");
            assert_eq!(headers[CONTENT_TYPE], "text/plain; charset=utf-8");
        }
        // objects that is no list: nothing to answer, and nothing stored.
        assert_eq!(call(&c, "/transport/v7/subscribe", r#"{"objects":"x"}"#).await.2, r#"{"objects":null}"#);
        assert!(c.store.all().is_empty());
    }

    #[tokio::test]
    async fn entry_points_every_service_back_here() {
        let c = cloud();
        let (status, headers, body) = call(&c, "/entry", "").await;
        assert_eq!((status, &headers[CONTENT_TYPE]), (StatusCode::OK, &HeaderValue::from_static("application/json")));
        let entry: Value = serde_json::from_str(&body).unwrap();
        for k in ["czfe_url", "transport_url", "direct_transport_url", "ping_url"] {
            assert_eq!(entry[k], json!("https://127.0.0.1/transport"), "{k}");
        }
        assert_eq!(entry["weather_url"], json!("https://127.0.0.1/weather/v1?query="));
        assert_eq!(entry["software_update_url"], json!(""), "no firmware updates from here");
        assert_eq!(call(&c, "/pro_info/x", "").await.0, StatusCode::NOT_FOUND);
        assert_eq!(call(&c, "/elsewhere", "").await.0, StatusCode::NOT_FOUND);
        // GET transport/device/<serial>: every bucket, without values.
        c.store.merge("device.S", &fields!{"t" => 20}, 1, 100, true);
        let (_, _, body) = call(&c, "/transport/device/S", "").await;
        assert_eq!(body, r#"{"objects":[{"object_revision":1,"object_timestamp":100,"object_key":"device.S"}]}"#);
    }
}
