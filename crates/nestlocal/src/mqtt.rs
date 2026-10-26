//! A small MQTT 3.1.1 client: what the bridge needs and nothing more. One
//! connection with a retained will, QoS 0 and 1 publishes, QoS 1
//! subscriptions, a keepalive ping, and a reconnect every 10 s while the
//! broker cannot be reached. Publishes made while disconnected are dropped:
//! on each connection the bridge publishes its whole state again.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{Buf, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::logf;

/// The broker drops the connection after 1.5 times this without a packet.
/// The unit sleeps (wake.rs) and the ping timer stops with it, so the broker
/// must wait out a sleep; the unit wakes for every broker message, and
/// Bridge::woke sends something on a wake when this is half gone.
pub const KEEPALIVE: Duration = Duration::from_secs(20 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY: Duration = Duration::from_secs(10);

/// What a connection calls back into.
pub trait Handler: Send + Sync + 'static {
    /// Connected (again): subscribe and publish the state.
    fn connected(&self);
    /// A message on a subscribed topic.
    fn message(&self, topic: &str, payload: &[u8]);
}

/// A broker URL: tcp:// or mqtt:// in the clear, ssl://, tls:// or
/// mqtts:// over TLS.
#[derive(Clone, Debug, PartialEq)]
pub struct Broker {
    pub url: String,
    pub tls: bool,
    pub host: String,
    pub port: u16,
}

impl Broker {
    pub fn parse(url: &str) -> Result<Broker, String> {
        let (scheme, rest) = url.split_once("://").ok_or_else(|| format!("{url}: expected scheme://host:port"))?;
        let tls = match scheme {
            "tcp" | "mqtt" => false,
            "ssl" | "tls" | "mqtts" => true,
            _ => return Err(format!("{url}: unsupported scheme {scheme}")),
        };
        let rest = rest.trim_end_matches('/');
        let (host, port) = match rest.strip_prefix('[') {
            Some(v6) => {
                let (host, port) = v6.split_once("]:").ok_or_else(|| format!("{url}: missing port"))?;
                (host, port)
            }
            None => rest.rsplit_once(':').ok_or_else(|| format!("{url}: missing port"))?,
        };
        let port = port.parse().map_err(|_| format!("{url}: bad port {port}"))?;
        Ok(Broker { url: url.into(), tls, host: host.into(), port })
    }
}

pub struct Options {
    pub client_id: String,
    pub user: Option<String>,
    pub password: String,
    /// Topic and payload, sent retained at QoS 1.
    pub will: (String, Vec<u8>),
    pub tls: Option<Arc<rustls::ClientConfig>>,
}

enum Request {
    Publish { topic: String, payload: Vec<u8>, qos: u8, retain: bool },
    Subscribe { filter: String },
}

/// Publishes and subscribes through the connection's task.
#[derive(Clone)]
pub struct Client {
    tx: mpsc::Sender<Request>,
    connected: Arc<AtomicBool>,
    sent: Arc<AtomicU64>,
}

pub struct Connection {
    broker: Broker,
    options: Options,
    rx: mpsc::Receiver<Request>,
    connected: Arc<AtomicBool>,
    /// When the last packet went to the broker, in Unix seconds: the wall
    /// clock, which keeps counting while the unit sleeps.
    sent: Arc<AtomicU64>,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Client {
    pub fn new(broker: Broker, options: Options) -> (Client, Connection) {
        // Room for a whole announcement, some forty discovery configs.
        let (tx, rx) = mpsc::channel(512);
        let connected = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicU64::new(0));
        (Client { tx, connected: connected.clone(), sent: sent.clone() }, Connection { broker, options, rx, connected, sent })
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn publish(&self, topic: &str, payload: &[u8], qos: u8, retain: bool) {
        if self.is_connected() {
            let _ = self.tx.try_send(Request::Publish { topic: topic.into(), payload: payload.to_vec(), qos, retain });
        }
    }

    /// How long since the last packet to the broker, by the wall clock.
    pub fn idle(&self) -> Duration {
        Duration::from_secs(unix_now().saturating_sub(self.sent.load(Ordering::Relaxed)))
    }

    pub fn subscribe(&self, filter: &str) {
        let _ = self.tx.try_send(Request::Subscribe { filter: filter.into() });
    }
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

impl Connection {
    /// Keeps a connection to the broker, for good.
    pub async fn run(mut self, handler: Arc<impl Handler>) {
        let mut last_error = String::new();
        let mut logged = Instant::now();
        loop {
            if let Err(e) = self.session(&*handler).await {
                // Once per streak of the same failure, and every 10 minutes.
                let e = e.to_string();
                if e != last_error || logged.elapsed() >= Duration::from_secs(600) {
                    logf!("mqtt: {}: {e}", self.broker.url);
                    last_error = e;
                    logged = Instant::now();
                }
            }
            self.connected.store(false, Ordering::Relaxed);
            while self.rx.try_recv().is_ok() {}
            tokio::time::sleep(RETRY).await;
        }
    }

    async fn connect(&self) -> std::io::Result<Box<dyn Stream>> {
        if self.broker.tls && self.options.tls.is_none() {
            return Err(std::io::Error::other("TLS broker, but no TLS configuration"));
        }
        let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((self.broker.host.as_str(), self.broker.port)))
            .await.map_err(|_| std::io::Error::other("connect timed out"))??;
        tcp.set_nodelay(true)?;
        let Some(tls) = &self.options.tls else {
            return Ok(Box::new(tcp));
        };
        let name = rustls::pki_types::ServerName::try_from(self.broker.host.clone()).map_err(std::io::Error::other)?;
        Ok(Box::new(tokio_rustls::TlsConnector::from(tls.clone()).connect(name, tcp).await?))
    }

    async fn session(&mut self, handler: &impl Handler) -> std::io::Result<()> {
        let stream = self.connect().await?;
        let (mut rd, mut wr) = tokio::io::split(stream);
        wr.write_all(&connect_packet(&self.options)).await?;
        self.sent.store(unix_now(), Ordering::Relaxed);
        let mut buf = BytesMut::with_capacity(4096);
        let connack = tokio::time::timeout(CONNECT_TIMEOUT, next_packet(&mut rd, &mut buf))
            .await.map_err(|_| std::io::Error::other("no CONNACK"))??;
        match connack {
            (0x20, body) if body.len() >= 2 && body[1] == 0 => {}
            (0x20, body) if body.len() >= 2 => return Err(std::io::Error::other(refused(body[1]))),
            _ => return Err(std::io::Error::other("expected CONNACK")),
        }
        self.connected.store(true, Ordering::Relaxed);
        handler.connected();

        let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + KEEPALIVE, KEEPALIVE);
        let mut awaiting_pong = false;
        let mut next_id: u16 = 0;
        loop {
            tokio::select! {
                Some(request) = self.rx.recv() => {
                    next_id = next_id.wrapping_add(1).max(1);
                    wr.write_all(&encode(request, next_id)).await?;
                    self.sent.store(unix_now(), Ordering::Relaxed);
                }
                read = rd.read_buf(&mut buf) => {
                    if read? == 0 {
                        return Err(std::io::Error::other("connection closed by the broker"));
                    }
                    while let Some((header, body)) = split_packet(&mut buf)? {
                        awaiting_pong = false;
                        if header >> 4 == 3 {
                            let (topic, payload, id) = parse_publish(header, &body)?;
                            if let Some(id) = id {
                                wr.write_all(&[0x40, 2, (id >> 8) as u8, id as u8]).await?;
                                self.sent.store(unix_now(), Ordering::Relaxed);
                            }
                            handler.message(&topic, &payload);
                        }
                    }
                }
                _ = ping.tick() => {
                    if awaiting_pong {
                        return Err(std::io::Error::other("no answer to ping"));
                    }
                    wr.write_all(&[0xC0, 0]).await?;
                    self.sent.store(unix_now(), Ordering::Relaxed);
                    awaiting_pong = true;
                }
            }
        }
    }
}

fn refused(code: u8) -> String {
    match code {
        1 => "refused: unacceptable protocol version".into(),
        2 => "refused: identifier rejected".into(),
        3 => "refused: server unavailable".into(),
        4 => "refused: bad user name or password".into(),
        5 => "refused: not authorized".into(),
        c => format!("refused: code {c}"),
    }
}

fn put_str(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s);
}

fn packet(header: u8, body: Vec<u8>) -> Vec<u8> {
    let mut out = vec![header];
    let mut len = body.len();
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
    out.extend(body);
    out
}

fn connect_packet(o: &Options) -> Vec<u8> {
    // Clean session, a will retained at QoS 1, and a user and password
    // when there is a user.
    let mut flags = 0x02 | 0x04 | 0x08 | 0x20;
    if o.user.is_some() {
        flags |= 0x80 | 0x40;
    }
    let mut body = Vec::new();
    put_str(&mut body, b"MQTT");
    body.push(4);
    body.push(flags);
    body.extend_from_slice(&(KEEPALIVE.as_secs() as u16).to_be_bytes());
    put_str(&mut body, o.client_id.as_bytes());
    put_str(&mut body, o.will.0.as_bytes());
    put_str(&mut body, &o.will.1);
    if let Some(user) = &o.user {
        put_str(&mut body, user.as_bytes());
        put_str(&mut body, o.password.as_bytes());
    }
    packet(0x10, body)
}

fn encode(request: Request, id: u16) -> Vec<u8> {
    match request {
        Request::Publish { topic, payload, qos, retain } => {
            let mut body = Vec::new();
            put_str(&mut body, topic.as_bytes());
            if qos > 0 {
                body.extend_from_slice(&id.to_be_bytes());
            }
            body.extend(payload);
            packet(0x30 | (qos.min(1) << 1) | u8::from(retain), body)
        }
        Request::Subscribe { filter } => {
            let mut body = id.to_be_bytes().to_vec();
            put_str(&mut body, filter.as_bytes());
            body.push(1);
            packet(0x82, body)
        }
    }
}

/// The next whole packet in buf, if it has arrived: its first byte and body.
fn split_packet(buf: &mut BytesMut) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let (mut len, mut shift, mut at) = (0usize, 0, 1);
    loop {
        let Some(&byte) = buf.get(at) else { return Ok(None) };
        len |= usize::from(byte & 0x7F) << shift;
        at += 1;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 21 {
            return Err(std::io::Error::other("malformed packet length"));
        }
    }
    if buf.len() < at + len {
        return Ok(None);
    }
    let header = buf[0];
    buf.advance(at);
    Ok(Some((header, buf.split_to(len).to_vec())))
}

async fn next_packet(rd: &mut (impl AsyncRead + Unpin), buf: &mut BytesMut) -> std::io::Result<(u8, Vec<u8>)> {
    loop {
        if let Some(p) = split_packet(buf)? {
            return Ok(p);
        }
        if rd.read_buf(buf).await? == 0 {
            return Err(std::io::Error::other("connection closed by the broker"));
        }
    }
}

/// A PUBLISH's topic, payload, and packet id when it wants a PUBACK.
fn parse_publish(header: u8, body: &[u8]) -> std::io::Result<(String, Vec<u8>, Option<u16>)> {
    let bad = || std::io::Error::other("malformed PUBLISH");
    let len = usize::from(u16::from_be_bytes([*body.first().ok_or_else(bad)?, *body.get(1).ok_or_else(bad)?]));
    let topic = body.get(2..2 + len).ok_or_else(bad)?;
    let mut at = 2 + len;
    let id = if (header >> 1) & 3 > 0 {
        let id = u16::from_be_bytes([*body.get(at).ok_or_else(bad)?, *body.get(at + 1).ok_or_else(bad)?]);
        at += 2;
        Some(id)
    } else {
        None
    };
    Ok((String::from_utf8_lossy(topic).into_owned(), body[at..].to_vec(), id))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// The next packet a loopback broker reads.
    pub async fn read_packet(rd: &mut (impl AsyncRead + Unpin), buf: &mut BytesMut) -> (u8, Vec<u8>) {
        next_packet(rd, buf).await.unwrap()
    }

    /// A PUBLISH's topic and payload.
    pub fn publish_of(header: u8, body: &[u8]) -> (String, Vec<u8>) {
        let (topic, payload, _) = parse_publish(header, body).unwrap();
        (topic, payload)
    }

    /// A QoS 0 PUBLISH, as a broker sends it.
    pub fn publish_packet(topic: &str, payload: &[u8]) -> Vec<u8> {
        encode(Request::Publish { topic: topic.into(), payload: payload.to_vec(), qos: 0, retain: false }, 0)
    }

    #[test]
    fn broker_urls() {
        let b = Broker::parse("tcp://mqtt.lan:1883").unwrap();
        assert_eq!((b.tls, b.host.as_str(), b.port), (false, "mqtt.lan", 1883));
        assert!(Broker::parse("ssl://10.0.0.2:8883").unwrap().tls);
        assert_eq!(Broker::parse("tcp://[fd00::2]:1883").unwrap().host, "fd00::2");
        assert!(Broker::parse("mqtt.lan:1883").is_err());
        assert!(Broker::parse("wss://mqtt.lan/mqtt").is_err());
        assert!(Broker::parse("tcp://mqtt.lan").is_err());
    }

    #[test]
    fn packets_round_trip() {
        let o = Options { client_id: "nestlocal-S".into(), user: Some("u".into()), password: "p".into(),
                          will: ("nestlocal/S/availability".into(), b"offline".to_vec()), tls: None };
        let connect = connect_packet(&o);
        assert_eq!(&connect[..2], &[0x10, connect.len() as u8 - 2]);
        assert_eq!(connect[9], 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80);

        let publish = encode(Request::Publish { topic: "t/x".into(), payload: vec![b'x'; 300], qos: 1, retain: true }, 7);
        let mut buf = BytesMut::from(&publish[..]);
        let (header, body) = split_packet(&mut buf).unwrap().unwrap();
        assert_eq!(header, 0x33);
        assert!(buf.is_empty());
        let (topic, payload, id) = parse_publish(header, &body).unwrap();
        assert_eq!((topic.as_str(), payload.len(), id), ("t/x", 300, Some(7)));

        // A packet split across reads waits for the rest.
        let mut partial = BytesMut::from(&publish[..10]);
        assert!(split_packet(&mut partial).unwrap().is_none());
    }

    #[test]
    fn remaining_lengths_at_every_width() {
        for (len, width) in [(0, 1), (127, 1), (128, 2), (16_383, 2), (16_384, 3), (2_097_151, 3), (2_097_152, 4)] {
            let p = packet(0x30, vec![7; len]);
            assert_eq!(p.len(), 1 + width + len, "{len}");
            // Every byte but the last: the length's bytes still arriving.
            assert!(split_packet(&mut BytesMut::from(&p[..1 + width])).unwrap().is_none() || len == 0, "{len}");
            assert!(split_packet(&mut BytesMut::from(&p[..p.len() - 1])).unwrap().is_none() || len == 0, "{len}");
            let mut buf = BytesMut::from(&p[..]);
            let (header, body) = split_packet(&mut buf).unwrap().unwrap();
            assert_eq!((header, body.len(), buf.len()), (0x30, len, 0), "{len}");
        }
        // Five length bytes is no MQTT packet.
        assert!(split_packet(&mut BytesMut::from(&[0x30, 0xFF, 0xFF, 0xFF, 0xFF, 0x01][..])).is_err());
        // Packets back to back come out one at a time; a PINGRESP has no body.
        let mut buf = BytesMut::from(&[0xD0, 0x00, 0x90, 0x03, 0x00, 0x01, 0x01, 0x30][..]);
        assert_eq!(split_packet(&mut buf).unwrap(), Some((0xD0, vec![])));
        assert_eq!(split_packet(&mut buf).unwrap(), Some((0x90, vec![0, 1, 1])));
        assert_eq!(split_packet(&mut buf).unwrap(), None);
        assert_eq!(&buf[..], &[0x30], "a started packet is kept for the next read");
    }

    #[test]
    fn publishes_as_they_arrive() {
        // QoS 0: no packet id, an empty payload allowed.
        assert_eq!(parse_publish(0x31, &[0, 1, b't']).unwrap(), ("t".into(), vec![], None));
        assert_eq!(parse_publish(0x32, &[0, 1, b't', 0x12, 0x34, b'p']).unwrap(), ("t".into(), b"p".to_vec(), Some(0x1234)));
        for (header, body) in [(0x30, &[][..]), (0x30, &[0][..]), (0x30, &[0, 5, b't'][..]), (0x32, &[0, 1, b't', 0x12][..])] {
            assert!(parse_publish(header, body).is_err(), "{header:#x} {body:?}");
        }
        // Outgoing: QoS 0 carries no id, and a subscription asks for QoS 1.
        assert_eq!(encode(Request::Publish { topic: "a".into(), payload: b"x".to_vec(), qos: 0, retain: false }, 9),
                   vec![0x30, 4, 0, 1, b'a', b'x']);
        assert_eq!(encode(Request::Subscribe { filter: "a/#".into() }, 0x0102), vec![0x82, 8, 1, 2, 0, 3, b'a', b'/', b'#', 1]);
        // Without a user, neither the user nor the password flag, nor either field.
        let o = Options { client_id: "c".into(), user: None, password: "ignored".into(),
                          will: ("w".into(), b"offline".to_vec()), tls: None };
        let connect = connect_packet(&o);
        assert_eq!(connect[9], 0x02 | 0x04 | 0x08 | 0x20);
        assert!(connect.ends_with(b"\x00\x07offline"));
    }

    #[derive(Default)]
    struct Recorder {
        connected: AtomicBool,
        messages: std::sync::Mutex<Vec<(String, Vec<u8>)>>,
    }

    impl Handler for Recorder {
        fn connected(&self) {
            self.connected.store(true, Ordering::Relaxed);
        }
        fn message(&self, topic: &str, payload: &[u8]) {
            self.messages.lock().unwrap().push((topic.into(), payload.to_vec()));
        }
    }

    fn connection(port: u16) -> (Client, Connection) {
        let broker = Broker::parse(&format!("tcp://127.0.0.1:{port}")).unwrap();
        Client::new(broker, Options { client_id: "nestlocal-S".into(), user: Some("u".into()), password: "p".into(),
                                      will: ("nestlocal/S/availability".into(), b"offline".to_vec()), tls: None })
    }

    #[tokio::test]
    async fn a_session_with_a_loopback_broker() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, mut conn) = connection(listener.local_addr().unwrap().port());
        let handler = Arc::new(Recorder::default());
        let session = tokio::spawn({
            let handler = handler.clone();
            async move { conn.session(&*handler).await }
        });

        let (mut broker, _) = listener.accept().await.unwrap();
        let mut buf = BytesMut::new();
        let (header, body) = next_packet(&mut broker, &mut buf).await.unwrap();
        assert_eq!((header, &body[..6]), (0x10, &b"\x00\x04MQTT"[..]));
        // CONNACK, then a QoS 1 PUBLISH cut in two writes, then two QoS 0
        // ones in one.
        let qos1 = encode(Request::Publish { topic: "nestlocal/S/set/mode".into(), payload: b"heat".to_vec(), qos: 1, retain: false }, 0x0A0B);
        broker.write_all(&[0x20, 2, 0, 0]).await.unwrap();
        broker.write_all(&qos1[..3]).await.unwrap();
        broker.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        broker.write_all(&qos1[3..]).await.unwrap();
        let mut two = encode(Request::Publish { topic: "a".into(), payload: b"1".to_vec(), qos: 0, retain: false }, 0);
        two.extend(encode(Request::Publish { topic: "b".into(), payload: vec![], qos: 0, retain: true }, 0));
        broker.write_all(&two).await.unwrap();

        // Only the QoS 1 one is acknowledged, with its own id.
        let (header, body) = next_packet(&mut broker, &mut buf).await.unwrap();
        assert_eq!((header, body), (0x40, vec![0x0A, 0x0B]));
        assert!(handler.connected.load(Ordering::Relaxed) && client.is_connected());
        client.publish("nestlocal/S/availability", b"online", 1, true);
        let (header, body) = next_packet(&mut broker, &mut buf).await.unwrap();
        assert_eq!(parse_publish(header, &body).unwrap(), ("nestlocal/S/availability".into(), b"online".to_vec(), Some(1)));
        assert_eq!(header, 0x33);
        assert_eq!(*handler.messages.lock().unwrap(), vec![
            ("nestlocal/S/set/mode".to_string(), b"heat".to_vec()), ("a".to_string(), b"1".to_vec()), ("b".to_string(), vec![]),
        ]);

        drop(broker);
        let e = session.await.unwrap().unwrap_err();
        assert_eq!(e.to_string(), "connection closed by the broker");
    }

    #[tokio::test]
    async fn a_tls_broker_without_a_tls_configuration_is_never_dialled() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let broker = Broker::parse(&format!("ssl://127.0.0.1:{}", listener.local_addr().unwrap().port())).unwrap();
        let (client, mut conn) = Client::new(broker, Options { client_id: "nestlocal-S".into(), user: Some("u".into()),
            password: "p".into(), will: ("nestlocal/S/availability".into(), b"offline".to_vec()), tls: None });
        let e = conn.session(&Recorder::default()).await.unwrap_err();
        assert_eq!(e.to_string(), "TLS broker, but no TLS configuration");
        assert!(tokio::time::timeout(Duration::from_millis(50), listener.accept()).await.is_err());
        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn a_refused_connection_says_why() {
        for (connack, want) in [(&[0x20, 2, 0, 4][..], "refused: bad user name or password"),
                                (&[0x20, 2, 0, 9][..], "refused: code 9"),
                                (&[0x90, 3, 0, 1, 0][..], "expected CONNACK")] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (client, mut conn) = connection(listener.local_addr().unwrap().port());
            let handler = Recorder::default();
            let broker = async {
                let (mut broker, _) = listener.accept().await.unwrap();
                next_packet(&mut broker, &mut BytesMut::new()).await.unwrap();
                broker.write_all(connack).await.unwrap();
                broker
            };
            let (result, _broker) = tokio::join!(conn.session(&handler), broker);
            assert_eq!(result.unwrap_err().to_string(), want);
            assert!(!handler.connected.load(Ordering::Relaxed) && !client.is_connected());
        }
    }
}
