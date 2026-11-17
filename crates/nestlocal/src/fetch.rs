//! The HTTP(S) GETs nestlocal makes itself: the release check and an
//! update's download, which goes to a file as it arrives. The unit's CA store holds Nest's roots alone, so TLS
//! trusts Mozilla's (webpki-roots).

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::Incoming;
use hyper::header::{ACCEPT, HOST, LOCATION, USER_AGENT};
use hyper::{Request, Response, Uri};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

/// Redirects followed: GitHub sends a release asset on to its storage once.
const REDIRECTS: usize = 5;

fn tls() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            Arc::new(rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
        })
        .clone()
}

/// The body of url, following redirects, at most limit bytes, within timeout.
pub async fn get(url: &str, limit: usize, timeout: Duration) -> Result<Vec<u8>, String> {
    within(url, timeout, async {
        let (url, response) = follow(url).await?;
        Ok(Limited::new(response.into_body(), limit)
            .collect()
            .await
            .map_err(|e| format!("{url}: body: {e}"))?
            .to_bytes()
            .to_vec())
    })
    .await
}

/// The body of url written to path as it arrives, as get takes it; path is
/// removed when the download fails.
pub async fn download(url: &str, path: &Path, limit: usize, timeout: Duration) -> Result<(), String> {
    let result = within(url, timeout, async {
        let (url, response) = follow(url).await?;
        let mut file = tokio::fs::File::create(path).await.map_err(|e| format!("{}: {e}", path.display()))?;
        let mut body = response.into_body();
        let mut written = 0;
        while let Some(frame) = body.frame().await {
            let Ok(data) = frame.map_err(|e| format!("{url}: body: {e}"))?.into_data() else {
                continue;
            };
            written += data.len();
            if written > limit {
                return Err(format!("{url}: body: more than {limit} bytes"));
            }
            file.write_all(&data).await.map_err(|e| format!("{}: {e}", path.display()))?;
        }
        file.sync_all().await.map_err(|e| format!("{}: {e}", path.display()))
    })
    .await;
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

async fn within<T>(
    url: &str,
    timeout: Duration,
    request: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tokio::time::timeout(timeout, request)
        .await
        .unwrap_or_else(|_| Err(format!("{url}: no answer within {}", crate::logger::duration(timeout))))
}

/// The answer to url once its redirects are followed, and the URL that gave it.
async fn follow(url: &str) -> Result<(String, Response<Incoming>), String> {
    let mut url = url.to_string();
    for _ in 0..=REDIRECTS {
        let uri: Uri = url.parse().map_err(|e| format!("{url}: {e}"))?;
        let response = once(&uri).await.map_err(|e| format!("{url}: {e}"))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response.headers().get(LOCATION).and_then(|v| v.to_str().ok());
            let location = location.ok_or_else(|| format!("{url}: {status} without a location"))?;
            url = resolve(&uri, location);
            continue;
        }
        if !status.is_success() {
            return Err(format!("{url}: {status}"));
        }
        return Ok((url, response));
    }
    Err(format!("{url}: more than {REDIRECTS} redirects"))
}

/// A redirect's location, absolute or against the URI it came from.
fn resolve(base: &Uri, location: &str) -> String {
    if location.contains("://") {
        return location.to_string();
    }
    let origin = format!("{}://{}", base.scheme_str().unwrap_or("http"), base.authority().map_or("", |a| a.as_str()));
    if location.starts_with('/') {
        format!("{origin}{location}")
    } else {
        format!("{origin}/{location}")
    }
}

async fn once(uri: &Uri) -> Result<Response<Incoming>, String> {
    let host = uri.host().ok_or("no host")?.to_string();
    let https = match uri.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        other => return Err(format!("not http or https: {other:?}")),
    };
    let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
    let request = Request::get(uri.path_and_query().map_or("/", |p| p.as_str()))
        .header(HOST, uri.authority().map_or(host.as_str(), |a| a.as_str()))
        .header(USER_AGENT, concat!("nestlocal/", env!("CARGO_PKG_VERSION")))
        .header(ACCEPT, "*/*")
        .body(Empty::<Bytes>::new())
        .map_err(|e| e.to_string())?;
    let tcp = TcpStream::connect((host.as_str(), port)).await.map_err(|e| e.to_string())?;
    if https {
        let name = ServerName::try_from(host).map_err(|e| e.to_string())?;
        let stream = tokio_rustls::TlsConnector::from(tls()).connect(name, tcp).await.map_err(|e| e.to_string())?;
        send(stream, request).await
    } else {
        send(tcp, request).await
    }
}

async fn send(
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    request: Request<Empty<Bytes>>,
) -> Result<Response<Incoming>, String> {
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.map_err(|e| e.to_string())?;
    tokio::spawn(connection);
    sender.send_request(request).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn redirects_resolve_absolute_rooted_and_relative() {
        let base: Uri = "https://api.example/repos/x".parse().unwrap();
        assert_eq!(resolve(&base, "https://cdn.example/a"), "https://cdn.example/a");
        assert_eq!(resolve(&base, "/b"), "https://api.example/b");
        assert_eq!(resolve(&base, "c"), "https://api.example/c");
    }

    /// Answers each connection with the next canned response.
    async fn serve(responses: Vec<String>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for response in responses {
                let (mut conn, _) = listener.accept().await.unwrap();
                // The whole request, whatever segments it came in: closing on
                // unread bytes would reset the connection under the answer.
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if conn.read(&mut byte).await.unwrap() == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                conn.write_all(response.as_bytes()).await.unwrap();
            }
        });
        format!("http://{address}")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_redirect_is_followed_and_a_body_over_the_limit_refused() {
        let base = serve(vec![
            "HTTP/1.1 302 Found\r\nLocation: /asset\r\nContent-Length: 0\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".into(),
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".into(),
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into(),
        ])
        .await;
        let timeout = Duration::from_secs(30);
        assert_eq!(get(&format!("{base}/release"), 100, timeout).await.unwrap(), b"hello");
        assert!(get(&base, 4, timeout).await.unwrap_err().contains("body"));
        assert!(get(&base, 100, timeout).await.unwrap_err().contains("404"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_download_lands_in_its_file_and_one_over_the_limit_leaves_none() {
        let base = serve(vec![
            "HTTP/1.1 302 Found\r\nLocation: /asset\r\nContent-Length: 0\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".into(),
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".into(),
        ])
        .await;
        let dir = std::env::temp_dir().join(format!("nestlocal-download-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bundle.tgz");
        let timeout = Duration::from_secs(30);
        download(&format!("{base}/release"), &path, 100, timeout).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert!(download(&base, &path, 4, timeout).await.unwrap_err().contains("body"));
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
