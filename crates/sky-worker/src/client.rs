//! Typed client wrappers for calling the worker's RPC services.
//!
//! These clients sit atop the raw tonic-generated clients, providing
//! a more ergonomic API: our error types instead of `tonic::Status`,
//! automatic request ID propagation via Connect metadata, and a hook
//! for future framework-level concerns (retries, circuit breaking,
//! tracing span attachment).

use sky_proto::v1::{GreetRequest, hello_service_client::HelloServiceClient, SkyResponse};
use sky_runtime::{RequestId, WorkerError};
use tonic::transport::Channel;

/// Client for calling the worker's `HelloService`.
///
/// Cheap to clone — the underlying channel is reference-counted.
/// Clones share the same underlying connection.
#[derive(Clone)]
pub struct HelloClient {
    inner: HelloServiceClient<Channel>,
    pool_name: String,
}

impl HelloClient {
    /// Construct a HelloClient over the given channel.
    ///
    /// `pool_name` is used in error messages to identify which worker
    /// pool an error originated from. For Phase 1 this is always
    /// "default" since there's one pool.
    pub(crate) fn new(channel: Channel, pool_name: impl Into<String>) -> Self {
        Self {
            inner: HelloServiceClient::new(channel),
            pool_name: pool_name.into(),
        }
    }

    /// Call `HelloService.Greet` on the worker.
    ///
    /// The `request_id` is forwarded to the worker as an `x-request-id`
    /// Connect metadata header, which the worker-side logger picks up
    /// for correlation.
    pub async fn greet(
        &self,
        request: GreetRequest,
        request_id: RequestId,
    ) -> Result<SkyResponse, WorkerError> {
        // Clone the client for this call. tonic's generated clients
        // require &mut self for RPC methods; cloning is cheap because
        // Channel is reference-counted internally.
        let mut client = self.inner.clone();

        // Wrap the request in tonic's Request type so we can attach metadata.
        let mut tonic_request = tonic::Request::new(request);

        // Attach the request ID as metadata. The header name must be lowercase
        // per gRPC conventions.
        tonic_request.metadata_mut().insert(
            "x-request-id",
            request_id
                .to_string()
                .parse()
                .expect("request ID should always be valid ASCII"),
        );

        // Make the RPC and translate the result into our error types.
        let response = client.greet(tonic_request).await.map_err(|status| {
            WorkerError::WorkerReturnedError {
                pool: self.pool_name.clone(),
                message: status.message().to_string(),
            }
        })?;

        // The response is wrapped in tonic::Response<T>; we only want T.
        Ok(response.into_inner())
    }
}
