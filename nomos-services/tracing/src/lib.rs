use std::{
    fmt::{Debug, Display, Formatter},
    io::Write,
    marker::PhantomData,
    panic,
    sync::{Arc, Mutex},
};

use nomos_tracing::{
    filter::envfilter::{EnvFilterConfig, create_envfilter_layer},
    logging::{
        gelf::{GelfConfig, create_gelf_layer},
        local::{FileConfig, create_file_layer, create_writer_layer},
        loki::{LokiConfig, create_loki_layer},
    },
    metrics::otlp::{OtlpMetricsConfig, create_otlp_metrics_layer},
    tracing::otlp::{OtlpTracingConfig, create_otlp_tracing_layer},
};
use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use serde::{Deserialize, Serialize};
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    filter::LevelFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _,
};

#[cfg(feature = "profiling")]
mod console;

pub struct Tracing<RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    logger_guard: Option<WorkerGuard>,
    _runtime_service_id: PhantomData<RuntimeServiceId>,
}

/// This is a wrapper around a writer to allow cloning which is
/// required by contract by Overwatch for a configuration struct
#[derive(Clone)]
pub struct SharedWriter {
    inner: Arc<Mutex<dyn Write + Send + Sync>>,
}

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.lock().unwrap().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.lock().unwrap().flush()
    }
}

impl SharedWriter {
    pub fn new<W: Write + Send + Sync + 'static>(writer: W) -> Self {
        Self {
            inner: Arc::new(Mutex::new(writer)),
        }
    }

    #[must_use]
    pub fn to_inner(&self) -> Arc<Mutex<dyn Write + Send + Sync>> {
        Arc::clone(&self.inner)
    }

    pub fn from_inner(inner: Arc<Mutex<dyn Write + Send + Sync>>) -> Self {
        Self { inner }
    }
}

impl Debug for SharedWriter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedWriter").finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LoggerLayer {
    Gelf(GelfConfig),
    File(FileConfig),
    Loki(LokiConfig),
    Stdout,
    Stderr,
    #[serde(skip)]
    Writer(SharedWriter),
    // do not collect logs
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TracingLayer {
    Otlp(OtlpTracingConfig),
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FilterLayer {
    EnvFilter(EnvFilterConfig),
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MetricsLayer {
    Otlp(OtlpMetricsConfig),
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokioConsoleConfig {
    pub bind_address: String,
    pub port: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ConsoleLayer {
    Console(TokioConsoleConfig),
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TracingSettings {
    pub logger: LoggerLayer,
    pub tracing: TracingLayer,
    pub filter: FilterLayer,
    pub metrics: MetricsLayer,
    pub console: ConsoleLayer,
    #[serde(with = "serde_level")]
    pub level: Level,
}

impl Default for TracingSettings {
    fn default() -> Self {
        Self {
            logger: LoggerLayer::Stdout,
            tracing: TracingLayer::None,
            filter: FilterLayer::None,
            metrics: MetricsLayer::None,
            console: ConsoleLayer::None,
            level: Level::DEBUG,
        }
    }
}

impl TracingSettings {
    #[inline]
    #[must_use]
    pub const fn new(
        logger: LoggerLayer,
        tracing: TracingLayer,
        filter: FilterLayer,
        metrics: MetricsLayer,
        console: ConsoleLayer,
        level: Level,
    ) -> Self {
        Self {
            logger,
            tracing,
            filter,
            metrics,
            console,
            level,
        }
    }
}

impl<RuntimeServiceId> ServiceData for Tracing<RuntimeServiceId> {
    type Settings = TracingSettings;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ();
}

#[async_trait::async_trait]
impl<RuntimeServiceId> ServiceCore<RuntimeServiceId> for Tracing<RuntimeServiceId>
where
    RuntimeServiceId: AsServiceId<Self> + Display + Send,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, overwatch::DynError> {
        use std::sync::Once;

        static ONCE_INIT: Once = Once::new();

        let config = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        let (logger_layer, logger_guard): (
            Box<dyn tracing_subscriber::Layer<_> + Send + Sync>,
            Option<WorkerGuard>,
        ) = match config.logger {
            LoggerLayer::Gelf(config) => {
                let gelf_layer = create_gelf_layer(
                    &config,
                    service_resources_handle.overwatch_handle.runtime(),
                )?;
                (Box::new(gelf_layer), None)
            }
            LoggerLayer::File(config) => {
                let (layer, guard) = create_file_layer(config);
                (Box::new(layer), Some(guard))
            }
            LoggerLayer::Loki(config) => {
                let loki_layer =
                    create_loki_layer(config, service_resources_handle.overwatch_handle.runtime())?;
                (Box::new(loki_layer), None)
            }
            LoggerLayer::Stdout => {
                let (layer, guard) = create_writer_layer(std::io::stdout());
                (Box::new(layer), Some(guard))
            }
            LoggerLayer::Stderr => {
                let (layer, guard) = create_writer_layer(std::io::stderr());
                (Box::new(layer), Some(guard))
            }
            LoggerLayer::Writer(writer) => {
                let (layer, guard) = create_writer_layer(writer);
                (Box::new(layer), Some(guard))
            }
            LoggerLayer::None => (Box::new(tracing_subscriber::fmt::Layer::new()), None),
        };

        let mut other_layers: Vec<Box<dyn tracing_subscriber::Layer<_> + Send + Sync>> = vec![];

        if let TracingLayer::Otlp(config) = config.tracing {
            let tracing_layer = create_otlp_tracing_layer(config)?;
            other_layers.push(Box::new(tracing_layer));
        }

        if let FilterLayer::EnvFilter(config) = config.filter {
            let filter_layer = create_envfilter_layer(config)?;
            other_layers.push(Box::new(filter_layer));
        }

        if let MetricsLayer::Otlp(config) = config.metrics {
            let metrics_layer = create_otlp_metrics_layer(config)?;
            other_layers.push(Box::new(metrics_layer));
        }

        ONCE_INIT.call_once(move || {
            let mut layers: Vec<Box<dyn tracing_subscriber::Layer<_> + Send + Sync>> = vec![];

            let level_filter = {
                #[cfg(feature = "profiling")]
                if let ConsoleLayer::Console(console_config) = &config.console
                    && let Some(console_layer) = console::create_console_layer(console_config)
                {
                    layers.push(console_layer);
                    LevelFilter::TRACE
                } else {
                    LevelFilter::from(config.level)
                }
                #[cfg(not(feature = "profiling"))]
                LevelFilter::from(config.level)
            };

            layers.push(logger_layer);
            layers.extend(other_layers);

            tracing_subscriber::registry()
                .with(level_filter)
                .with(layers)
                .init();
        });

        panic::set_hook(Box::new(nomos_tracing::panic::panic_hook));

        Ok(Self {
            service_resources_handle,
            logger_guard,
            _runtime_service_id: PhantomData,
        })
    }

    async fn run(self) -> Result<(), overwatch::DynError> {
        let Self {
            logger_guard: _logger_guard,
            service_resources_handle,
            ..
        } = self;

        service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        // Wait indefinitely until the service is stopped.
        // When it's stopped, the logger guard will be dropped. That will flush all
        // pending logs.
        std::future::pending::<()>().await;
        Ok(())
    }
}

mod serde_level {
    use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer, de::Error as _};

    use super::Level;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Level, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = <String>::deserialize(deserializer)?;
        v.parse()
            .map_err(|e| D::Error::custom(format!("invalid log level {e}")))
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "Signature must match serde requirement."
    )]
    pub fn serialize<S>(value: &Level, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.as_str().serialize(serializer)
    }
}
