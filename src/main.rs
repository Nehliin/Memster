use anyhow::Result;
use hyper::Body;
use hyper::Error;
use hyper::Method;
use hyper::Request;
use hyper::Response;
use hyper::Server;
use std::net::TcpListener;
use tower::make::Shared;
use tower::ServiceBuilder;
use tower_http::trace::DefaultMakeSpan;
use tower_http::trace::DefaultOnResponse;
use tower_http::trace::TraceLayer;
use tower_http::LatencyUnit;
use tracing::{error, info, warn};


async fn set() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone



pub async fn run(listener: TcpListener) -> Result<()> {
    println!("starting");
    let service = ServiceBuilder::new()
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().include_headers(true))
                .on_response(
                    DefaultOnResponse::new()
                        .include_headers(true)
                        .latency_unit(LatencyUnit::Micros),
                ),
        )
        .service_fn(handler);
    info!("Starting Server On: {}", listener.local_addr()?);
    Ok(Server::from_tcp(listener)?
        .serve(Shared::new(service))
        .await?)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_thread_names(true)
        .pretty()
        .with_max_level(tracing::Level::INFO)
        .init();

    // from config
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], 8080).into();
    let listener = TcpListener::bind(&addr).expect("failed to bind");
    run(listener).await.expect("Server crashed");
}

#[cfg(test)]
mod test {
    use super::*;
    use bytes::Bytes;
    use hyper::StatusCode;
    use reqwest::Client;
    use reqwest::Response;
    // TODO Move this to the lib itself
    struct MemsterClient {
        // Use request instead
        client: Client,
        server_addr: String,
    }

    impl MemsterClient {
        // More idiomatic perhaps i.e return result insted
        // Non string values?
        async fn set(&self, key: String, value: Bytes) -> Response {
            self.client
                .post(format!("{}/set/{}", self.server_addr, key))
                .body(value)
                .send()
                .await
                .expect("Failed to send set key request")
        }

        async fn get(&self, key: String) -> Response {
            self.client
                .post(format!("{}/get/{}", self.server_addr, key))
                .send()
                .await
                .expect("Failed to send get key request")
        }
    }

    fn setup_server() -> MemsterClient {
        //lasy static
        tracing_subscriber::fmt()
            .with_thread_names(true)
            .with_max_level(tracing::Level::INFO)
            .init();
        let addr: std::net::SocketAddr = ([127, 0, 0, 1], 0).into();
        let listener = TcpListener::bind(&addr).expect("Integration test Socket can't be bound");
        let local_addr = listener.local_addr().unwrap();
        let _ = tokio::spawn(run(listener));
        info!("addr!: {}", local_addr);
        MemsterClient {
            client: Client::new(),
            server_addr: format!("http://{}", local_addr),
        }
    }

    #[tokio::test]
    async fn insert() {
        let client = setup_server();
        let response = client
            .set("some-key".into(), Bytes::from("Some value"))
            .await;
        assert_eq!(StatusCode::OK, response.status());
        let response = client.get("some-key".into()).await;
        assert_eq!(StatusCode::OK, response.status());
        assert_eq!("Some value".to_string(), response.text().await.unwrap());
    }
}
