use crate::errors::{AtlasError, Result};
use crate::proxy::ProxyManager;
use crate::routing::RoutingManager;
use crate::types::{
    ClientId, ServerId, RegionId, Position, TransitionRequest, Message,
    MessageSource, MessageDestination, ClientConnection
};
use async_trait::async_trait;
use chrono::{DateTime, Utc, Duration as ChronoDuration};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, atomic::{AtomicU32, Ordering}};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tokio::time::{timeout, interval};
use tracing::{debug, info, error, warn, instrument};
use uuid::Uuid;

#[async_trait]
pub trait TransitionHandler: Send + Sync {
    async fn prepare_transition(&self, request: &TransitionRequest) -> Result<TransitionContext>;
    async fn execute_transition(&self, context: TransitionContext) -> Result<()>;
    async fn rollback_transition(&self, context: &TransitionContext, reason: String) -> Result<()>;
    async fn confirm_transition(&self, transition_id: &str) -> Result<()>;
}

pub struct TransitionManager {
    proxy: Arc<ProxyManager>,
    routing: Arc<RoutingManager>,
    active_transitions: Arc<DashMap<String, ActiveTransition>>,
    transition_handlers: Arc<DashMap<TransitionType, Arc<dyn TransitionHandler>>>,
    transition_queue: Arc<Mutex<mpsc::UnboundedReceiver<TransitionRequest>>>,
    transition_sender: mpsc::UnboundedSender<TransitionRequest>,
    metrics: TransitionMetrics,
    cleanup_task: Option<tokio::task::JoinHandle<()>>,
    processing_task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Debug, Clone)]
pub struct ActiveTransition {
    pub id: String,
    pub request: TransitionRequest,
    pub state: TransitionState,
    pub context: Option<TransitionContext>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub timeout_at: DateTime<Utc>,
    pub retry_count: u32,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TransitionState {
    Pending,
    Preparing,
    Prepared,
    Executing,
    Completed,
    Failed,
    RolledBack,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub enum TransitionType {
    RegionChange,
    ServerSwitch,
    LoadBalancing,
    Emergency,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionContext {
    pub transition_id: String,
    pub transition_type: TransitionType,
    pub client_id: ClientId,
    pub from_server: ServerId,
    pub to_server: ServerId,
    pub from_region: RegionId,
    pub to_region: RegionId,
    pub client_state: Vec<u8>,
    pub position: Position,
    pub metadata: HashMap<String, String>,
    pub prepared_at: DateTime<Utc>,
    pub deadline: DateTime<Utc>,
}

#[derive(Debug, Default)]
struct TransitionMetrics {
    transitions_initiated: AtomicU32,
    transitions_completed: AtomicU32,
    transitions_failed: AtomicU32,
    transitions_rolled_back: AtomicU32,
    active_transitions: AtomicU32,
    average_transition_time_ms: AtomicU32,
}

impl TransitionManager {
    pub fn new(
        proxy: Arc<ProxyManager>,
        routing: Arc<RoutingManager>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        
        Self {
            proxy,
            routing,
            active_transitions: Arc::new(DashMap::new()),
            transition_handlers: Arc::new(DashMap::new()),
            transition_queue: Arc::new(Mutex::new(rx)),
            transition_sender: tx,
            metrics: TransitionMetrics::default(),
            cleanup_task: None,
            processing_task: None,
        }
    }
    
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting transition manager");
        
        self.register_default_handlers().await?;
        self.start_processing_task().await;
        self.start_cleanup_task().await;
        
        Ok(())
    }
    
    pub async fn stop(&mut self) {
        if let Some(task) = self.processing_task.take() {
            task.abort();
        }
        
        if let Some(task) = self.cleanup_task.take() {
            task.abort();
        }
        
        for mut transition in self.active_transitions.iter_mut() {
            if matches!(transition.state, TransitionState::Preparing | TransitionState::Executing) {
                transition.state = TransitionState::Cancelled;
                transition.updated_at = Utc::now();
            }
        }
        
        info!("Transition manager stopped");
    }
    
    #[instrument(skip(self), fields(client_id = %client_id, from_region = %from_region, to_region = %to_region))]
    pub async fn initiate_transition(
        &self,
        client_id: ClientId,
        from_server: ServerId,
        to_server: ServerId,
        from_region: RegionId,
        to_region: RegionId,
        position: Position,
        state_data: Vec<u8>,
    ) -> Result<String> {
        let transition_id = Uuid::new_v4().to_string();
        
        let request = TransitionRequest {
            client_id: client_id.clone(),
            from_server,
            to_server,
            from_region: from_region.clone(),
            to_region: to_region.clone(),
            position,
            state_data,
            timestamp: Utc::now(),
        };
        
        let active_transition = ActiveTransition {
            id: transition_id.clone(),
            request: request.clone(),
            state: TransitionState::Pending,
            context: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            timeout_at: Utc::now() + ChronoDuration::seconds(30),
            retry_count: 0,
            error_message: None,
        };
        
        self.active_transitions.insert(transition_id.clone(), active_transition);
        self.metrics.transitions_initiated.fetch_add(1, Ordering::SeqCst);
        self.metrics.active_transitions.fetch_add(1, Ordering::SeqCst);
        
        if let Err(e) = self.transition_sender.send(request) {
            error!("Failed to queue transition request: {}", e);
            self.active_transitions.remove(&transition_id);
            self.metrics.active_transitions.fetch_sub(1, Ordering::SeqCst);
            return Err(AtlasError::Transition(format!("Failed to queue transition: {}", e)));
        }
        
        info!("Initiated transition {} for client {} from region {} to region {}",
              transition_id, client_id.0, from_region, to_region);
        
        Ok(transition_id)
    }
    
    pub async fn get_transition_status(&self, transition_id: &str) -> Result<TransitionState> {
        self.active_transitions
            .get(transition_id)
            .map(|t| t.state.clone())
            .ok_or_else(|| AtlasError::Transition(format!("Transition {} not found", transition_id)))
    }
    
    pub async fn cancel_transition(&self, transition_id: &str) -> Result<()> {
        if let Some(mut transition) = self.active_transitions.get_mut(transition_id) {
            match transition.state {
                TransitionState::Pending | TransitionState::Preparing => {
                    transition.state = TransitionState::Cancelled;
                    transition.updated_at = Utc::now();
                    info!("Cancelled transition {}", transition_id);
                    Ok(())
                }
                TransitionState::Executing => {
                    if let Some(context) = &transition.context {
                        if let Some(handler) = self.get_handler(&context.transition_type) {
                            handler.rollback_transition(context, "Transition cancelled by user".to_string()).await?;
                        }
                    }
                    transition.state = TransitionState::Cancelled;
                    transition.updated_at = Utc::now();
                    info!("Cancelled executing transition {}", transition_id);
                    Ok(())
                }
                _ => {
                    Err(AtlasError::Transition(format!("Cannot cancel transition {} in state {:?}", 
                                                     transition_id, transition.state)))
                }
            }
        } else {
            Err(AtlasError::Transition(format!("Transition {} not found", transition_id)))
        }
    }
    
    pub fn register_handler(&self, transition_type: TransitionType, handler: Arc<dyn TransitionHandler>) {
        self.transition_handlers.insert(transition_type, handler);
    }
    
    pub fn get_metrics(&self) -> TransitionMetricsSnapshot {
        TransitionMetricsSnapshot {
            transitions_initiated: self.metrics.transitions_initiated.load(Ordering::SeqCst),
            transitions_completed: self.metrics.transitions_completed.load(Ordering::SeqCst),
            transitions_failed: self.metrics.transitions_failed.load(Ordering::SeqCst),
            transitions_rolled_back: self.metrics.transitions_rolled_back.load(Ordering::SeqCst),
            active_transitions: self.metrics.active_transitions.load(Ordering::SeqCst),
            average_transition_time_ms: self.metrics.average_transition_time_ms.load(Ordering::SeqCst),
        }
    }
    
    async fn register_default_handlers(&self) -> Result<()> {
        let region_handler = Arc::new(RegionTransitionHandler::new(
            Arc::clone(&self.proxy),
            Arc::clone(&self.routing),
        ));
        
        let server_handler = Arc::new(ServerTransitionHandler::new(
            Arc::clone(&self.proxy),
            Arc::clone(&self.routing),
        ));
        
        self.register_handler(TransitionType::RegionChange, region_handler);
        self.register_handler(TransitionType::ServerSwitch, server_handler.clone());
        self.register_handler(TransitionType::LoadBalancing, server_handler.clone());
        self.register_handler(TransitionType::Emergency, server_handler);
        
        Ok(())
    }
    
    async fn start_processing_task(&mut self) {
        let active_transitions = Arc::clone(&self.active_transitions);
        let transition_handlers = Arc::clone(&self.transition_handlers);
        let transition_queue = Arc::clone(&self.transition_queue);
        let metrics = self.metrics.clone();
        
        let task = tokio::spawn(async move {
            let mut queue = transition_queue.lock().await;
            
            while let Some(request) = queue.recv().await {
                let transition_id = Self::find_transition_id(&active_transitions, &request).await;
                
                if let Some(id) = transition_id {
                    if let Some(result) = Self::process_transition(
                        &id,
                        &active_transitions,
                        &transition_handlers,
                        &metrics,
                    ).await {
                        if let Err(e) = result {
                            error!("Failed to process transition {}: {}", id, e);
                        }
                    }
                }
            }
        });
        
        self.processing_task = Some(task);
    }
    
    async fn start_cleanup_task(&mut self) {
        let active_transitions = Arc::clone(&self.active_transitions);
        let metrics = self.metrics.clone();
        
        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(30));
            
            loop {
                interval.tick().await;
                
                let now = Utc::now();
                let mut completed_transitions = Vec::new();
                
                for entry in active_transitions.iter() {
                    let transition = entry.value();
                    
                    let should_cleanup = match transition.state {
                        TransitionState::Completed | TransitionState::Failed | 
                        TransitionState::RolledBack | TransitionState::Cancelled => {
                            now.signed_duration_since(transition.updated_at).num_minutes() > 5
                        }
                        _ => {
                            now > transition.timeout_at
                        }
                    };
                    
                    if should_cleanup {
                        completed_transitions.push(transition.id.clone());
                    }
                }
                
                for transition_id in completed_transitions {
                    active_transitions.remove(&transition_id);
                    metrics.active_transitions.fetch_sub(1, Ordering::SeqCst);
                    debug!("Cleaned up transition {}", transition_id);
                }
            }
        });
        
        self.cleanup_task = Some(task);
    }
    
    async fn find_transition_id(
        active_transitions: &DashMap<String, ActiveTransition>,
        request: &TransitionRequest,
    ) -> Option<String> {
        active_transitions.iter()
            .find(|entry| {
                let transition = entry.value();
                transition.request.client_id == request.client_id &&
                transition.request.timestamp == request.timestamp
            })
            .map(|entry| entry.key().clone())
    }
    
    async fn process_transition(
        transition_id: &str,
        active_transitions: &DashMap<String, ActiveTransition>,
        handlers: &DashMap<TransitionType, Arc<dyn TransitionHandler>>,
        metrics: &TransitionMetrics,
    ) -> Option<Result<()>> {
        let mut transition = active_transitions.get_mut(transition_id)?;
        
        if transition.state != TransitionState::Pending {
            return None;
        }
        
        let start_time = Instant::now();
        
        transition.state = TransitionState::Preparing;
        transition.updated_at = Utc::now();
        
        let transition_type = Self::determine_transition_type(&transition.request);
        let handler = handlers.get(&transition_type)?.clone();
        
        let prepare_result = timeout(
            Duration::from_secs(10),
            handler.prepare_transition(&transition.request)
        ).await;
        
        match prepare_result {
            Ok(Ok(context)) => {
                transition.state = TransitionState::Prepared;
                transition.context = Some(context.clone());
                transition.updated_at = Utc::now();
                
                drop(transition);
                
                let execute_result = timeout(
                    Duration::from_secs(15),
                    handler.execute_transition(context.clone())
                ).await;
                
                let mut transition = active_transitions.get_mut(transition_id)?;
                
                match execute_result {
                    Ok(Ok(())) => {
                        transition.state = TransitionState::Executing;
                        transition.updated_at = Utc::now();
                        
                        drop(transition);
                        
                        if let Err(e) = handler.confirm_transition(transition_id).await {
                            error!("Failed to confirm transition {}: {}", transition_id, e);
                        }
                        
                        let mut transition = active_transitions.get_mut(transition_id)?;
                        transition.state = TransitionState::Completed;
                        transition.updated_at = Utc::now();
                        
                        metrics.transitions_completed.fetch_add(1, Ordering::SeqCst);
                        
                        let duration = start_time.elapsed().as_millis() as u32;
                        metrics.average_transition_time_ms.store(duration, Ordering::SeqCst);
                        
                        info!("Completed transition {} in {}ms", transition_id, duration);
                        
                        Some(Ok(()))
                    }
                    Ok(Err(e)) => {
                        let error_msg = e.to_string();
                        
                        transition.state = TransitionState::Failed;
                        transition.error_message = Some(error_msg.clone());
                        transition.updated_at = Utc::now();
                        
                        if let Some(ctx) = &transition.context {
                            if let Err(rollback_err) = handler.rollback_transition(ctx, error_msg.clone()).await {
                                error!("Failed to rollback transition {}: {}", transition_id, rollback_err);
                            } else {
                                transition.state = TransitionState::RolledBack;
                                metrics.transitions_rolled_back.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                        
                        metrics.transitions_failed.fetch_add(1, Ordering::SeqCst);
                        error!("Failed transition {}: {}", transition_id, error_msg);
                        
                        Some(Err(AtlasError::Transition(error_msg)))
                    }
                    Err(_) => {
                        let error_msg = "Execution timeout".to_string();
                        
                        transition.state = TransitionState::Failed;
                        transition.error_message = Some(error_msg.clone());
                        transition.updated_at = Utc::now();
                        
                        if let Some(ctx) = &transition.context {
                            if let Err(rollback_err) = handler.rollback_transition(ctx, error_msg.clone()).await {
                                error!("Failed to rollback transition {}: {}", transition_id, rollback_err);
                            } else {
                                transition.state = TransitionState::RolledBack;
                                metrics.transitions_rolled_back.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                        
                        metrics.transitions_failed.fetch_add(1, Ordering::SeqCst);
                        error!("Failed transition {}: {}", transition_id, error_msg);
                        
                        Some(Err(AtlasError::Transition(error_msg)))
                    }
                }
            }
            Ok(Err(e)) => {
                let error_msg = e.to_string();
                
                transition.state = TransitionState::Failed;
                transition.error_message = Some(error_msg.clone());
                transition.updated_at = Utc::now();
                
                metrics.transitions_failed.fetch_add(1, Ordering::SeqCst);
                error!("Failed to prepare transition {}: {}", transition_id, error_msg);
                
                Some(Err(AtlasError::Transition(error_msg)))
            }
            Err(_) => {
                let error_msg = "Preparation timeout".to_string();
                
                transition.state = TransitionState::Failed;
                transition.error_message = Some(error_msg.clone());
                transition.updated_at = Utc::now();
                
                metrics.transitions_failed.fetch_add(1, Ordering::SeqCst);
                error!("Failed to prepare transition {}: {}", transition_id, error_msg);
                
                Some(Err(AtlasError::Transition(error_msg)))
            }
        }
    }
    
    fn determine_transition_type(request: &TransitionRequest) -> TransitionType {
        if request.from_region != request.to_region {
            TransitionType::RegionChange
        } else {
            TransitionType::ServerSwitch
        }
    }
    
    fn get_handler(&self, transition_type: &TransitionType) -> Option<Arc<dyn TransitionHandler>> {
        self.transition_handlers.get(transition_type).map(|h| h.clone())
    }
}

impl Clone for TransitionMetrics {
    fn clone(&self) -> Self {
        Self {
            transitions_initiated: AtomicU32::new(self.transitions_initiated.load(Ordering::SeqCst)),
            transitions_completed: AtomicU32::new(self.transitions_completed.load(Ordering::SeqCst)),
            transitions_failed: AtomicU32::new(self.transitions_failed.load(Ordering::SeqCst)),
            transitions_rolled_back: AtomicU32::new(self.transitions_rolled_back.load(Ordering::SeqCst)),
            active_transitions: AtomicU32::new(self.active_transitions.load(Ordering::SeqCst)),
            average_transition_time_ms: AtomicU32::new(self.average_transition_time_ms.load(Ordering::SeqCst)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionMetricsSnapshot {
    pub transitions_initiated: u32,
    pub transitions_completed: u32,
    pub transitions_failed: u32,
    pub transitions_rolled_back: u32,
    pub active_transitions: u32,
    pub average_transition_time_ms: u32,
}

pub struct RegionTransitionHandler {
    proxy: Arc<ProxyManager>,
    routing: Arc<RoutingManager>,
}

impl RegionTransitionHandler {
    pub fn new(proxy: Arc<ProxyManager>, routing: Arc<RoutingManager>) -> Self {
        Self { proxy, routing }
    }
}

#[async_trait]
impl TransitionHandler for RegionTransitionHandler {
    async fn prepare_transition(&self, request: &TransitionRequest) -> Result<TransitionContext> {
        debug!("Preparing region transition for client {}", request.client_id.0);
        
        let transition_id = Uuid::new_v4().to_string();
        
        let context = TransitionContext {
            transition_id: transition_id.clone(),
            transition_type: TransitionType::RegionChange,
            client_id: request.client_id.clone(),
            from_server: request.from_server.clone(),
            to_server: request.to_server.clone(),
            from_region: request.from_region.clone(),
            to_region: request.to_region.clone(),
            client_state: request.state_data.clone(),
            position: request.position.clone(),
            metadata: HashMap::new(),
            prepared_at: Utc::now(),
            deadline: Utc::now() + ChronoDuration::seconds(30),
        };
        
        let prepare_msg = Message {
            id: Uuid::new_v4(),
            from: MessageSource::Atlas("transition-manager".to_string()),
            to: MessageDestination::Server(request.to_server.clone()),
            message_type: "prepare_client_transfer".to_string(),
            payload: bincode::serialize(&context)?,
            timestamp: Utc::now(),
            encrypted: false,
            compressed: false,
        };
        
        self.proxy.send_to_server(&request.to_server, 
            tokio_tungstenite::tungstenite::Message::Binary(
                bincode::serialize(&prepare_msg)?
            )).await?;
        
        Ok(context)
    }
    
    async fn execute_transition(&self, context: TransitionContext) -> Result<()> {
        debug!("Executing region transition for client {}", context.client_id.0);
        
        let transfer_msg = Message {
            id: Uuid::new_v4(),
            from: MessageSource::Atlas("transition-manager".to_string()),
            to: MessageDestination::Server(context.from_server.clone()),
            message_type: "transfer_client".to_string(),
            payload: bincode::serialize(&context)?,
            timestamp: Utc::now(),
            encrypted: false,
            compressed: false,
        };
        
        self.proxy.send_to_server(&context.from_server,
            tokio_tungstenite::tungstenite::Message::Binary(
                bincode::serialize(&transfer_msg)?
            )).await?;
        
        let accept_msg = Message {
            id: Uuid::new_v4(),
            from: MessageSource::Atlas("transition-manager".to_string()),
            to: MessageDestination::Server(context.to_server.clone()),
            message_type: "accept_client".to_string(),
            payload: bincode::serialize(&context)?,
            timestamp: Utc::now(),
            encrypted: false,
            compressed: false,
        };
        
        self.proxy.send_to_server(&context.to_server,
            tokio_tungstenite::tungstenite::Message::Binary(
                bincode::serialize(&accept_msg)?
            )).await?;
        
        self.routing.remove_client(&context.client_id).await;
        
        Ok(())
    }
    
    async fn rollback_transition(&self, context: &TransitionContext, reason: String) -> Result<()> {
        warn!("Rolling back region transition for client {}: {}", context.client_id.0, reason);
        
        let rollback_msg = Message {
            id: Uuid::new_v4(),
            from: MessageSource::Atlas("transition-manager".to_string()),
            to: MessageDestination::Server(context.to_server.clone()),
            message_type: "rollback_client_transfer".to_string(),
            payload: bincode::serialize(&(context, reason))?,
            timestamp: Utc::now(),
            encrypted: false,
            compressed: false,
        };
        
        self.proxy.send_to_server(&context.to_server,
            tokio_tungstenite::tungstenite::Message::Binary(
                bincode::serialize(&rollback_msg)?
            )).await?;
        
        Ok(())
    }
    
    async fn confirm_transition(&self, transition_id: &str) -> Result<()> {
        debug!("Confirming region transition {}", transition_id);
        Ok(())
    }
}

pub struct ServerTransitionHandler {
    proxy: Arc<ProxyManager>,
    routing: Arc<RoutingManager>,
}

impl ServerTransitionHandler {
    pub fn new(proxy: Arc<ProxyManager>, routing: Arc<RoutingManager>) -> Self {
        Self { proxy, routing }
    }
}

#[async_trait]
impl TransitionHandler for ServerTransitionHandler {
    async fn prepare_transition(&self, request: &TransitionRequest) -> Result<TransitionContext> {
        debug!("Preparing server transition for client {}", request.client_id.0);
        
        let transition_id = Uuid::new_v4().to_string();
        
        let context = TransitionContext {
            transition_id: transition_id.clone(),
            transition_type: TransitionType::ServerSwitch,
            client_id: request.client_id.clone(),
            from_server: request.from_server.clone(),
            to_server: request.to_server.clone(),
            from_region: request.from_region.clone(),
            to_region: request.to_region.clone(),
            client_state: request.state_data.clone(),
            position: request.position.clone(),
            metadata: HashMap::new(),
            prepared_at: Utc::now(),
            deadline: Utc::now() + ChronoDuration::seconds(15),
        };
        
        let prepare_msg = Message {
            id: Uuid::new_v4(),
            from: MessageSource::Atlas("transition-manager".to_string()),
            to: MessageDestination::Server(request.to_server.clone()),
            message_type: "prepare_server_switch".to_string(),
            payload: bincode::serialize(&context)?,
            timestamp: Utc::now(),
            encrypted: false,
            compressed: false,
        };
        
        self.proxy.send_to_server(&request.to_server,
            tokio_tungstenite::tungstenite::Message::Binary(
                bincode::serialize(&prepare_msg)?
            )).await?;
        
        Ok(context)
    }
    
    async fn execute_transition(&self, context: TransitionContext) -> Result<()> {
        debug!("Executing server transition for client {}", context.client_id.0);
        
        let switch_msg = Message {
            id: Uuid::new_v4(),
            from: MessageSource::Atlas("transition-manager".to_string()),
            to: MessageDestination::Server(context.to_server.clone()),
            message_type: "execute_server_switch".to_string(),
            payload: bincode::serialize(&context)?,
            timestamp: Utc::now(),
            encrypted: false,
            compressed: false,
        };
        
        self.proxy.send_to_server(&context.to_server,
            tokio_tungstenite::tungstenite::Message::Binary(
                bincode::serialize(&switch_msg)?
            )).await?;
        
        Ok(())
    }
    
    async fn rollback_transition(&self, context: &TransitionContext, reason: String) -> Result<()> {
        warn!("Rolling back server transition for client {}: {}", context.client_id.0, reason);
        
        let rollback_msg = Message {
            id: Uuid::new_v4(),
            from: MessageSource::Atlas("transition-manager".to_string()),
            to: MessageDestination::Server(context.to_server.clone()),
            message_type: "rollback_server_switch".to_string(),
            payload: bincode::serialize(&(context, reason))?,
            timestamp: Utc::now(),
            encrypted: false,
            compressed: false,
        };
        
        self.proxy.send_to_server(&context.to_server,
            tokio_tungstenite::tungstenite::Message::Binary(
                bincode::serialize(&rollback_msg)?
            )).await?;
        
        Ok(())
    }
    
    async fn confirm_transition(&self, transition_id: &str) -> Result<()> {
        debug!("Confirming server transition {}", transition_id);
        Ok(())
    }
}