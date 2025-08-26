use crate::config::{ClusterConfig, ConsensusAlgorithm};
use crate::errors::{AtlasError, Result};
use crate::types::{ClusterNode, NodeRole, NodeStatus, ServerId, RegionId};
use async_trait::async_trait;
use chrono::{DateTime, Utc, Duration as ChronoDuration};
use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, BTreeMap};
use std::net::SocketAddr;
use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, AtomicU32, Ordering}};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, RwLock as TokioRwLock};
use tokio::time::{interval, timeout};
use tracing::{debug, info, error, warn, instrument};
use uuid::Uuid;

pub struct ClusterManager {
    config: ClusterConfig,
    node_id: String,
    local_node: Arc<RwLock<ClusterNode>>,
    cluster_nodes: Arc<DashMap<String, ClusterNode>>,
    consensus_engine: Arc<dyn ConsensusEngine>,
    leadership_state: Arc<TokioRwLock<LeadershipState>>,
    cluster_state: Arc<RwLock<ClusterState>>,
    network_layer: Arc<ClusterNetwork>,
    membership_manager: Arc<MembershipManager>,
    partition_detector: Arc<PartitionDetector>,
    cluster_tasks: Vec<tokio::task::JoinHandle<()>>,
    is_running: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub struct ClusterState {
    pub term: u64,
    pub leader_id: Option<String>,
    pub nodes: HashMap<String, ClusterNode>,
    pub regions: HashMap<RegionId, HashSet<String>>,
    pub partition_groups: Vec<HashSet<String>>,
    pub last_heartbeat: HashMap<String, DateTime<Utc>>,
    pub split_brain_detected: bool,
}

#[derive(Debug, Clone)]
pub struct LeadershipState {
    pub current_role: NodeRole,
    pub current_term: u64,
    pub voted_for: Option<String>,
    pub votes_received: HashSet<String>,
    pub last_heartbeat_sent: DateTime<Utc>,
    pub last_heartbeat_received: HashMap<String, DateTime<Utc>>,
    pub election_timeout: Duration,
    pub is_candidate: bool,
}

#[async_trait]
pub trait ConsensusEngine: Send + Sync {
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn propose(&self, proposal: ConsensusProposal) -> Result<ConsensusResult>;
    async fn handle_message(&self, message: ConsensusMessage) -> Result<()>;
    async fn get_current_state(&self) -> ConsensusState;
    fn get_algorithm(&self) -> ConsensusAlgorithm;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusProposal {
    pub id: String,
    pub proposer: String,
    pub term: u64,
    pub proposal_type: ProposalType,
    pub data: Vec<u8>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProposalType {
    NodeJoin(String, SocketAddr),
    NodeLeave(String),
    RegionAssignment(RegionId, String),
    ConfigUpdate(String),
    LeaderElection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusMessage {
    pub id: String,
    pub from: String,
    pub to: String,
    pub message_type: MessageType,
    pub term: u64,
    pub data: Vec<u8>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageType {
    RequestVote,
    RequestVoteResponse,
    AppendEntries,
    AppendEntriesResponse,
    Heartbeat,
    HeartbeatResponse,
    ProposeValue,
    PreparePhase1,
    PreparePhase2,
    AcceptPhase1,
    AcceptPhase2,
}

#[derive(Debug, Clone)]
pub struct ConsensusResult {
    pub accepted: bool,
    pub term: u64,
    pub committed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConsensusState {
    pub term: u64,
    pub role: NodeRole,
    pub leader: Option<String>,
    pub committed_index: u64,
    pub last_applied: u64,
}

pub struct ClusterNetwork {
    local_address: SocketAddr,
    connections: Arc<DashMap<String, NetworkConnection>>,
    message_handlers: Arc<DashMap<String, Box<dyn MessageHandler>>>,
    network_tasks: Vec<tokio::task::JoinHandle<()>>,
}

#[derive(Debug)]
struct NetworkConnection {
    node_id: String,
    address: SocketAddr,
    sender: mpsc::UnboundedSender<ConsensusMessage>,
    last_activity: Instant,
    connection_state: ConnectionState,
}

#[derive(Debug, Clone, PartialEq)]
enum ConnectionState {
    Connecting,
    Connected,
    Disconnected,
    Failed,
}

#[async_trait]
pub trait MessageHandler: Send + Sync {
    async fn handle_message(&self, message: ConsensusMessage) -> Result<()>;
}

pub struct MembershipManager {
    config: ClusterConfig,
    known_nodes: Arc<DashMap<String, ClusterNode>>,
    seed_connections: Vec<SocketAddr>,
    membership_protocol: Arc<dyn MembershipProtocol>,
}

#[async_trait]
pub trait MembershipProtocol: Send + Sync {
    async fn join_cluster(&self, node: &ClusterNode) -> Result<()>;
    async fn leave_cluster(&self, node_id: &str) -> Result<()>;
    async fn update_node_status(&self, node_id: &str, status: NodeStatus) -> Result<()>;
    async fn get_cluster_members(&self) -> Result<Vec<ClusterNode>>;
}

pub struct PartitionDetector {
    config: ClusterConfig,
    partition_history: Arc<Mutex<Vec<PartitionEvent>>>,
    current_partitions: Arc<RwLock<Vec<HashSet<String>>>>,
}

#[derive(Debug, Clone)]
struct PartitionEvent {
    timestamp: DateTime<Utc>,
    partition_groups: Vec<HashSet<String>>,
    duration: Option<Duration>,
    resolved: bool,
}

impl ClusterManager {
    pub async fn new(config: ClusterConfig) -> Result<Self> {
        let node_id = config.node_id.clone();
        let local_address = config.seed_nodes.first()
            .ok_or_else(|| AtlasError::Cluster("No seed nodes configured".to_string()))?;
        
        let local_node = Arc::new(RwLock::new(ClusterNode {
            id: node_id.clone(),
            address: *local_address,
            role: NodeRole::Follower,
            status: NodeStatus::Starting,
            last_seen: Utc::now(),
            load: 0.0,
            regions_managed: Vec::new(),
        }));
        
        let consensus_engine = create_consensus_engine(&config).await?;
        let network_layer = Arc::new(ClusterNetwork::new(*local_address).await?);
        let membership_manager = Arc::new(MembershipManager::new(config.clone()).await?);
        let partition_detector = Arc::new(PartitionDetector::new(config.clone()));
        
        let leadership_state = Arc::new(TokioRwLock::new(LeadershipState {
            current_role: NodeRole::Follower,
            current_term: 0,
            voted_for: None,
            votes_received: HashSet::new(),
            last_heartbeat_sent: Utc::now(),
            last_heartbeat_received: HashMap::new(),
            election_timeout: Duration::from_millis(config.election_timeout),
            is_candidate: false,
        }));
        
        let cluster_state = Arc::new(RwLock::new(ClusterState {
            term: 0,
            leader_id: None,
            nodes: HashMap::new(),
            regions: HashMap::new(),
            partition_groups: Vec::new(),
            last_heartbeat: HashMap::new(),
            split_brain_detected: false,
        }));
        
        Ok(Self {
            config,
            node_id,
            local_node,
            cluster_nodes: Arc::new(DashMap::new()),
            consensus_engine,
            leadership_state,
            cluster_state,
            network_layer,
            membership_manager,
            partition_detector,
            cluster_tasks: Vec::new(),
            is_running: Arc::new(AtomicBool::new(false)),
        })
    }
    
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting cluster manager for node {}", self.node_id);
        
        self.is_running.store(true, Ordering::SeqCst);
        
        {
            let mut local_node = self.local_node.write();
            local_node.status = NodeStatus::Starting;
        }
        
        self.network_layer.start().await?;
        self.consensus_engine.start().await?;
        
        self.start_heartbeat_task().await;
        self.start_election_task().await;
        self.start_membership_task().await;
        self.start_partition_detection_task().await;
        self.start_health_monitoring_task().await;
        
        self.join_cluster().await?;
        
        {
            let mut local_node = self.local_node.write();
            local_node.status = NodeStatus::Active;
        }
        
        info!("Cluster manager started successfully");
        Ok(())
    }
    
    pub async fn stop(&mut self) -> Result<()> {
        info!("Stopping cluster manager");
        
        self.is_running.store(false, Ordering::SeqCst);
        
        self.leave_cluster().await?;
        
        for task in self.cluster_tasks.drain(..) {
            task.abort();
        }
        
        self.consensus_engine.stop().await?;
        self.network_layer.stop().await?;
        
        {
            let mut local_node = self.local_node.write();
            local_node.status = NodeStatus::Stopping;
        }
        
        info!("Cluster manager stopped");
        Ok(())
    }
    
    pub async fn get_cluster_status(&self) -> ClusterStatus {
        let state = self.cluster_state.read();
        let leadership = self.leadership_state.read().await;
        let local_node = self.local_node.read();
        
        ClusterStatus {
            node_id: self.node_id.clone(),
            role: leadership.current_role.clone(),
            term: leadership.current_term,
            leader_id: state.leader_id.clone(),
            cluster_size: state.nodes.len(),
            healthy_nodes: state.nodes.values().filter(|n| n.status == NodeStatus::Active).count(),
            regions_managed: local_node.regions_managed.len(),
            split_brain_detected: state.split_brain_detected,
            partition_groups: state.partition_groups.len(),
        }
    }
    
    pub async fn propose_consensus(&self, proposal: ConsensusProposal) -> Result<ConsensusResult> {
        let leadership = self.leadership_state.read().await;
        
        if leadership.current_role != NodeRole::Leader {
            return Err(AtlasError::Cluster("Only leader can propose consensus".to_string()));
        }
        
        self.consensus_engine.propose(proposal).await
    }
    
    pub async fn assign_region(&self, region_id: RegionId, node_id: String) -> Result<()> {
        let proposal = ConsensusProposal {
            id: Uuid::new_v4().to_string(),
            proposer: self.node_id.clone(),
            term: self.leadership_state.read().await.current_term,
            proposal_type: ProposalType::RegionAssignment(region_id.clone(), node_id.clone()),
            data: vec![],
            timestamp: Utc::now(),
        };
        
        let result = self.propose_consensus(proposal).await?;
        
        if result.accepted && result.committed {
            let mut state = self.cluster_state.write();
            state.regions.entry(region_id)
                .or_insert_with(HashSet::new)
                .insert(node_id);
            info!("Region assignment committed successfully");
        } else {
            return Err(AtlasError::Cluster("Failed to commit region assignment".to_string()));
        }
        
        Ok(())
    }
    
    pub async fn get_region_assignments(&self) -> HashMap<RegionId, HashSet<String>> {
        self.cluster_state.read().regions.clone()
    }
    
    pub async fn is_leader(&self) -> bool {
        let leadership = self.leadership_state.read().await;
        leadership.current_role == NodeRole::Leader
    }
    
    pub async fn get_leader_id(&self) -> Option<String> {
        self.cluster_state.read().leader_id.clone()
    }
    
    pub async fn get_cluster_nodes(&self) -> Vec<ClusterNode> {
        self.cluster_nodes.iter().map(|entry| entry.value().clone()).collect()
    }
    
    async fn join_cluster(&self) -> Result<()> {
        info!("Joining cluster");
        
        let local_node = self.local_node.read().clone();
        self.membership_manager.join_cluster(&local_node).await?;
        
        let proposal = ConsensusProposal {
            id: Uuid::new_v4().to_string(),
            proposer: self.node_id.clone(),
            term: 0,
            proposal_type: ProposalType::NodeJoin(local_node.id.clone(), local_node.address),
            data: bincode::serialize(&local_node)?,
            timestamp: Utc::now(),
        };
        
        for seed_addr in &self.config.seed_nodes {
            if let Err(e) = self.network_layer.connect_to_node("seed".to_string(), *seed_addr).await {
                warn!("Failed to connect to seed node {}: {}", seed_addr, e);
            }
        }
        
        Ok(())
    }
    
    async fn leave_cluster(&self) -> Result<()> {
        info!("Leaving cluster");
        
        if self.is_leader().await {
            self.step_down_as_leader().await?;
        }
        
        let proposal = ConsensusProposal {
            id: Uuid::new_v4().to_string(),
            proposer: self.node_id.clone(),
            term: self.leadership_state.read().await.current_term,
            proposal_type: ProposalType::NodeLeave(self.node_id.clone()),
            data: vec![],
            timestamp: Utc::now(),
        };
        
        let _ = self.propose_consensus(proposal).await;
        self.membership_manager.leave_cluster(&self.node_id).await?;
        
        Ok(())
    }
    
    async fn step_down_as_leader(&self) -> Result<()> {
        info!("Stepping down as leader");
        
        let mut leadership = self.leadership_state.write().await;
        leadership.current_role = NodeRole::Follower;
        leadership.votes_received.clear();
        leadership.is_candidate = false;
        
        let mut state = self.cluster_state.write();
        state.leader_id = None;
        
        Ok(())
    }
    
    async fn start_heartbeat_task(&mut self) {
        let leadership_state = Arc::clone(&self.leadership_state);
        let cluster_nodes = Arc::clone(&self.cluster_nodes);
        let network_layer = Arc::clone(&self.network_layer);
        let node_id = self.node_id.clone();
        let heartbeat_interval = Duration::from_millis(self.config.heartbeat_interval);
        let is_running = Arc::clone(&self.is_running);
        
        let task = tokio::spawn(async move {
            let mut interval = interval(heartbeat_interval);
            
            while is_running.load(Ordering::SeqCst) {
                interval.tick().await;
                
                let leadership = leadership_state.read().await;
                
                if leadership.current_role == NodeRole::Leader {
                    let heartbeat_msg = ConsensusMessage {
                        id: Uuid::new_v4().to_string(),
                        from: node_id.clone(),
                        to: "all".to_string(),
                        message_type: MessageType::Heartbeat,
                        term: leadership.current_term,
                        data: vec![],
                        timestamp: Utc::now(),
                    };
                    
                    for node in cluster_nodes.iter() {
                        if node.key() != &node_id {
                            if let Err(e) = network_layer.send_message(node.key().clone(), heartbeat_msg.clone()).await {
                                debug!("Failed to send heartbeat to {}: {}", node.key(), e);
                            }
                        }
                    }
                }
            }
        });
        
        self.cluster_tasks.push(task);
    }
    
    async fn start_election_task(&mut self) {
        let leadership_state = Arc::clone(&self.leadership_state);
        let cluster_nodes = Arc::clone(&self.cluster_nodes);
        let cluster_state = Arc::clone(&self.cluster_state);
        let network_layer = Arc::clone(&self.network_layer);
        let node_id = self.node_id.clone();
        let election_timeout = Duration::from_millis(self.config.election_timeout);
        let is_running = Arc::clone(&self.is_running);
        
        let task = tokio::spawn(async move {
            let mut interval = interval(election_timeout / 2);
            
            while is_running.load(Ordering::SeqCst) {
                interval.tick().await;
                
                let should_start_election = {
                    let leadership = leadership_state.read().await;
                    let state = cluster_state.read();
                    
                    leadership.current_role == NodeRole::Follower &&
                    state.leader_id.is_none() &&
                    Utc::now().signed_duration_since(leadership.last_heartbeat_sent).num_milliseconds() > election_timeout.as_millis() as i64
                };
                
                if should_start_election {
                    Self::start_leader_election(
                        &leadership_state,
                        &cluster_nodes,
                        &cluster_state,
                        &network_layer,
                        &node_id,
                    ).await;
                }
            }
        });
        
        self.cluster_tasks.push(task);
    }
    
    async fn start_membership_task(&mut self) {
        let membership_manager = Arc::clone(&self.membership_manager);
        let cluster_nodes = Arc::clone(&self.cluster_nodes);
        let cluster_state = Arc::clone(&self.cluster_state);
        let is_running = Arc::clone(&self.is_running);
        
        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(10));
            
            while is_running.load(Ordering::SeqCst) {
                interval.tick().await;
                
                match membership_manager.get_cluster_members().await {
                    Ok(members) => {
                        cluster_nodes.clear();
                        let mut state = cluster_state.write();
                        state.nodes.clear();
                        
                        for member in members {
                            cluster_nodes.insert(member.id.clone(), member.clone());
                            state.nodes.insert(member.id.clone(), member);
                        }
                    }
                    Err(e) => {
                        error!("Failed to get cluster members: {}", e);
                    }
                }
            }
        });
        
        self.cluster_tasks.push(task);
    }
    
    async fn start_partition_detection_task(&mut self) {
        let partition_detector = Arc::clone(&self.partition_detector);
        let cluster_nodes = Arc::clone(&self.cluster_nodes);
        let cluster_state = Arc::clone(&self.cluster_state);
        let is_running = Arc::clone(&self.is_running);
        
        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(30));
            
            while is_running.load(Ordering::SeqCst) {
                interval.tick().await;
                
                let nodes: Vec<ClusterNode> = cluster_nodes.iter()
                    .map(|entry| entry.value().clone())
                    .collect();
                
                if let Ok(partitions) = partition_detector.detect_partitions(&nodes).await {
                    if partitions.len() > 1 {
                        warn!("Network partition detected: {} groups", partitions.len());
                        let mut state = cluster_state.write();
                        state.partition_groups = partitions;
                        state.split_brain_detected = true;
                    } else {
                        let mut state = cluster_state.write();
                        if state.split_brain_detected {
                            info!("Network partition resolved");
                            state.split_brain_detected = false;
                            state.partition_groups.clear();
                        }
                    }
                }
            }
        });
        
        self.cluster_tasks.push(task);
    }
    
    async fn start_health_monitoring_task(&mut self) {
        let cluster_nodes = Arc::clone(&self.cluster_nodes);
        let cluster_state = Arc::clone(&self.cluster_state);
        let is_running = Arc::clone(&self.is_running);
        
        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(5));
            
            while is_running.load(Ordering::SeqCst) {
                interval.tick().await;
                
                let now = Utc::now();
                let mut unhealthy_nodes = Vec::new();
                
                for mut node_entry in cluster_nodes.iter_mut() {
                    let node = node_entry.value_mut();
                    let time_since_seen = now.signed_duration_since(node.last_seen);
                    
                    if time_since_seen.num_seconds() > 30 {
                        if node.status != NodeStatus::Failed {
                            node.status = NodeStatus::Failed;
                            unhealthy_nodes.push(node.id.clone());
                        }
                    } else if node.status == NodeStatus::Failed && time_since_seen.num_seconds() < 10 {
                        node.status = NodeStatus::Active;
                    }
                }
                
                if !unhealthy_nodes.is_empty() {
                    warn!("Detected unhealthy nodes: {:?}", unhealthy_nodes);
                }
            }
        });
        
        self.cluster_tasks.push(task);
    }
    
    async fn start_leader_election(
        leadership_state: &Arc<TokioRwLock<LeadershipState>>,
        cluster_nodes: &Arc<DashMap<String, ClusterNode>>,
        cluster_state: &Arc<RwLock<ClusterState>>,
        network_layer: &Arc<ClusterNetwork>,
        node_id: &str,
    ) {
        info!("Starting leader election");
        
        let mut leadership = leadership_state.write().await;
        leadership.current_role = NodeRole::Candidate;
        leadership.current_term += 1;
        leadership.voted_for = Some(node_id.to_string());
        leadership.votes_received.clear();
        leadership.votes_received.insert(node_id.to_string());
        leadership.is_candidate = true;
        
        let vote_request = ConsensusMessage {
            id: Uuid::new_v4().to_string(),
            from: node_id.to_string(),
            to: "all".to_string(),
            message_type: MessageType::RequestVote,
            term: leadership.current_term,
            data: vec![],
            timestamp: Utc::now(),
        };
        
        for node in cluster_nodes.iter() {
            if node.key() != node_id {
                if let Err(e) = network_layer.send_message(node.key().clone(), vote_request.clone()).await {
                    debug!("Failed to send vote request to {}: {}", node.key(), e);
                }
            }
        }
        
        let majority = (cluster_nodes.len() / 2) + 1;
        if leadership.votes_received.len() >= majority {
            leadership.current_role = NodeRole::Leader;
            leadership.is_candidate = false;
            
            let mut state = cluster_state.write();
            state.leader_id = Some(node_id.to_string());
            state.term = leadership.current_term;
            
            info!("Elected as cluster leader for term {}", leadership.current_term);
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClusterStatus {
    pub node_id: String,
    pub role: NodeRole,
    pub term: u64,
    pub leader_id: Option<String>,
    pub cluster_size: usize,
    pub healthy_nodes: usize,
    pub regions_managed: usize,
    pub split_brain_detected: bool,
    pub partition_groups: usize,
}

async fn create_consensus_engine(config: &ClusterConfig) -> Result<Arc<dyn ConsensusEngine>> {
    match config.consensus_algorithm {
        ConsensusAlgorithm::Raft => {
            Ok(Arc::new(RaftConsensus::new(config.clone()).await?))
        }
        ConsensusAlgorithm::Pbft => {
            Ok(Arc::new(PbftConsensus::new(config.clone()).await?))
        }
    }
}

pub struct RaftConsensus {
    config: ClusterConfig,
    state: Arc<RwLock<RaftState>>,
    log: Arc<Mutex<Vec<LogEntry>>>,
    commit_index: Arc<AtomicU64>,
    last_applied: Arc<AtomicU64>,
}

#[derive(Debug, Clone)]
struct RaftState {
    current_term: u64,
    voted_for: Option<String>,
    role: NodeRole,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LogEntry {
    term: u64,
    index: u64,
    proposal: ConsensusProposal,
    committed: bool,
}

impl RaftConsensus {
    pub async fn new(config: ClusterConfig) -> Result<Self> {
        Ok(Self {
            config,
            state: Arc::new(RwLock::new(RaftState {
                current_term: 0,
                voted_for: None,
                role: NodeRole::Follower,
            })),
            log: Arc::new(Mutex::new(Vec::new())),
            commit_index: Arc::new(AtomicU64::new(0)),
            last_applied: Arc::new(AtomicU64::new(0)),
        })
    }
}

#[async_trait]
impl ConsensusEngine for RaftConsensus {
    async fn start(&self) -> Result<()> {
        info!("Starting Raft consensus engine");
        Ok(())
    }
    
    async fn stop(&self) -> Result<()> {
        info!("Stopping Raft consensus engine");
        Ok(())
    }
    
    async fn propose(&self, proposal: ConsensusProposal) -> Result<ConsensusResult> {
        let state = self.state.read();
        if state.role != NodeRole::Leader {
            return Ok(ConsensusResult {
                accepted: false,
                term: state.current_term,
                committed: false,
                error: Some("Not leader".to_string()),
            });
        }
        
        let mut log = self.log.lock().await;
        let log_entry = LogEntry {
            term: state.current_term,
            index: log.len() as u64,
            proposal,
            committed: false,
        };
        
        log.push(log_entry);
        
        Ok(ConsensusResult {
            accepted: true,
            term: state.current_term,
            committed: true,
            error: None,
        })
    }
    
    async fn handle_message(&self, _message: ConsensusMessage) -> Result<()> {
        Ok(())
    }
    
    async fn get_current_state(&self) -> ConsensusState {
        let state = self.state.read();
        ConsensusState {
            term: state.current_term,
            role: state.role.clone(),
            leader: None,
            committed_index: self.commit_index.load(Ordering::SeqCst),
            last_applied: self.last_applied.load(Ordering::SeqCst),
        }
    }
    
    fn get_algorithm(&self) -> ConsensusAlgorithm {
        ConsensusAlgorithm::Raft
    }
}

pub struct PbftConsensus {
    config: ClusterConfig,
    state: Arc<RwLock<PbftState>>,
    message_log: Arc<Mutex<Vec<ConsensusMessage>>>,
    view: Arc<AtomicU64>,
}

#[derive(Debug, Clone)]
struct PbftState {
    phase: PbftPhase,
    proposal: Option<ConsensusProposal>,
    prepare_votes: HashMap<String, bool>,
    commit_votes: HashMap<String, bool>,
}

#[derive(Debug, Clone, PartialEq)]
enum PbftPhase {
    PrePrepare,
    Prepare,
    Commit,
}

impl PbftConsensus {
    pub async fn new(config: ClusterConfig) -> Result<Self> {
        Ok(Self {
            config,
            state: Arc::new(RwLock::new(PbftState {
                phase: PbftPhase::PrePrepare,
                proposal: None,
                prepare_votes: HashMap::new(),
                commit_votes: HashMap::new(),
            })),
            message_log: Arc::new(Mutex::new(Vec::new())),
            view: Arc::new(AtomicU64::new(0)),
        })
    }
}

#[async_trait]
impl ConsensusEngine for PbftConsensus {
    async fn start(&self) -> Result<()> {
        info!("Starting PBFT consensus engine");
        Ok(())
    }
    
    async fn stop(&self) -> Result<()> {
        info!("Stopping PBFT consensus engine");
        Ok(())
    }
    
    async fn propose(&self, proposal: ConsensusProposal) -> Result<ConsensusResult> {
        let mut state = self.state.write();
        state.proposal = Some(proposal);
        state.phase = PbftPhase::PrePrepare;
        
        Ok(ConsensusResult {
            accepted: true,
            term: self.view.load(Ordering::SeqCst),
            committed: false,
            error: None,
        })
    }
    
    async fn handle_message(&self, message: ConsensusMessage) -> Result<()> {
        let mut message_log = self.message_log.lock().await;
        message_log.push(message);
        Ok(())
    }
    
    async fn get_current_state(&self) -> ConsensusState {
        ConsensusState {
            term: self.view.load(Ordering::SeqCst),
            role: NodeRole::Follower,
            leader: None,
            committed_index: 0,
            last_applied: 0,
        }
    }
    
    fn get_algorithm(&self) -> ConsensusAlgorithm {
        ConsensusAlgorithm::Pbft
    }
}

impl ClusterNetwork {
    pub async fn new(local_address: SocketAddr) -> Result<Self> {
        Ok(Self {
            local_address,
            connections: Arc::new(DashMap::new()),
            message_handlers: Arc::new(DashMap::new()),
            network_tasks: Vec::new(),
        })
    }
    
    pub async fn start(&self) -> Result<()> {
        info!("Starting cluster network on {}", self.local_address);
        Ok(())
    }
    
    pub async fn stop(&self) -> Result<()> {
        info!("Stopping cluster network");
        Ok(())
    }
    
    pub async fn connect_to_node(&self, node_id: String, address: SocketAddr) -> Result<()> {
        info!("Connecting to node {} at {}", node_id, address);
        
        let (tx, mut rx) = mpsc::unbounded_channel();
        
        let connection = NetworkConnection {
            node_id: node_id.clone(),
            address,
            sender: tx,
            last_activity: Instant::now(),
            connection_state: ConnectionState::Connected,
        };
        
        self.connections.insert(node_id, connection);
        Ok(())
    }
    
    pub async fn send_message(&self, node_id: String, message: ConsensusMessage) -> Result<()> {
        if let Some(connection) = self.connections.get(&node_id) {
            connection.sender.send(message)
                .map_err(|_| AtlasError::Cluster("Failed to send message".to_string()))?;
        }
        Ok(())
    }
}

impl MembershipManager {
    pub async fn new(config: ClusterConfig) -> Result<Self> {
        Ok(Self {
            config: config.clone(),
            known_nodes: Arc::new(DashMap::new()),
            seed_connections: config.seed_nodes,
            membership_protocol: Arc::new(GossipProtocol::new()),
        })
    }
}

#[async_trait]
impl MembershipProtocol for MembershipManager {
    async fn join_cluster(&self, node: &ClusterNode) -> Result<()> {
        self.known_nodes.insert(node.id.clone(), node.clone());
        Ok(())
    }
    
    async fn leave_cluster(&self, node_id: &str) -> Result<()> {
        self.known_nodes.remove(node_id);
        Ok(())
    }
    
    async fn update_node_status(&self, node_id: &str, status: NodeStatus) -> Result<()> {
        if let Some(mut node) = self.known_nodes.get_mut(node_id) {
            node.status = status;
        }
        Ok(())
    }
    
    async fn get_cluster_members(&self) -> Result<Vec<ClusterNode>> {
        Ok(self.known_nodes.iter().map(|entry| entry.value().clone()).collect())
    }
}

pub struct GossipProtocol;

impl GossipProtocol {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl MembershipProtocol for GossipProtocol {
    async fn join_cluster(&self, _node: &ClusterNode) -> Result<()> {
        Ok(())
    }
    
    async fn leave_cluster(&self, _node_id: &str) -> Result<()> {
        Ok(())
    }
    
    async fn update_node_status(&self, _node_id: &str, _status: NodeStatus) -> Result<()> {
        Ok(())
    }
    
    async fn get_cluster_members(&self) -> Result<Vec<ClusterNode>> {
        Ok(Vec::new())
    }
}

impl PartitionDetector {
    pub fn new(config: ClusterConfig) -> Self {
        Self {
            config,
            partition_history: Arc::new(Mutex::new(Vec::new())),
            current_partitions: Arc::new(RwLock::new(Vec::new())),
        }
    }
    
    pub async fn detect_partitions(&self, nodes: &[ClusterNode]) -> Result<Vec<HashSet<String>>> {
        let mut partitions = Vec::new();
        let mut visited = HashSet::new();
        
        for node in nodes {
            if !visited.contains(&node.id) && node.status == NodeStatus::Active {
                let mut partition = HashSet::new();
                self.find_connected_nodes(&node.id, nodes, &mut partition, &mut visited);
                
                if !partition.is_empty() {
                    partitions.push(partition);
                }
            }
        }
        
        if partitions.is_empty() {
            partitions.push(nodes.iter().map(|n| n.id.clone()).collect());
        }
        
        Ok(partitions)
    }
    
    fn find_connected_nodes(
        &self,
        node_id: &str,
        all_nodes: &[ClusterNode],
        partition: &mut HashSet<String>,
        visited: &mut HashSet<String>,
    ) {
        if visited.contains(node_id) {
            return;
        }
        
        visited.insert(node_id.to_string());
        partition.insert(node_id.to_string());
        
        for node in all_nodes {
            if node.status == NodeStatus::Active && !visited.contains(&node.id) {
                self.find_connected_nodes(&node.id, all_nodes, partition, visited);
            }
        }
    }
}