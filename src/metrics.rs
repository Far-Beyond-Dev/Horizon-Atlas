use crate::config::{MetricsConfig, MetricsExporter as MetricsExporterConfig};
use crate::errors::{AtlasError, Result};
use crate::proxy::ProxyMetricsSnapshot;
use crate::routing::RoutingMetricsSnapshot;
use crate::transitions::TransitionMetricsSnapshot;
use crate::health::ClusterHealthSummary;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use parking_lot::RwLock;
use prometheus::{
    Registry, Counter, Gauge, Histogram, HistogramVec, CounterVec, GaugeVec,
    Encoder, TextEncoder, opts, histogram_opts,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{debug, info, error, warn};

pub struct MetricsManager {
    config: MetricsConfig,
    registry: Arc<Registry>,
    metrics: Arc<AtlasMetrics>,
    exporters: Vec<Box<dyn MetricsExporter>>,
    collection_tasks: Vec<tokio::task::JoinHandle<()>>,
    http_server_task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
pub struct AtlasMetrics {
    // Connection metrics
    pub active_client_connections: Gauge,
    pub active_server_connections: Gauge,
    pub total_connections: Counter,
    pub connection_errors: Counter,
    
    // Message metrics
    pub messages_processed: Counter,
    pub messages_failed: Counter,
    pub message_latency: Histogram,
    pub bytes_transferred: Counter,
    
    // Routing metrics
    pub routes_calculated: Counter,
    pub routing_cache_hits: Counter,
    pub routing_cache_misses: Counter,
    pub position_lookups: Counter,
    
    // Transition metrics
    pub transitions_initiated: Counter,
    pub transitions_completed: Counter,
    pub transitions_failed: Counter,
    pub transition_duration: Histogram,
    
    // Health metrics
    pub healthy_servers: Gauge,
    pub failed_servers: Gauge,
    pub server_response_times: HistogramVec,
    pub server_availability: GaugeVec,
    
    // Cluster metrics
    pub cluster_nodes: Gauge,
    pub cluster_leader_elections: Counter,
    pub cluster_consensus_duration: Histogram,
    
    // System metrics
    pub cpu_usage: Gauge,
    pub memory_usage: Gauge,
    pub disk_usage: Gauge,
    pub network_bytes_sent: Counter,
    pub network_bytes_received: Counter,
    
    // Performance metrics
    pub request_duration: HistogramVec,
    pub error_rate: GaugeVec,
    pub throughput: GaugeVec,
}

pub trait MetricsExporter: Send + Sync {
    fn export(&self, metrics: &MetricsSnapshot) -> Result<()>;
    fn name(&self) -> &str;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub timestamp: DateTime<Utc>,
    pub proxy_metrics: ProxyMetricsSnapshot,
    pub routing_metrics: RoutingMetricsSnapshot,
    pub transition_metrics: TransitionMetricsSnapshot,
    pub health_summary: ClusterHealthSummary,
    pub system_metrics: SystemMetrics,
    pub custom_metrics: HashMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemMetrics {
    pub uptime_seconds: u64,
    pub cpu_usage_percent: f32,
    pub memory_usage_bytes: u64,
    pub memory_total_bytes: u64,
    pub disk_usage_bytes: u64,
    pub disk_total_bytes: u64,
    pub network_bytes_sent: u64,
    pub network_bytes_received: u64,
    pub open_file_descriptors: u32,
    pub goroutines_count: u32,
}

impl MetricsManager {
    pub fn new(config: MetricsConfig) -> Result<Self> {
        let registry = Arc::new(Registry::new());
        let metrics = Arc::new(AtlasMetrics::new(&registry)?);
        let exporters = Self::create_exporters(&config)?;
        
        Ok(Self {
            config,
            registry,
            metrics,
            exporters,
            collection_tasks: Vec::new(),
            http_server_task: None,
        })
    }
    
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting metrics manager");
        
        if self.config.enabled {
            self.start_collection_task().await;
            self.start_export_task().await;
            
            if !self.exporters.is_empty() {
                self.start_http_server().await?;
            }
        }
        
        Ok(())
    }
    
    pub async fn stop(&mut self) {
        info!("Stopping metrics manager");
        
        for task in self.collection_tasks.drain(..) {
            task.abort();
        }
        
        if let Some(task) = self.http_server_task.take() {
            task.abort();
        }
        
        info!("Metrics manager stopped");
    }
    
    pub fn get_metrics(&self) -> Arc<AtlasMetrics> {
        Arc::clone(&self.metrics)
    }
    
    pub fn record_proxy_metrics(&self, proxy_metrics: ProxyMetricsSnapshot) {
        self.metrics.active_client_connections.set(proxy_metrics.active_client_connections as f64);
        self.metrics.active_server_connections.set(proxy_metrics.active_server_connections as f64);
        self.metrics.messages_processed.inc_by(proxy_metrics.messages_processed as f64);
        self.metrics.messages_failed.inc_by(proxy_metrics.messages_failed as f64);
        self.metrics.bytes_transferred.inc_by(proxy_metrics.bytes_transferred as f64);
        self.metrics.connection_errors.inc_by(proxy_metrics.connection_errors as f64);
    }
    
    pub fn record_routing_metrics(&self, routing_metrics: RoutingMetricsSnapshot) {
        self.metrics.routes_calculated.inc_by(routing_metrics.routes_calculated as f64);
        self.metrics.routing_cache_hits.inc_by(routing_metrics.cache_hits as f64);
        self.metrics.routing_cache_misses.inc_by(routing_metrics.cache_misses as f64);
        self.metrics.position_lookups.inc_by(routing_metrics.position_lookups as f64);
    }
    
    pub fn record_transition_metrics(&self, transition_metrics: TransitionMetricsSnapshot) {
        self.metrics.transitions_initiated.inc_by(transition_metrics.transitions_initiated as f64);
        self.metrics.transitions_completed.inc_by(transition_metrics.transitions_completed as f64);
        self.metrics.transitions_failed.inc_by(transition_metrics.transitions_failed as f64);
        
        if transition_metrics.average_transition_time_ms > 0 {
            self.metrics.transition_duration.observe(transition_metrics.average_transition_time_ms as f64 / 1000.0);
        }
    }
    
    pub fn record_health_summary(&self, health_summary: ClusterHealthSummary) {
        self.metrics.healthy_servers.set(health_summary.healthy_servers as f64);
        self.metrics.failed_servers.set(health_summary.failed_servers as f64);
    }
    
    pub fn record_system_metrics(&self, system_metrics: SystemMetrics) {
        self.metrics.cpu_usage.set(system_metrics.cpu_usage_percent as f64);
        self.metrics.memory_usage.set(system_metrics.memory_usage_bytes as f64);
        self.metrics.disk_usage.set(system_metrics.disk_usage_bytes as f64);
        self.metrics.network_bytes_sent.inc_by(system_metrics.network_bytes_sent as f64);
        self.metrics.network_bytes_received.inc_by(system_metrics.network_bytes_received as f64);
    }
    
    pub fn record_request_duration(&self, endpoint: &str, method: &str, duration_seconds: f64) {
        self.metrics.request_duration
            .with_label_values(&[endpoint, method])
            .observe(duration_seconds);
    }
    
    pub fn record_server_response_time(&self, server_id: &str, response_time_ms: f64) {
        self.metrics.server_response_times
            .with_label_values(&[server_id])
            .observe(response_time_ms / 1000.0);
    }
    
    pub fn record_server_availability(&self, server_id: &str, availability: f64) {
        self.metrics.server_availability
            .with_label_values(&[server_id])
            .set(availability);
    }
    
    pub fn record_error_rate(&self, component: &str, error_rate: f64) {
        self.metrics.error_rate
            .with_label_values(&[component])
            .set(error_rate);
    }
    
    pub fn record_throughput(&self, component: &str, throughput: f64) {
        self.metrics.throughput
            .with_label_values(&[component])
            .set(throughput);
    }
    
    pub async fn get_prometheus_metrics(&self) -> Result<String> {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        
        encoder.encode_to_string(&metric_families)
            .map_err(|e| AtlasError::Internal(format!("Failed to encode metrics: {}", e)))
    }
    
    pub async fn create_snapshot(
        &self,
        proxy_metrics: ProxyMetricsSnapshot,
        routing_metrics: RoutingMetricsSnapshot,
        transition_metrics: TransitionMetricsSnapshot,
        health_summary: ClusterHealthSummary,
    ) -> MetricsSnapshot {
        let system_metrics = self.collect_system_metrics().await;
        
        MetricsSnapshot {
            timestamp: Utc::now(),
            proxy_metrics,
            routing_metrics,
            transition_metrics,
            health_summary,
            system_metrics,
            custom_metrics: HashMap::new(),
        }
    }
    
    fn create_exporters(config: &MetricsConfig) -> Result<Vec<Box<dyn MetricsExporter>>> {
        let mut exporters: Vec<Box<dyn MetricsExporter>> = Vec::new();
        
        for exporter_config in &config.exporters {
            match exporter_config {
                MetricsExporterConfig::Prometheus { endpoint } => {
                    exporters.push(Box::new(PrometheusExporter::new(endpoint.clone())));
                }
                MetricsExporterConfig::InfluxDb { endpoint, database } => {
                    exporters.push(Box::new(InfluxDbExporter::new(endpoint.clone(), database.clone())));
                }
                MetricsExporterConfig::DataDog { api_key } => {
                    exporters.push(Box::new(DataDogExporter::new(api_key.clone())));
                }
            }
        }
        
        Ok(exporters)
    }
    
    async fn start_collection_task(&mut self) {
        let metrics = Arc::clone(&self.metrics);
        let collection_interval = self.config.collection_interval;
        
        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_millis(collection_interval));
            
            loop {
                interval.tick().await;
                
                let system_metrics = Self::collect_system_metrics_static().await;
                metrics.cpu_usage.set(system_metrics.cpu_usage_percent as f64);
                metrics.memory_usage.set(system_metrics.memory_usage_bytes as f64);
                metrics.disk_usage.set(system_metrics.disk_usage_bytes as f64);
                
                debug!("Collected system metrics");
            }
        });
        
        self.collection_tasks.push(task);
    }
    
    async fn start_export_task(&mut self) {
        let exporters = std::mem::take(&mut self.exporters);
        let registry = Arc::clone(&self.registry);
        let collection_interval = self.config.collection_interval;
        
        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_millis(collection_interval * 5));
            
            loop {
                interval.tick().await;
                
                let snapshot = MetricsSnapshot {
                    timestamp: Utc::now(),
                    proxy_metrics: ProxyMetricsSnapshot {
                        active_client_connections: 0,
                        active_server_connections: 0,
                        messages_processed: 0,
                        messages_failed: 0,
                        bytes_transferred: 0,
                        connection_errors: 0,
                    },
                    routing_metrics: RoutingMetricsSnapshot {
                        routes_calculated: 0,
                        cache_hits: 0,
                        cache_misses: 0,
                        position_lookups: 0,
                        routing_errors: 0,
                        active_client_sessions: 0,
                        cached_routes: 0,
                    },
                    transition_metrics: TransitionMetricsSnapshot {
                        transitions_initiated: 0,
                        transitions_completed: 0,
                        transitions_failed: 0,
                        transitions_rolled_back: 0,
                        active_transitions: 0,
                        average_transition_time_ms: 0,
                    },
                    health_summary: ClusterHealthSummary::default(),
                    system_metrics: Self::collect_system_metrics_static().await,
                    custom_metrics: HashMap::new(),
                };
                
                for exporter in &exporters {
                    if let Err(e) = exporter.export(&snapshot) {
                        error!("Failed to export metrics via {}: {}", exporter.name(), e);
                    }
                }
            }
        });
        
        self.collection_tasks.push(task);
    }
    
    async fn start_http_server(&mut self) -> Result<()> {
        use axum::{routing::get, Router, response::IntoResponse};
        use tower_http::cors::CorsLayer;
        
        let registry = Arc::clone(&self.registry);
        let bind_address = self.config.bind_address;
        
        let app = Router::new()
            .route("/metrics", get(move || async move {
                let encoder = TextEncoder::new();
                let metric_families = registry.gather();
                
                match encoder.encode_to_string(&metric_families) {
                    Ok(output) => output.into_response(),
                    Err(e) => {
                        error!("Failed to encode metrics: {}", e);
                        "Error encoding metrics".into_response()
                    }
                }
            }))
            .route("/health", get(|| async { "OK" }))
            .layer(CorsLayer::permissive());
        
        let task = tokio::spawn(async move {
            if let Err(e) = axum::serve(
                tokio::net::TcpListener::bind(bind_address).await.unwrap(),
                app
            ).await {
                error!("Metrics HTTP server error: {}", e);
            }
        });
        
        self.http_server_task = Some(task);
        info!("Metrics HTTP server started on {}", bind_address);
        
        Ok(())
    }
    
    async fn collect_system_metrics(&self) -> SystemMetrics {
        Self::collect_system_metrics_static().await
    }
    
    async fn collect_system_metrics_static() -> SystemMetrics {
        let uptime = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        
        SystemMetrics {
            uptime_seconds: uptime,
            cpu_usage_percent: Self::get_cpu_usage().await,
            memory_usage_bytes: Self::get_memory_usage().await,
            memory_total_bytes: Self::get_total_memory().await,
            disk_usage_bytes: Self::get_disk_usage().await,
            disk_total_bytes: Self::get_total_disk().await,
            network_bytes_sent: 0,
            network_bytes_received: 0,
            open_file_descriptors: 0,
            goroutines_count: 0,
        }
    }
    
    async fn get_cpu_usage() -> f32 {
        0.0
    }
    
    async fn get_memory_usage() -> u64 {
        0
    }
    
    async fn get_total_memory() -> u64 {
        0
    }
    
    async fn get_disk_usage() -> u64 {
        0
    }
    
    async fn get_total_disk() -> u64 {
        0
    }
}

impl AtlasMetrics {
    pub fn new(registry: &Registry) -> Result<Self> {
        let active_client_connections = Gauge::new(
            "atlas_active_client_connections",
            "Number of active client connections"
        )?;
        registry.register(Box::new(active_client_connections.clone()))?;
        
        let active_server_connections = Gauge::new(
            "atlas_active_server_connections",
            "Number of active server connections"
        )?;
        registry.register(Box::new(active_server_connections.clone()))?;
        
        let total_connections = Counter::new(
            "atlas_total_connections",
            "Total number of connections established"
        )?;
        registry.register(Box::new(total_connections.clone()))?;
        
        let connection_errors = Counter::new(
            "atlas_connection_errors",
            "Number of connection errors"
        )?;
        registry.register(Box::new(connection_errors.clone()))?;
        
        let messages_processed = Counter::new(
            "atlas_messages_processed",
            "Number of messages processed"
        )?;
        registry.register(Box::new(messages_processed.clone()))?;
        
        let messages_failed = Counter::new(
            "atlas_messages_failed",
            "Number of messages that failed processing"
        )?;
        registry.register(Box::new(messages_failed.clone()))?;
        
        let message_latency = Histogram::with_opts(histogram_opts!(
            "atlas_message_latency_seconds",
            "Message processing latency in seconds",
            vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0]
        ))?;
        registry.register(Box::new(message_latency.clone()))?;
        
        let bytes_transferred = Counter::new(
            "atlas_bytes_transferred",
            "Total bytes transferred"
        )?;
        registry.register(Box::new(bytes_transferred.clone()))?;
        
        let routes_calculated = Counter::new(
            "atlas_routes_calculated",
            "Number of routes calculated"
        )?;
        registry.register(Box::new(routes_calculated.clone()))?;
        
        let routing_cache_hits = Counter::new(
            "atlas_routing_cache_hits",
            "Number of routing cache hits"
        )?;
        registry.register(Box::new(routing_cache_hits.clone()))?;
        
        let routing_cache_misses = Counter::new(
            "atlas_routing_cache_misses",
            "Number of routing cache misses"
        )?;
        registry.register(Box::new(routing_cache_misses.clone()))?;
        
        let position_lookups = Counter::new(
            "atlas_position_lookups",
            "Number of position-based server lookups"
        )?;
        registry.register(Box::new(position_lookups.clone()))?;
        
        let transitions_initiated = Counter::new(
            "atlas_transitions_initiated",
            "Number of transitions initiated"
        )?;
        registry.register(Box::new(transitions_initiated.clone()))?;
        
        let transitions_completed = Counter::new(
            "atlas_transitions_completed",
            "Number of transitions completed"
        )?;
        registry.register(Box::new(transitions_completed.clone()))?;
        
        let transitions_failed = Counter::new(
            "atlas_transitions_failed",
            "Number of transitions failed"
        )?;
        registry.register(Box::new(transitions_failed.clone()))?;
        
        let transition_duration = Histogram::with_opts(histogram_opts!(
            "atlas_transition_duration_seconds",
            "Transition duration in seconds",
            vec![0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]
        ))?;
        registry.register(Box::new(transition_duration.clone()))?;
        
        let healthy_servers = Gauge::new(
            "atlas_healthy_servers",
            "Number of healthy servers"
        )?;
        registry.register(Box::new(healthy_servers.clone()))?;
        
        let failed_servers = Gauge::new(
            "atlas_failed_servers",
            "Number of failed servers"
        )?;
        registry.register(Box::new(failed_servers.clone()))?;
        
        let server_response_times = HistogramVec::new(
            histogram_opts!(
                "atlas_server_response_time_seconds",
                "Server response times",
                vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0]
            ),
            &["server_id"]
        )?;
        registry.register(Box::new(server_response_times.clone()))?;
        
        let server_availability = GaugeVec::new(
            opts!("atlas_server_availability", "Server availability"),
            &["server_id"]
        )?;
        registry.register(Box::new(server_availability.clone()))?;
        
        let cluster_nodes = Gauge::new(
            "atlas_cluster_nodes",
            "Number of cluster nodes"
        )?;
        registry.register(Box::new(cluster_nodes.clone()))?;
        
        let cluster_leader_elections = Counter::new(
            "atlas_cluster_leader_elections",
            "Number of cluster leader elections"
        )?;
        registry.register(Box::new(cluster_leader_elections.clone()))?;
        
        let cluster_consensus_duration = Histogram::with_opts(histogram_opts!(
            "atlas_cluster_consensus_duration_seconds",
            "Cluster consensus duration",
            vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0]
        ))?;
        registry.register(Box::new(cluster_consensus_duration.clone()))?;
        
        let cpu_usage = Gauge::new(
            "atlas_cpu_usage_percent",
            "CPU usage percentage"
        )?;
        registry.register(Box::new(cpu_usage.clone()))?;
        
        let memory_usage = Gauge::new(
            "atlas_memory_usage_bytes",
            "Memory usage in bytes"
        )?;
        registry.register(Box::new(memory_usage.clone()))?;
        
        let disk_usage = Gauge::new(
            "atlas_disk_usage_bytes",
            "Disk usage in bytes"
        )?;
        registry.register(Box::new(disk_usage.clone()))?;
        
        let network_bytes_sent = Counter::new(
            "atlas_network_bytes_sent",
            "Total network bytes sent"
        )?;
        registry.register(Box::new(network_bytes_sent.clone()))?;
        
        let network_bytes_received = Counter::new(
            "atlas_network_bytes_received",
            "Total network bytes received"
        )?;
        registry.register(Box::new(network_bytes_received.clone()))?;
        
        let request_duration = HistogramVec::new(
            histogram_opts!(
                "atlas_request_duration_seconds",
                "Request duration in seconds",
                vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0]
            ),
            &["endpoint", "method"]
        )?;
        registry.register(Box::new(request_duration.clone()))?;
        
        let error_rate = GaugeVec::new(
            opts!("atlas_error_rate", "Error rate by component"),
            &["component"]
        )?;
        registry.register(Box::new(error_rate.clone()))?;
        
        let throughput = GaugeVec::new(
            opts!("atlas_throughput", "Throughput by component"),
            &["component"]
        )?;
        registry.register(Box::new(throughput.clone()))?;
        
        Ok(Self {
            active_client_connections,
            active_server_connections,
            total_connections,
            connection_errors,
            messages_processed,
            messages_failed,
            message_latency,
            bytes_transferred,
            routes_calculated,
            routing_cache_hits,
            routing_cache_misses,
            position_lookups,
            transitions_initiated,
            transitions_completed,
            transitions_failed,
            transition_duration,
            healthy_servers,
            failed_servers,
            server_response_times,
            server_availability,
            cluster_nodes,
            cluster_leader_elections,
            cluster_consensus_duration,
            cpu_usage,
            memory_usage,
            disk_usage,
            network_bytes_sent,
            network_bytes_received,
            request_duration,
            error_rate,
            throughput,
        })
    }
}

pub struct PrometheusExporter {
    endpoint: String,
    client: reqwest::Client,
}

impl PrometheusExporter {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            client: reqwest::Client::new(),
        }
    }
}

impl MetricsExporter for PrometheusExporter {
    fn export(&self, _metrics: &MetricsSnapshot) -> Result<()> {
        Ok(())
    }
    
    fn name(&self) -> &str {
        "prometheus"
    }
}

pub struct InfluxDbExporter {
    endpoint: String,
    database: String,
    client: reqwest::Client,
}

impl InfluxDbExporter {
    pub fn new(endpoint: String, database: String) -> Self {
        Self {
            endpoint,
            database,
            client: reqwest::Client::new(),
        }
    }
}

impl MetricsExporter for InfluxDbExporter {
    fn export(&self, metrics: &MetricsSnapshot) -> Result<()> {
        let line_protocol = self.convert_to_line_protocol(metrics)?;
        
        tokio::spawn({
            let client = self.client.clone();
            let endpoint = format!("{}/write?db={}", self.endpoint, self.database);
            
            async move {
                if let Err(e) = client.post(&endpoint)
                    .body(line_protocol)
                    .send()
                    .await
                {
                    error!("Failed to send metrics to InfluxDB: {}", e);
                }
            }
        });
        
        Ok(())
    }
    
    fn name(&self) -> &str {
        "influxdb"
    }
}

impl InfluxDbExporter {
    fn convert_to_line_protocol(&self, metrics: &MetricsSnapshot) -> Result<String> {
        let timestamp = metrics.timestamp.timestamp_nanos_opt().unwrap_or(0);
        let mut lines = Vec::new();
        
        lines.push(format!(
            "proxy_metrics active_client_connections={}i,active_server_connections={}i,messages_processed={}i {}",
            metrics.proxy_metrics.active_client_connections,
            metrics.proxy_metrics.active_server_connections,
            metrics.proxy_metrics.messages_processed,
            timestamp
        ));
        
        lines.push(format!(
            "system_metrics cpu_usage={:.2},memory_usage={},disk_usage={} {}",
            metrics.system_metrics.cpu_usage_percent,
            metrics.system_metrics.memory_usage_bytes,
            metrics.system_metrics.disk_usage_bytes,
            timestamp
        ));
        
        Ok(lines.join("\n"))
    }
}

pub struct DataDogExporter {
    api_key: String,
    client: reqwest::Client,
}

impl DataDogExporter {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
        }
    }
}

impl MetricsExporter for DataDogExporter {
    fn export(&self, metrics: &MetricsSnapshot) -> Result<()> {
        let datadog_metrics = self.convert_to_datadog_format(metrics)?;
        
        tokio::spawn({
            let client = self.client.clone();
            let api_key = self.api_key.clone();
            
            async move {
                if let Err(e) = client
                    .post("https://api.datadoghq.com/api/v1/series")
                    .header("DD-API-KEY", &api_key)
                    .header("Content-Type", "application/json")
                    .json(&datadog_metrics)
                    .send()
                    .await
                {
                    error!("Failed to send metrics to DataDog: {}", e);
                }
            }
        });
        
        Ok(())
    }
    
    fn name(&self) -> &str {
        "datadog"
    }
}

impl DataDogExporter {
    fn convert_to_datadog_format(&self, metrics: &MetricsSnapshot) -> Result<serde_json::Value> {
        let timestamp = metrics.timestamp.timestamp();
        
        let series = vec![
            serde_json::json!({
                "metric": "atlas.proxy.active_client_connections",
                "points": [[timestamp, metrics.proxy_metrics.active_client_connections]],
                "type": "gauge",
                "tags": ["service:atlas"]
            }),
            serde_json::json!({
                "metric": "atlas.system.cpu_usage",
                "points": [[timestamp, metrics.system_metrics.cpu_usage_percent]],
                "type": "gauge",
                "tags": ["service:atlas"]
            }),
        ];
        
        Ok(serde_json::json!({ "series": series }))
    }
}