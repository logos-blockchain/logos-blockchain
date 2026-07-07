use std::{collections::HashMap, error::Error};

use opentelemetry::KeyValue;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{WithExportConfig as _, WithHttpConfig as _, WithTonicConfig as _};
use opentelemetry_sdk::{Resource, logs::SdkLoggerProvider};
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use serde::{Deserialize, Serialize};
use tonic::metadata::MetadataMap;

use crate::{OtlpProtocol, OtlpServiceConfig};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpLoggingConfig {
    #[serde(flatten)]
    pub service: OtlpServiceConfig,
}

pub fn create_otlp_layer(
    config: OtlpLoggingConfig,
) -> Result<
    OpenTelemetryTracingBridge<SdkLoggerProvider, opentelemetry_sdk::logs::SdkLogger>,
    Box<dyn Error + Send + Sync>,
> {
    let resource = Resource::builder()
        .with_attributes(vec![KeyValue::new(
            SERVICE_NAME,
            config.service.service_name.clone(),
        )])
        .build();

    let exporter = match config.service.protocol {
        OtlpProtocol::Grpc => build_grpc_exporter(config)?,
        OtlpProtocol::Http => build_http_exporter(config)?,
    };

    let logger_provider = SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build();

    Ok(OpenTelemetryTracingBridge::new(&logger_provider))
}

fn build_grpc_exporter(
    config: OtlpLoggingConfig,
) -> Result<opentelemetry_otlp::LogExporter, Box<dyn Error + Send + Sync>> {
    let mut builder = opentelemetry_otlp::LogExporter::builder()
        .with_tonic()
        .with_endpoint(config.service.url.to_string());

    if let Some(auth) = config.service.authorization_header {
        let mut metadata = MetadataMap::new();
        metadata.insert("authorization", auth.parse()?);
        builder = builder.with_metadata(metadata);
    }

    Ok(builder.build()?)
}

fn build_http_exporter(
    config: OtlpLoggingConfig,
) -> Result<opentelemetry_otlp::LogExporter, Box<dyn Error + Send + Sync>> {
    let mut builder = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(config.service.url.to_string());

    if let Some(auth) = config.service.authorization_header {
        builder = builder.with_headers(HashMap::from([("authorization".to_owned(), auth)]));
    }

    Ok(builder.build()?)
}
