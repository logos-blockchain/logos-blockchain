use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

use opentelemetry::{
    KeyValue, Value, global,
    metrics::{AsyncInstrument, Counter, Gauge, Histogram, Meter},
};

fn meter() -> Meter {
    global::meter("logos-blockchain-node")
}

static U64_COUNTERS: LazyLock<Mutex<HashMap<&'static str, Counter<u64>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static F64_COUNTERS: LazyLock<Mutex<HashMap<&'static str, Counter<f64>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static U64_GAUGES: LazyLock<Mutex<HashMap<&'static str, Gauge<u64>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static U64_HISTOGRAMS: LazyLock<Mutex<HashMap<&'static str, Histogram<u64>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static F64_HISTOGRAMS: LazyLock<Mutex<HashMap<&'static str, Histogram<f64>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

type ObservableGaugeCell = Arc<Mutex<Option<u64>>>;
static OBSERVABLE_U64_GAUGES: LazyLock<Mutex<HashMap<&'static str, ObservableGaugeCell>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn reset_cached_instruments() {
    U64_COUNTERS
        .lock()
        .expect("u64 counter lock poisoned")
        .clear();
    F64_COUNTERS
        .lock()
        .expect("f64 counter lock poisoned")
        .clear();
    U64_GAUGES.lock().expect("u64 gauge lock poisoned").clear();
    U64_HISTOGRAMS
        .lock()
        .expect("u64 histogram lock poisoned")
        .clear();
    F64_HISTOGRAMS
        .lock()
        .expect("f64 histogram lock poisoned")
        .clear();
    OBSERVABLE_U64_GAUGES
        .lock()
        .expect("observable u64 gauge lock poisoned")
        .clear();
}

pub trait IntoMetricValue {
    fn into_metric_value(self) -> Value;
}

impl IntoMetricValue for Value {
    fn into_metric_value(self) -> Value {
        self
    }
}

impl IntoMetricValue for &str {
    fn into_metric_value(self) -> Value {
        Value::from(self.to_owned())
    }
}

impl IntoMetricValue for String {
    fn into_metric_value(self) -> Value {
        Value::from(self)
    }
}

impl IntoMetricValue for u16 {
    fn into_metric_value(self) -> Value {
        Value::from(i64::from(self))
    }
}

pub fn key_value(key: &'static str, value: impl IntoMetricValue) -> KeyValue {
    KeyValue::new(key, value.into_metric_value())
}

pub trait IntoMetricU64 {
    fn into_metric_u64(self) -> u64;
}

impl IntoMetricU64 for u64 {
    fn into_metric_u64(self) -> u64 {
        self
    }
}

impl IntoMetricU64 for usize {
    fn into_metric_u64(self) -> u64 {
        u64::try_from(self).unwrap_or(u64::MAX)
    }
}

impl IntoMetricU64 for u32 {
    fn into_metric_u64(self) -> u64 {
        u64::from(self)
    }
}

impl IntoMetricU64 for i32 {
    fn into_metric_u64(self) -> u64 {
        u64::try_from(self).unwrap_or(0)
    }
}

macro_rules! get_instrument {
    ($map:expr, $name:expr, $method:ident) => {{
        match $map.lock() {
            Ok(mut map) => Some(
                map.entry($name)
                    .or_insert_with(|| meter().$method($name).build())
                    .clone(),
            ),
            Err(e) => {
                tracing::error!("Instrument '{}' lock poisoned: {:?}", $name, e);
                None
            }
        }
    }};
    // Support for optional callback for observable gauges.
    ($map:expr, $name:expr, $method:ident, $callback_for_cell:expr) => {{
        match $map.lock() {
            Ok(mut map) => Some(Arc::clone(map.entry($name).or_insert_with(|| {
                let cell = Arc::new(Mutex::new(None));
                meter()
                    .$method($name)
                    .with_callback(($callback_for_cell)(Arc::clone(&cell)))
                    .build();
                cell
            }))),
            Err(e) => {
                tracing::error!("Instrument '{}' lock poisoned: {e:?}", $name);
                None
            }
        }
    }};
}

pub fn increase_counter_u64(
    name: &'static str,
    value: impl IntoMetricU64,
    attributes: &[KeyValue],
) {
    if let Some(c) = get_instrument!(U64_COUNTERS, name, u64_counter) {
        c.add(value.into_metric_u64(), attributes);
    }
}

pub fn counter_f64(name: &'static str, value: f64, attributes: &[KeyValue]) {
    if let Some(counter) = get_instrument!(F64_COUNTERS, name, f64_counter) {
        counter.add(value, attributes);
    }
}

pub fn gauge_u64(name: &'static str, value: impl IntoMetricU64, attributes: &[KeyValue]) {
    if let Some(gauge) = get_instrument!(U64_GAUGES, name, u64_gauge) {
        gauge.record(value.into_metric_u64(), attributes);
    }
}

pub fn observable_gauge_u64_set(name: &'static str, value: impl IntoMetricU64) {
    if let Some(cell) = get_instrument!(
        OBSERVABLE_U64_GAUGES,
        name,
        u64_observable_gauge,
        observe_cell_u64
    ) {
        match cell.lock() {
            Ok(mut current) => *current = Some(value.into_metric_u64()),
            Err(e) => tracing::error!("Observable gauge '{name}' value lock poisoned: {e:?}"),
        }
    }
}

fn observe_cell_u64(
    cell: ObservableGaugeCell,
) -> impl Fn(&dyn AsyncInstrument<u64>) + Send + Sync + 'static {
    move |observer| {
        if let Ok(value) = cell.lock()
            && let Some(value) = *value
        {
            observer.observe(value, &[]);
        }
    }
}

pub fn observable_gauge_u64_clear(name: &'static str) {
    match OBSERVABLE_U64_GAUGES.lock() {
        Ok(map) => {
            if let Some(cell) = map.get(name) {
                match cell.lock() {
                    Ok(mut current) => *current = None,
                    Err(e) => {
                        tracing::error!("Observable gauge '{name}' value lock poisoned: {e:?}");
                    }
                }
            }
        }
        Err(e) => tracing::error!("Observable gauge '{name}' lock poisoned: {e:?}"),
    }
}

pub fn histogram_u64(name: &'static str, value: impl IntoMetricU64, attributes: &[KeyValue]) {
    if let Some(hist) = get_instrument!(U64_HISTOGRAMS, name, u64_histogram) {
        hist.record(value.into_metric_u64(), attributes);
    }
}

pub fn histogram_f64(name: &'static str, value: f64, attributes: &[KeyValue]) {
    if let Some(hist) = get_instrument!(F64_HISTOGRAMS, name, f64_histogram) {
        hist.record(value, attributes);
    }
}
