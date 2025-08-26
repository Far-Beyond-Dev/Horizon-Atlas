use crate::config::HealthConfig;
use crate::discovery::DiscoveryManager;
use crate::errors::{AtlasError, Result};
use crate::types::{ServerId, HealthReport, NodeStatus, GameServer};
use serde::{Serialize, Deserialize};
use chrono::{DateTime, Utc, Duration as ChronoDuration};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, atomic::{AtomicBool, AtomicU32, Ordering}};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{debug, info, error, warn, instrument};

pub struct HealthMonitor {
    config: HealthConfig,
    discovery: Arc<DiscoveryManager>,
    server_health: Arc<DashMap<ServerId, ServerHealth>>,
    health_history: Arc<RwLock<HashMap<ServerId, VecDeque<HealthReport>>>>,
    alert_manager: AlertManager,
    health_checks_running: Arc<AtomicBool>,
    monitoring_tasks: Vec<tokio::task::JoinHandle<()>>,
}

#[derive(Debug, Clone)]
pub struct ServerHealth {
    pub server_id: ServerId,
    pub status: NodeStatus,
    pub last_check: DateTime<Utc>,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    pub response_time_ms: f32,
    pub error_rate: f32,
    pub availability: f32,
    pub last_failure_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HealthMetrics {
    pub timestamp: DateTime<Utc>,
    pub cpu_usage: f32,
    pub memory_usage: f32,
    pub network_usage: f32,
    pub active_connections: u32,
    pub message_throughput: f32,
    pub error_rate: f32,
    pub response_time_p50: f32,
    pub response_time_p95: f32,
    pub response_time_p99: f32,
}

pub struct AlertManager {
    alert_rules: Arc<RwLock<Vec<AlertRule>>>,
    active_alerts: Arc<DashMap<String, Alert>>,
    alert_history: Arc<Mutex<VecDeque<Alert>>>,
    notification_channels: Arc<RwLock<Vec<Box<dyn NotificationChannel>>>>,
}

#[derive(Debug, Clone)]
pub struct AlertRule {
    pub id: String,
    pub name: String,
    pub condition: AlertCondition,
    pub severity: AlertSeverity,
    pub threshold: f32,
    pub duration: Duration,
    pub cooldown: Duration,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub enum AlertCondition {
    HighCpuUsage,
    HighMemoryUsage,
    HighErrorRate,
    HighResponseTime,
    LowAvailability,
    ServerDown,
    NetworkPartition,
    DiskSpaceRunning,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AlertSeverity {
    Critical,
    Warning,
    Info,
}

#[derive(Debug, Clone)]
pub struct Alert {
    pub id: String,
    pub rule_id: String,
    pub server_id: Option<ServerId>,
    pub severity: AlertSeverity,
    pub title: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub status: AlertStatus,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AlertStatus {
    Active,
    Resolved,
    Suppressed,
}

pub trait NotificationChannel: Send + Sync {
    fn send_alert(&self, alert: &Alert) -> Result<()>;
    fn name(&self) -> &str;
}

impl HealthMonitor {
    pub fn new(config: HealthConfig, discovery: Arc<DiscoveryManager>) -> Self {
        let alert_manager = AlertManager::new();
        
        Self {
            config,
            discovery,
            server_health: Arc::new(DashMap::new()),
            health_history: Arc::new(RwLock::new(HashMap::new())),
            alert_manager,
            health_checks_running: Arc::new(AtomicBool::new(false)),
            monitoring_tasks: Vec::new(),
        }
    }
    
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting health monitor");
        
        self.health_checks_running.store(true, Ordering::SeqCst);
        
        self.setup_default_alert_rules().await;
        
        self.start_health_check_task().await;
        self.start_metrics_collection_task().await;
        self.start_alert_evaluation_task().await;
        self.start_cleanup_task().await;
        
        Ok(())
    }
    
    pub async fn stop(&mut self) {
        info!("Stopping health monitor");
        
        self.health_checks_running.store(false, Ordering::SeqCst);
        
        for task in self.monitoring_tasks.drain(..) {
            task.abort();
        }
        
        info!("Health monitor stopped");
    }
    
    #[instrument(skip(self), fields(server_id = %server_id))]
    pub async fn check_server_health(&self, server_id: &ServerId) -> Result<ServerHealth> {
        let server = self.discovery.get_server(server_id).await?
            .ok_or_else(|| AtlasError::ServerNotFound { server_id: server_id.0.clone() })?;
        
        let start_time = Instant::now();
        let check_result = self.perform_health_check(&server).await;
        let response_time = start_time.elapsed().as_millis() as f32;
        
        let mut health = self.server_health.entry(server_id.clone()).or_insert_with(|| {
            ServerHealth {
                server_id: server_id.clone(),
                status: NodeStatus::Starting,
                last_check: Utc::now(),
                consecutive_failures: 0,
                consecutive_successes: 0,
                response_time_ms: 0.0,
                error_rate: 0.0,
                availability: 1.0,
                last_failure_reason: None,
            }
        });
        
        health.last_check = Utc::now();
        health.response_time_ms = response_time;
        
        match check_result {
            Ok(metrics) => {
                health.consecutive_failures = 0;
                health.consecutive_successes += 1;
                health.error_rate = metrics.error_rate;
                
                if health.consecutive_successes >= self.config.success_threshold {
                    health.status = NodeStatus::Active;
                    health.last_failure_reason = None;
                }
                
                self.update_availability(&mut health);
                self.record_health_metrics(server_id, &metrics).await;
                
                debug!("Health check passed for server {}", server_id);
            }
            Err(e) => {
                health.consecutive_successes = 0;
                health.consecutive_failures += 1;
                health.last_failure_reason = Some(e.to_string());
                
                if health.consecutive_failures >= self.config.failure_threshold {
                    health.status = NodeStatus::Failed;
                }
                
                self.update_availability(&mut health);
                
                warn!("Health check failed for server {}: {}", server_id, e);
            }
        }
        
        Ok(health.clone())
    }
    
    pub async fn get_server_health(&self, server_id: &ServerId) -> Option<ServerHealth> {
        self.server_health.get(server_id).map(|h| h.clone())
    }
    
    pub async fn get_all_server_health(&self) -> HashMap<ServerId, ServerHealth> {
        self.server_health.iter().map(|entry| (entry.key().clone(), entry.value().clone())).collect()
    }
    
    pub async fn get_health_history(&self, server_id: &ServerId, limit: Option<usize>) -> Vec<HealthReport> {
        let history = self.health_history.read();
        if let Some(server_history) = history.get(server_id) {
            let limit = limit.unwrap_or(100);
            server_history.iter().rev().take(limit).cloned().collect()
        } else {
            Vec::new()
        }
    }
    
    pub async fn get_cluster_health_summary(&self) -> ClusterHealthSummary {
        let mut summary = ClusterHealthSummary::default();
        
        for health in self.server_health.iter() {
            summary.total_servers += 1;
            
            match health.status {
                NodeStatus::Active => summary.healthy_servers += 1,
                NodeStatus::Failed => summary.failed_servers += 1,
                NodeStatus::Inactive => summary.inactive_servers += 1,
                _ => summary.unknown_servers += 1,
            }
            
            summary.total_response_time += health.response_time_ms;
            summary.total_error_rate += health.error_rate;
            summary.total_availability += health.availability;
        }
        
        if summary.total_servers > 0 {
            summary.average_response_time = summary.total_response_time / summary.total_servers as f32;
            summary.average_error_rate = summary.total_error_rate / summary.total_servers as f32;
            summary.average_availability = summary.total_availability / summary.total_servers as f32;
            summary.cluster_health_score = summary.average_availability * (1.0 - summary.average_error_rate);
        }
        
        summary
    }
    
    pub fn add_alert_rule(&self, rule: AlertRule) {
        self.alert_manager.add_alert_rule(rule);
    }
    
    pub fn remove_alert_rule(&self, rule_id: &str) {
        self.alert_manager.remove_alert_rule(rule_id);
    }
    
    pub fn get_active_alerts(&self) -> Vec<Alert> {
        self.alert_manager.get_active_alerts()
    }
    
    pub fn add_notification_channel(&self, channel: Box<dyn NotificationChannel>) {
        self.alert_manager.add_notification_channel(channel);
    }
    
    async fn perform_health_check(&self, server: &GameServer) -> Result<HealthMetrics> {
        let client = reqwest::Client::new();
        let health_url = format!("http://{}/health", server.address);
        
        let response = tokio::time::timeout(
            Duration::from_millis(self.config.timeout),
            client.get(&health_url).send()
        ).await
        .map_err(|_| AtlasError::HealthCheck("Request timeout".to_string()))?
        .map_err(|e| AtlasError::HealthCheck(format!("HTTP request failed: {}", e)))?;
        
        if !response.status().is_success() {
            return Err(AtlasError::HealthCheck(
                format!("HTTP {} from {}", response.status(), health_url)
            ));
        }
        
        let health_data: serde_json::Value = response.json().await
            .map_err(|e| AtlasError::HealthCheck(format!("Failed to parse health response: {}", e)))?;
        
        let metrics = HealthMetrics {
            timestamp: Utc::now(),
            cpu_usage: health_data.get("cpu_usage").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            memory_usage: health_data.get("memory_usage").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            network_usage: health_data.get("network_usage").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            active_connections: health_data.get("active_connections").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            message_throughput: health_data.get("message_throughput").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            error_rate: health_data.get("error_rate").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            response_time_p50: health_data.get("response_time_p50").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            response_time_p95: health_data.get("response_time_p95").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            response_time_p99: health_data.get("response_time_p99").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        };
        
        Ok(metrics)
    }
    
    fn update_availability(&self, health: &mut ServerHealth) {
        const AVAILABILITY_WINDOW: f32 = 100.0;
        
        let failure_rate = health.consecutive_failures as f32 / AVAILABILITY_WINDOW;
        health.availability = (1.0 - failure_rate).max(0.0).min(1.0);
    }
    
    async fn record_health_metrics(&self, server_id: &ServerId, metrics: &HealthMetrics) {
        let report = HealthReport {
            node_id: server_id.0.clone(),
            timestamp: metrics.timestamp,
            status: NodeStatus::Active,
            cpu_usage: metrics.cpu_usage,
            memory_usage: metrics.memory_usage,
            network_usage: metrics.network_usage,
            active_connections: metrics.active_connections,
            message_throughput: metrics.message_throughput,
            error_rate: metrics.error_rate,
            latency_p95: metrics.response_time_p95,
        };
        
        let mut history = self.health_history.write();
        let server_history = history.entry(server_id.clone()).or_insert_with(VecDeque::new);
        
        server_history.push_back(report);
        
        let retention_limit = (self.config.metrics_retention / self.config.check_interval) as usize;
        while server_history.len() > retention_limit {
            server_history.pop_front();
        }
    }
    
    async fn setup_default_alert_rules(&self) {
        let rules = vec![
            AlertRule {
                id: "high_cpu".to_string(),
                name: "High CPU Usage".to_string(),
                condition: AlertCondition::HighCpuUsage,
                severity: AlertSeverity::Warning,
                threshold: 80.0,
                duration: Duration::from_secs(300),
                cooldown: Duration::from_secs(600),
                enabled: true,
            },
            AlertRule {
                id: "high_memory".to_string(),
                name: "High Memory Usage".to_string(),
                condition: AlertCondition::HighMemoryUsage,
                severity: AlertSeverity::Warning,
                threshold: 85.0,
                duration: Duration::from_secs(300),
                cooldown: Duration::from_secs(600),
                enabled: true,
            },
            AlertRule {
                id: "high_error_rate".to_string(),
                name: "High Error Rate".to_string(),
                condition: AlertCondition::HighErrorRate,
                severity: AlertSeverity::Critical,
                threshold: 5.0,
                duration: Duration::from_secs(60),
                cooldown: Duration::from_secs(300),
                enabled: true,
            },
            AlertRule {
                id: "server_down".to_string(),
                name: "Server Down".to_string(),
                condition: AlertCondition::ServerDown,
                severity: AlertSeverity::Critical,
                threshold: 1.0,
                duration: Duration::from_secs(30),
                cooldown: Duration::from_secs(60),
                enabled: true,
            },
        ];
        
        for rule in rules {
            self.add_alert_rule(rule);
        }
    }
    
    async fn start_health_check_task(&mut self) {
        let discovery = Arc::clone(&self.discovery);
        let server_health = Arc::clone(&self.server_health);
        let config = self.config.clone();
        let health_checks_running = Arc::clone(&self.health_checks_running);
        
        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_millis(config.check_interval));
            
            while health_checks_running.load(Ordering::SeqCst) {
                interval.tick().await;
                
                let servers = discovery.get_all_servers().await;
                
                for server in servers {
                    if health_checks_running.load(Ordering::SeqCst) {
                        let server_id = server.id;
                        let discovery_clone = Arc::clone(&discovery);
                        let server_health_clone = Arc::clone(&server_health);
                        let config_clone = config.clone();
                        
                        tokio::spawn(async move {
                            let health_monitor = HealthMonitor {
                                config: config_clone,
                                discovery: discovery_clone,
                                server_health: server_health_clone,
                                health_history: Arc::new(RwLock::new(HashMap::new())),
                                alert_manager: AlertManager::new(),
                                health_checks_running: Arc::new(AtomicBool::new(true)),
                                monitoring_tasks: Vec::new(),
                            };
                            
                            if let Err(e) = health_monitor.check_server_health(&server_id).await {
                                debug!("Health check error for server {}: {}", server_id, e);
                            }
                        });
                    }
                }
            }
        });
        
        self.monitoring_tasks.push(task);
    }
    
    async fn start_metrics_collection_task(&mut self) {
        let server_health = Arc::clone(&self.server_health);
        let health_checks_running = Arc::clone(&self.health_checks_running);
        
        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(60));
            
            while health_checks_running.load(Ordering::SeqCst) {
                interval.tick().await;
                
                debug!("Collecting health metrics for {} servers", server_health.len());
            }
        });
        
        self.monitoring_tasks.push(task);
    }
    
    async fn start_alert_evaluation_task(&mut self) {
        let server_health = Arc::clone(&self.server_health);
        let alert_manager = self.alert_manager.clone();
        let health_checks_running = Arc::clone(&self.health_checks_running);
        
        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(30));
            
            while health_checks_running.load(Ordering::SeqCst) {
                interval.tick().await;
                
                for health_entry in server_health.iter() {
                    alert_manager.evaluate_alerts(&health_entry.key(), &health_entry.value()).await;
                }
            }
        });
        
        self.monitoring_tasks.push(task);
    }
    
    async fn start_cleanup_task(&mut self) {
        let health_history = Arc::clone(&self.health_history);
        let config = self.config.clone();
        let health_checks_running = Arc::clone(&self.health_checks_running);
        
        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(3600));
            
            while health_checks_running.load(Ordering::SeqCst) {
                interval.tick().await;
                
                let retention_cutoff = Utc::now() - ChronoDuration::milliseconds(config.metrics_retention as i64);
                let mut history = health_history.write();
                
                for server_history in history.values_mut() {
                    server_history.retain(|report| report.timestamp > retention_cutoff);
                }
                
                debug!("Cleaned up old health metrics");
            }
        });
        
        self.monitoring_tasks.push(task);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClusterHealthSummary {
    pub total_servers: u32,
    pub healthy_servers: u32,
    pub failed_servers: u32,
    pub inactive_servers: u32,
    pub unknown_servers: u32,
    pub average_response_time: f32,
    pub average_error_rate: f32,
    pub average_availability: f32,
    pub cluster_health_score: f32,
    pub total_response_time: f32,
    pub total_error_rate: f32,
    pub total_availability: f32,
}

impl AlertManager {
    pub fn new() -> Self {
        Self {
            alert_rules: Arc::new(RwLock::new(Vec::new())),
            active_alerts: Arc::new(DashMap::new()),
            alert_history: Arc::new(Mutex::new(VecDeque::new())),
            notification_channels: Arc::new(RwLock::new(Vec::new())),
        }
    }
    
    pub fn add_alert_rule(&self, rule: AlertRule) {
        let mut rules = self.alert_rules.write();
        rules.push(rule);
    }
    
    pub fn remove_alert_rule(&self, rule_id: &str) {
        let mut rules = self.alert_rules.write();
        rules.retain(|r| r.id != rule_id);
    }
    
    pub fn get_active_alerts(&self) -> Vec<Alert> {
        self.active_alerts.iter().map(|entry| entry.value().clone()).collect()
    }
    
    pub fn add_notification_channel(&self, channel: Box<dyn NotificationChannel>) {
        let mut channels = self.notification_channels.write();
        channels.push(channel);
    }
    
    pub async fn evaluate_alerts(&self, server_id: &ServerId, health: &ServerHealth) {
        let rules = self.alert_rules.read().clone();
        
        for rule in &rules {
            if !rule.enabled {
                continue;
            }
            
            let should_alert = match rule.condition {
                AlertCondition::HighCpuUsage => false,
                AlertCondition::HighMemoryUsage => false,
                AlertCondition::HighErrorRate => health.error_rate > rule.threshold,
                AlertCondition::HighResponseTime => health.response_time_ms > rule.threshold,
                AlertCondition::LowAvailability => health.availability < rule.threshold,
                AlertCondition::ServerDown => matches!(health.status, NodeStatus::Failed),
                _ => false,
            };
            
            let alert_key = format!("{}:{}", rule.id, server_id.0);
            
            if should_alert {
                if !self.active_alerts.contains_key(&alert_key) {
                    let alert = Alert {
                        id: uuid::Uuid::new_v4().to_string(),
                        rule_id: rule.id.clone(),
                        server_id: Some(server_id.clone()),
                        severity: rule.severity.clone(),
                        title: rule.name.clone(),
                        message: self.format_alert_message(&rule, server_id, health),
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                        resolved_at: None,
                        status: AlertStatus::Active,
                        metadata: HashMap::new(),
                    };
                    
                    self.active_alerts.insert(alert_key.clone(), alert.clone());
                    self.send_alert_notification(&alert).await;
                    info!("Alert created: {} for server {}", rule.name, server_id);
                }
            } else if let Some((_, mut alert)) = self.active_alerts.remove(&alert_key) {
                alert.status = AlertStatus::Resolved;
                alert.resolved_at = Some(Utc::now());
                alert.updated_at = Utc::now();
                
                self.send_alert_notification(&alert).await;
                info!("Alert resolved: {} for server {}", rule.name, server_id);
            }
        }
    }
    
    fn format_alert_message(&self, rule: &AlertRule, server_id: &ServerId, health: &ServerHealth) -> String {
        match rule.condition {
            AlertCondition::HighErrorRate => {
                format!("Server {} has high error rate: {:.2}% (threshold: {:.2}%)", 
                        server_id, health.error_rate, rule.threshold)
            }
            AlertCondition::HighResponseTime => {
                format!("Server {} has high response time: {:.2}ms (threshold: {:.2}ms)", 
                        server_id, health.response_time_ms, rule.threshold)
            }
            AlertCondition::LowAvailability => {
                format!("Server {} has low availability: {:.2}% (threshold: {:.2}%)", 
                        server_id, health.availability * 100.0, rule.threshold * 100.0)
            }
            AlertCondition::ServerDown => {
                format!("Server {} is down (status: {:?})", server_id, health.status)
            }
            _ => {
                format!("Alert {} triggered for server {}", rule.name, server_id)
            }
        }
    }
    
    async fn send_alert_notification(&self, alert: &Alert) {
        let channels = self.notification_channels.read();
        
        for channel in channels.iter() {
            if let Err(e) = channel.send_alert(alert) {
                error!("Failed to send alert via {}: {}", channel.name(), e);
            }
        }
    }
}

impl Clone for AlertManager {
    fn clone(&self) -> Self {
        Self {
            alert_rules: Arc::clone(&self.alert_rules),
            active_alerts: Arc::clone(&self.active_alerts),
            alert_history: Arc::clone(&self.alert_history),
            notification_channels: Arc::clone(&self.notification_channels),
        }
    }
}

pub struct LogNotificationChannel;

impl NotificationChannel for LogNotificationChannel {
    fn send_alert(&self, alert: &Alert) -> Result<()> {
        match alert.severity {
            AlertSeverity::Critical => error!("[ALERT] {}: {}", alert.title, alert.message),
            AlertSeverity::Warning => warn!("[ALERT] {}: {}", alert.title, alert.message),
            AlertSeverity::Info => info!("[ALERT] {}: {}", alert.title, alert.message),
        }
        Ok(())
    }
    
    fn name(&self) -> &str {
        "log"
    }
}