use std::error::Error;

use opentelemetry::KeyValue;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{WithExportConfig as _, WithTonicConfig as _};
use opentelemetry_sdk::{Resource, logs::SdkLoggerProvider};
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use serde::{Deserialize, Serialize};
use tonic::{
    Request, Status,
    metadata::{Ascii, MetadataValue},
};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpConfig {
    pub endpoint: Url,
    pub service_name: String,
    pub authorization_header: Option<String>,
}

pub fn create_otlp_layer(
    config: OtlpConfig,
) -> Result<
    OpenTelemetryTracingBridge<SdkLoggerProvider, opentelemetry_sdk::logs::SdkLogger>,
    Box<dyn Error + Send + Sync>,
> {
    let resource = Resource::builder()
        .with_attributes(vec![KeyValue::new(SERVICE_NAME, config.service_name)])
        .build();

    let exporter = {
        let mut exporter = opentelemetry_otlp::LogExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.to_string());
        if let Some(auth_header) = config.authorization_header {
            let Ok(auth_header_metadata) = auth_header.parse::<MetadataValue<Ascii>>() else {
                return Err(Box::new(Status::invalid_argument(
                    "Invalid authorization header value",
                )));
            };
            exporter = exporter.with_interceptor(move |mut req: Request<()>| {
                req.metadata_mut()
                    .insert("authorization", auth_header_metadata.clone());
                Ok(req)
            });
        }

        exporter.build()?
    };

    let logger_provider = SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build();

    Ok(OpenTelemetryTracingBridge::new(&logger_provider))
}
