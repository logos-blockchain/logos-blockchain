use std::{collections::HashMap, error::Error};

use opentelemetry_otlp::{WithExportConfig as _, WithHttpConfig as _, WithTonicConfig as _};
use serde::{Deserialize, Serialize};
use tonic::metadata::MetadataMap;

use crate::OtlpServiceConfig;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpMetricsConfig {
    #[serde(flatten)]
    pub service: OtlpServiceConfig,
}

pub(super) fn build_grpc_exporter(
    config: OtlpMetricsConfig,
) -> Result<opentelemetry_otlp::MetricExporter, Box<dyn Error + Send + Sync>> {
    let mut builder = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(config.service.url.to_string());

    if let Some(auth) = config.service.authorization_header {
        let mut metadata = MetadataMap::new();
        metadata.insert("authorization", auth.parse()?);
        builder = builder.with_metadata(metadata);
    }

    Ok(builder.build()?)
}

pub(super) fn build_http_exporter(
    config: OtlpMetricsConfig,
) -> Result<opentelemetry_otlp::MetricExporter, Box<dyn Error + Send + Sync>> {
    let mut builder = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(config.service.url.to_string());

    if let Some(auth) = config.service.authorization_header {
        builder = builder.with_headers(HashMap::from([("authorization".to_owned(), auth)]));
    }

    Ok(builder.build()?)
}
