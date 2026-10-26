//! Accepted TCP connection metadata, independent of forwarded HTTP headers.
use axum::{extract::connect_info::Connected, serve::IncomingStream};
use ndimon_types::NetworkSessionPath;
use std::net::{IpAddr, SocketAddr};
use tokio::net::TcpListener;

#[derive(Debug, Clone, Copy)]
pub struct SocketInfo {
    pub peer: SocketAddr,
    pub local: Option<SocketAddr>,
}
impl Connected<IncomingStream<'_, TcpListener>> for SocketInfo {
    fn connect_info(stream: IncomingStream<'_, TcpListener>) -> Self {
        Self {
            peer: *stream.remote_addr(),
            local: stream.io().local_addr().ok(),
        }
    }
}
fn mapped(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4),
        other => other,
    }
}
impl SocketInfo {
    pub fn network_session(&self) -> Option<NetworkSessionPath> {
        Some(NetworkSessionPath::new(
            mapped(self.peer.ip()),
            mapped(self.local?.ip()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::ConnectInfo, routing::get};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    #[tokio::test]
    async fn accepted_socket_reports_actual_peer_and_local_despite_forwarded_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/", get(|ConnectInfo(info): ConnectInfo<SocketInfo>| async move {
            let path = info.network_session().unwrap();
            Json(serde_json::json!({"peer":path.peer.to_string(),"local":path.local.to_string()}))
        }));
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketInfo>(),
            )
            .await
            .unwrap();
        });
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nX-Forwarded-For: 192.0.2.9\r\nX-Forwarded-Host: 192.0.2.10\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut response = String::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read_to_string(&mut response),
        )
        .await;
        server.abort();
        result.unwrap().unwrap();
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body).unwrap(),
            serde_json::json!({"peer":"127.0.0.1","local":"127.0.0.1"})
        );
    }
}
