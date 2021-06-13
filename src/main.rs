use crate::{
    config::{setup_tracing, MemsterConfig},
    storage::Storage,
};
use anyhow::Result;
use bytes::Bytes;
use hyper::{
    body::HttpBody,
    header::{self, HeaderName, HeaderValue},
    Body, Request, Response, StatusCode,
};
use std::{net::TcpListener, sync::Arc, time::Duration};
use storage::Key;
use tower::{make::Shared, ServiceBuilder};
use tower_http::{
    add_extension::AddExtensionLayer,
    compression::CompressionLayer,
    sensitive_headers::SetSensitiveHeadersLayer,
    set_header::SetResponseHeaderLayer,
    trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer},
    LatencyUnit,
};
use tracing::info;
use warp::{filters, hyper::Server, Filter};

mod config;
mod storage;

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn get_handler(key: Key, storage: Arc<Storage>) -> Response<Body> {
    if let Some(value) = storage.get(&key) {
        Response::new(Body::from(value))
    } else {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    }
}

fn set_handler(key: Key, body: Bytes, storage: Arc<Storage>) -> Response<Body> {
    storage.insert(key, body);
    Response::default()
}

fn response_content_length<B: HttpBody>(response: &Response<B>) -> Option<HeaderValue> {
    response.body().size_hint().exact().map(|size| {
        HeaderValue::from_str(&size.to_string())
            .expect("Response body size couldn't be converted to string")
    })
}

/// Sets up the service it self and starts it
async fn run(listener: TcpListener, storage: Storage) -> Result<()> {
    let get_endpoint = warp::get()
        .and(warp::path("get"))
        .and(warp::path::param())
        .and(filters::ext::get::<Arc<Storage>>())
        .map(get_handler);
    let set_endpoint = warp::post()
        .and(warp::path("set"))
        .and(warp::path::param())
        .and(filters::body::bytes())
        .and(filters::ext::get::<Arc<Storage>>())
        .map(set_handler);

    let warp_service = warp::service(get_endpoint.or(set_endpoint));
    let service = ServiceBuilder::new()
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().include_headers(true))
                .on_request(DefaultOnRequest::new())
                .on_response(DefaultOnResponse::new().latency_unit(LatencyUnit::Micros)),
        )
        .layer(AddExtensionLayer::new(Arc::new(storage)))
        .timeout(Duration::from_secs(5))
        // Compress responses
        .layer(CompressionLayer::new())
        // Set the `Content-Length` header if possible
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_LENGTH,
            response_content_length,
        ))
        // Sets the `Content-Type` for responses
        .layer(SetResponseHeaderLayer::<_, Request<Body>>::overriding(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        ))
        // Hide sensitive headers from the logs
        .layer(SetSensitiveHeadersLayer::new(vec![
            header::AUTHORIZATION,
            header::COOKIE,
            header::WWW_AUTHENTICATE,
            header::PROXY_AUTHORIZATION,
            HeaderName::from_static("cookie2"),
        ]))
        .service(warp_service);

    info!("Starting Server On: {}", listener.local_addr()?);
    Ok(Server::from_tcp(listener)?
        .serve(Shared::new(service))
        .await?)
}

#[tokio::main]
async fn main() {
    let config = MemsterConfig::load().expect("Failed to read memster config");
    setup_tracing(config.json_logging);

    info!("Setting up server with config: {:#?}", config);
    let listener = TcpListener::bind(&format!("{}:{}", config.host, config.port))
        .expect("Failed to bind TcpListener");
    let storage = Storage::new(
        config.pre_allocted_bytes,
        Duration::from_secs(config.ttl),
        config.num_shards,
    )
    .expect("Failed to create storage");

    run(listener, storage)
        .await
        .expect("Server crashed while starting");
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use super::*;
    use bytes::Bytes;
    use reqwest::Client;
    use reqwest::Response;
    use reqwest::StatusCode;

    const TTL: Duration = Duration::from_secs(600);

    struct MemsterClient {
        client: Client,
        server_addr: String,
    }

    impl MemsterClient {
        async fn set(&self, key: Key, value: Bytes) -> Response {
            self.client
                .post(format!("{}/set/{}", self.server_addr, key))
                .body(value)
                .send()
                .await
                .expect("Failed to send set key request")
        }

        async fn get(&self, key: Key) -> Response {
            self.client
                .get(format!("{}/get/{}", self.server_addr, key))
                .send()
                .await
                .expect("Failed to send get key request")
        }
    }

    lazy_static::lazy_static! {
        static ref TRACING: () = {
            // easier to read without json logging
            setup_tracing(false);
        };
    }

    fn setup_server() -> MemsterClient {
        // Might be worth createing a span for each test runner
        lazy_static::initialize(&TRACING);
        let addr: std::net::SocketAddr = ([127, 0, 0, 1], 0).into();
        let listener = TcpListener::bind(&addr).expect("Integration test Socket can't be bound");
        let local_addr = listener.local_addr().unwrap();
        let storage = Storage::new(0, TTL, None).unwrap();
        let _ = tokio::spawn(run(listener, storage));
        MemsterClient {
            client: Client::new(),
            server_addr: format!("http://{}", local_addr),
        }
    }

    // Simulate moving forward in time
    async fn time_travel(duration: Duration) {
        tokio::time::pause();
        tokio::time::advance(duration).await;
        tokio::time::resume();
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
        assert_eq!("Some value".to_owned(), response.text().await.unwrap());
    }

    #[tokio::test]
    async fn update() {
        let client = setup_server();
        let response = client
            .set("some-key".into(), Bytes::from("Some value"))
            .await;
        assert_eq!(StatusCode::OK, response.status());
        let response = client
            .set("some-key".into(), Bytes::from("Some other value"))
            .await;
        assert_eq!(StatusCode::OK, response.status());
        let response = client.get("some-key".into()).await;
        assert_eq!(StatusCode::OK, response.status());
        assert_eq!(
            "Some other value".to_owned(),
            response.text().await.unwrap()
        );
    }

    #[tokio::test]
    async fn non_existent_key() {
        let client = setup_server();
        let response = client.get("non-existing-key".into()).await;
        assert_eq!(StatusCode::NOT_FOUND, response.status());
    }

    #[tokio::test]
    async fn simple_ttl() {
        let client = setup_server();
        let response = client
            .set("some-key".into(), Bytes::from("Some value"))
            .await;
        assert_eq!(StatusCode::OK, response.status());
        let response = client.get("some-key".into()).await;
        assert_eq!(StatusCode::OK, response.status());
        assert_eq!("Some value".to_owned(), response.text().await.unwrap());
        time_travel(TTL).await;
        let response = client.get("some-key".into()).await;
        assert_eq!(StatusCode::NOT_FOUND, response.status());
    }

    #[tokio::test]
    async fn complex_ttl() {
        let client = setup_server();
        // Insert some data
        for i in 0..1000 {
            let response = client
                .set(format!("some-key-{}", i).into(), Bytes::from("Some value"))
                .await;
            assert_eq!(StatusCode::OK, response.status());
        }
        // Time travel
        time_travel(TTL / 2).await;
        // make sure it's present
        for i in 0..1000 {
            let response = client.get(format!("some-key-{}", i).into()).await;
            assert_eq!(StatusCode::OK, response.status());
            assert_eq!("Some value".to_owned(), response.text().await.unwrap());
        }
        // Insert some more data
        for i in 1001..1010 {
            let response = client
                .set(format!("some-key-{}", i).into(), Bytes::from("Some value"))
                .await;
            assert_eq!(StatusCode::OK, response.status());
        }
        // make all old expire
        time_travel(TTL / 2).await;
        // Make sure old keys have been deleted while the newer remains
        for i in 0..1010 {
            let response = client.get(format!("some-key-{}", i).into()).await;
            if i > 1000 {
                assert_eq!(StatusCode::OK, response.status());
                assert_eq!("Some value".to_owned(), response.text().await.unwrap());
            } else {
                assert_eq!(StatusCode::NOT_FOUND, response.status());
            }
        }
    }

    #[tokio::test]
    async fn ttl_is_updated() {
        let client = setup_server();
        // Insert some data
        let response = client
            .set(format!("some-key").into(), Bytes::from("Some value"))
            .await;
        assert_eq!(StatusCode::OK, response.status());
        // Time travel
        time_travel(TTL - Duration::from_secs(5)).await;
        // Update the key which should reset the ttl
        let response = client
            .set(format!("some-key").into(), Bytes::from("Some other value"))
            .await;
        assert_eq!(StatusCode::OK, response.status());
        time_travel(Duration::from_secs(6)).await;
        // Make sure the key is still present
        let response = client.get(format!("some-key").into()).await;
        assert_eq!(StatusCode::OK, response.status());
        assert_eq!(
            "Some other value".to_string(),
            response.text().await.unwrap()
        );
        // Time travel again to the final ttl of the key
        time_travel(TTL).await;
        // Check that it's no longer present
        let response = client.get(format!("some-key").into()).await;
        assert_eq!(StatusCode::NOT_FOUND, response.status());
    }
}
