use std::{collections::HashMap, fmt::Display, sync::Arc};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{ser::SerializeStruct, Deserialize, Serialize};
use tokio::sync::RwLock;
use tonic::Code;
use tracing::{error, warn};

use crate::{
    clients::{ClientCode, Error},
    pb::grpc::health::v1::HealthCheckResponse,
};

/// A health check endpoint for a singular client.
/// NOTE: Only implemented by HTTP clients, gRPC clients with health check support should use the generated `grpc::health::v1::health_client::HealthClient` service.
pub trait HealthCheck {
    /// Makes a request to the client service health check endpoint and turns result into a `HealthCheckResult`.
    fn check(&self) -> impl std::future::Future<Output = HealthCheckResult> + Send;
}

/// A health probe for aggregated health check results of multiple client services.
pub trait HealthProbe {
    /// Makes a health check request to each client and returns a map of client service ids to health check results.
    fn health(
        &self,
    ) -> impl std::future::Future<Output = Result<HashMap<String, HealthCheckResult>, Error>> + Send;
}

/// Health status determined for or returned by a client service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HealthStatus {
    /// The service is healthy and should be considered ready to serve requests.
    #[serde(rename = "HEALTHY")]
    Healthy,
    /// The service is unhealthy and should be considered not ready to serve requests.
    #[serde(rename = "UNHEALTHY")]
    Unhealthy,
    /// The health status of the service (and possibly the service itself) is unknown.
    /// The health check response indicated the service's health is unknown or the health request failed in a way that could have been a misconfiguration,
    /// meaning the actual service could still be healthy.
    #[serde(rename = "UNKNOWN")]
    Unknown,
}

/// An optional response body that can be interpreted from an HTTP health check response.
/// This is a minimal contract that allows HTTP health requests to opt in to more detailed health check responses than just the status code.
/// If the body omitted, the health check response is considered successful if the status code is `HTTP 200 OK`.
#[derive(serde::Deserialize)]
pub struct OptionalHealthCheckResponseBody {
    /// `HEALTHY`, `UNHEALTHY`, or `UNKNOWN`. Although `HEALTHY` is already implied without a body.
    pub health_status: HealthStatus,
    /// Optional reason for the health check result status being `UNHEALTHY` or `UNKNOWN`.
    /// May be omitted overall if the health check was successful.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Result of a health check request.
#[derive(Debug, Clone)]
pub struct HealthCheckResult {
    /// Overall health status of client service.
    /// `HEALTHY`, `UNHEALTHY`, or `UNKNOWN`.
    pub health_status: HealthStatus,
    /// Response code of the latest health check request.
    /// This should be omitted on serialization if the health check was successful (when the response is `HTTP 200 OK` or `gRPC 0 OK`).
    pub response_code: ClientCode,
    /// Optional reason for the health check result status being `UNHEALTHY` or `UNKNOWN`.
    /// May be omitted overall if the health check was successful.
    pub reason: Option<String>,
}

/// A cache to hold the latest health check results for each client service.
/// Orchestrator has a reference-counted mutex-protected instance of this cache.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HealthCheckCache {
    pub detectors: HashMap<String, HealthCheckResult>,
    pub chunkers: HashMap<String, HealthCheckResult>,
    pub generation: HashMap<String, HealthCheckResult>,
}

/// Response for the readiness probe endpoint that holds a serialized cache of health check results for each client service.
#[derive(Debug, Clone, Serialize)]
pub struct HealthProbeResponse {
    pub services: HealthCheckCache,
}

/// Query param for triggering the client health check probe on the `/info` endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct HealthCheckProbeParams {
    /// Whether to probe the client services' health checks or just return the cached health status.
    #[serde(default)]
    pub probe: bool,
}

impl HealthCheckResult {
    pub fn reason_from_health_check_response(response: &HealthCheckResponse) -> Option<String> {
        match response.status {
            0 => Some("from gRPC health check serving status: UNKNOWN".to_string()),
            1 => None,
            2 => Some("from gRPC health check serving status: NOT_SERVING".to_string()),
            3 => Some("from gRPC health check serving status: SERVICE_UNKNOWN".to_string()),
            _ => {
                error!(
                    "Unexpected gRPC health check serving status: {}",
                    response.status
                );
                Some(format!(
                    "Unexpected gRPC health check serving status: {}",
                    response.status
                ))
            }
        }
    }
}

impl HealthCheckCache {
    pub fn is_initialized(&self) -> bool {
        !self.detectors.is_empty() && !self.chunkers.is_empty() && !self.generation.is_empty()
    }
}

impl HealthProbeResponse {
    pub async fn from_cache(cache: Arc<RwLock<HealthCheckCache>>) -> Self {
        let services = cache.read().await.clone();
        Self { services }
    }
}

impl Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealthStatus::Healthy => write!(f, "HEALTHY"),
            HealthStatus::Unhealthy => write!(f, "UNHEALTHY"),
            HealthStatus::Unknown => write!(f, "UNKNOWN"),
        }
    }
}

impl Display for HealthCheckCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut services = vec![];
        let mut detectors = vec![];
        let mut chunkers = vec![];
        let mut generation = vec![];
        for (service, result) in &self.detectors {
            detectors.push(format!("\t\t{}: {}", service, result));
        }
        for (service, result) in &self.chunkers {
            chunkers.push(format!("\t\t{}: {}", service, result));
        }
        for (service, result) in &self.generation {
            generation.push(format!("\t\t{}: {}", service, result));
        }
        if !self.detectors.is_empty() {
            services.push(format!("\tdetectors: {{\n{}\t}}", detectors.join(",\n")));
        }
        if !self.chunkers.is_empty() {
            services.push(format!("\tchunkers: {{\n{}\t}}", chunkers.join(",\n")));
        }
        if !self.generation.is_empty() {
            services.push(format!("\tgeneration: {{\n{}\t}}", generation.join(",\n")));
        }
        write!(
            f,
            "configured client services: {{\n{}\n}}",
            services.join(",\n")
        )
    }
}

impl Display for HealthProbeResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.services)
    }
}

impl Serialize for HealthCheckResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.health_status {
            HealthStatus::Healthy => self.health_status.serialize(serializer),
            _ => match &self.reason {
                Some(reason) => {
                    let mut state = serializer.serialize_struct("HealthCheckResult", 3)?;
                    state.serialize_field("health_status", &self.health_status)?;
                    state.serialize_field("response_code", &self.response_code.to_string())?;
                    state.serialize_field("reason", reason)?;
                    state.end()
                }
                None => {
                    let mut state = serializer.serialize_struct("HealthCheckResult", 2)?;
                    state.serialize_field("health_status", &self.health_status)?;
                    state.serialize_field("response_code", &self.response_code.to_string())?;
                    state.end()
                }
            },
        }
    }
}

impl Display for HealthCheckResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reason {
            Some(reason) => write!(
                f,
                "{} ({})\n\t\t\t{}",
                self.health_status, self.response_code, reason
            ),
            None => write!(f, "{} ({})", self.health_status, self.response_code),
        }
    }
}

impl From<Result<tonic::Response<HealthCheckResponse>, tonic::Status>> for HealthCheckResult {
    fn from(result: Result<tonic::Response<HealthCheckResponse>, tonic::Status>) -> Self {
        match result {
            Ok(response) => {
                let response = response.into_inner();
                Self {
                    health_status: response.into(),
                    response_code: ClientCode::Grpc(Code::Ok),
                    reason: Self::reason_from_health_check_response(&response),
                }
            }
            Err(status) => Self {
                health_status: HealthStatus::Unknown,
                response_code: ClientCode::Grpc(status.code()),
                reason: Some(format!("gRPC health check failed: {}", status)),
            },
        }
    }
}

impl From<HealthCheckResponse> for HealthStatus {
    fn from(value: HealthCheckResponse) -> Self {
        // NOTE: gRPC Health v1 status codes: 0 = UNKNOWN, 1 = SERVING, 2 = NOT_SERVING, 3 = SERVICE_UNKNOWN
        match value.status {
            1 => Self::Healthy,
            2 => Self::Unhealthy,
            _ => Self::Unknown,
        }
    }
}

impl From<StatusCode> for HealthStatus {
    fn from(code: StatusCode) -> Self {
        match code.as_u16() {
            200 => Self::Healthy,
            201..=299 => {
                warn!(
                    "Unexpected HTTP successful health check response status code: {}",
                    code
                );
                Self::Healthy
            }
            503 => Self::Unhealthy,
            500..=502 | 504..=599 => {
                warn!(
                    "Unexpected HTTP server error health check response status code: {}",
                    code
                );
                Self::Unhealthy
            }
            _ => {
                warn!(
                    "Unexpected HTTP client error health check response status code: {}",
                    code
                );
                Self::Unknown
            }
        }
    }
}

impl IntoResponse for HealthProbeResponse {
    fn into_response(self) -> Response {
        (StatusCode::OK, Json(self)).into_response()
    }
}
