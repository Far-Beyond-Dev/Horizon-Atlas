use crate::config::ClusterConfig;
use crate::state::GameServerInfo;
use anyhow::Result;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{interval, Duration};
use tracing::{info, debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasNodeInfo {
    pub node_id: String,
    pub address: String,
    pub port: u16,
    pub status: NodeStatus,
    pub last_heartbeat: u64,
    pub managed_servers: Vec<String>,
    pub connection_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeStatus {
    Starting,
    Active,
    Draining,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterState {
    pub nodes: Vec<AtlasNodeInfo>,
    pub servers: Vec<GameServerInfo>,
    pub last_updated: u64,
}

pub struct ClusterManager {
    config: ClusterConfig,
    local_node: AtlasNodeInfo,
    peer_nodes: Arc<DashMap<String, AtlasNodeInfo>>,
    cluster_state: Arc<DashMap<String, ClusterState>>,
}

impl ClusterManager {
    pub fn new(config: ClusterConfig, address: String, port: u16) -> Self {
        let local_node = AtlasNodeInfo {
            node_id: config.node_id.clone(),
            address,
            port,
            status: NodeStatus::Starting,
            last_heartbeat: current_timestamp(),
            managed_servers: Vec::new(),
            connection_count: 0,
        };
        
        Self {
            config,
            local_node,
            peer_nodes: Arc::new(DashMap::new()),
            cluster_state: Arc::new(DashMap::new()),
        }
    }
    
    pub async fn start(&mut self) -> Result<()> {
        self.local_node.status = NodeStatus::Active;
        info!("Starting cluster manager for node: {}", self.config.node_id);
        
        self.start_discovery_loop().await;
        self.start_heartbeat_loop().await;
        
        Ok(())
    }
    
    pub async fn register_peer_node(&self, node_info: AtlasNodeInfo) -> Result<()> {
        debug!("Registering peer node: {}", node_info.node_id);
        self.peer_nodes.insert(node_info.node_id.clone(), node_info);
        Ok(())
    }
    
    pub async fn update_peer_heartbeat(&self, node_id: &str) -> Result<()> {
        if let Some(mut node) = self.peer_nodes.get_mut(node_id) {
            node.last_heartbeat = current_timestamp();
            node.status = NodeStatus::Active;
        }
        Ok(())
    }
    
    pub async fn get_cluster_nodes(&self) -> Vec<AtlasNodeInfo> {
        let mut nodes = vec![self.local_node.clone()];
        nodes.extend(self.peer_nodes.iter().map(|n| n.value().clone()));
        nodes
    }
    
    pub async fn get_active_nodes(&self) -> Vec<AtlasNodeInfo> {
        self.get_cluster_nodes()
            .await
            .into_iter()
            .filter(|n| matches!(n.status, NodeStatus::Active))
            .collect()
    }
    
    pub async fn update_local_connection_count(&mut self, count: usize) {
        self.local_node.connection_count = count;
        self.local_node.last_heartbeat = current_timestamp();
    }
    
    pub async fn update_managed_servers(&mut self, server_ids: Vec<String>) {
        self.local_node.managed_servers = server_ids;
    }
    
    pub async fn find_best_node_for_connection(&self) -> Option<AtlasNodeInfo> {
        let active_nodes = self.get_active_nodes().await;
        
        active_nodes
            .into_iter()
            .min_by_key(|node| node.connection_count)
    }
    
    pub async fn distribute_load(&self) -> Result<Vec<LoadBalancingAction>> {
        let nodes = self.get_active_nodes().await;
        let mut actions = Vec::new();
        
        if nodes.len() < 2 {
            return Ok(actions);
        }
        
        let total_connections: usize = nodes.iter().map(|n| n.connection_count).sum();
        let average_load = total_connections / nodes.len();
        let threshold = (average_load as f32 * 1.2) as usize;
        
        for node in &nodes {
            if node.connection_count > threshold {
                if let Some(target) = nodes.iter()
                    .filter(|n| n.node_id != node.node_id && n.connection_count < average_load)
                    .min_by_key(|n| n.connection_count) 
                {
                    actions.push(LoadBalancingAction::TransferConnections {
                        from_node: node.node_id.clone(),
                        to_node: target.node_id.clone(),
                        count: (node.connection_count - average_load) / 2,
                    });
                }
            }
        }
        
        Ok(actions)
    }
    
    pub async fn get_node_by_id(&self, node_id: &str) -> Option<AtlasNodeInfo> {
        if node_id == self.local_node.node_id {
            Some(self.local_node.clone())
        } else {
            self.peer_nodes.get(node_id).map(|n| n.value().clone())
        }
    }
    
    async fn start_discovery_loop(&self) {
        let _peer_nodes = Arc::clone(&self.peer_nodes);
        let discovery_interval = Duration::from_secs(self.config.discovery_interval);
        
        tokio::spawn(async move {
            let mut interval = interval(discovery_interval);
            
            loop {
                interval.tick().await;
                
                debug!("Running node discovery...");
            }
        });
    }
    
    async fn start_heartbeat_loop(&self) {
        let peer_nodes = Arc::clone(&self.peer_nodes);
        let health_check_interval = Duration::from_secs(self.config.health_check_interval);
        
        tokio::spawn(async move {
            let mut interval = interval(health_check_interval);
            
            loop {
                interval.tick().await;
                let current_time = current_timestamp();
                
                let mut failed_nodes = Vec::new();
                
                for node in peer_nodes.iter() {
                    let time_since_heartbeat = current_time - node.last_heartbeat;
                    
                    if time_since_heartbeat > 60 {
                        warn!(
                            "Node {} hasn't sent heartbeat for {} seconds",
                            node.node_id, time_since_heartbeat
                        );
                        failed_nodes.push(node.node_id.clone());
                    }
                }
                
                for node_id in failed_nodes {
                    if let Some(mut node) = peer_nodes.get_mut(&node_id) {
                        node.status = NodeStatus::Failed;
                    }
                }
            }
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LoadBalancingAction {
    TransferConnections {
        from_node: String,
        to_node: String,
        count: usize,
    },
    ScaleUp {
        region: String,
        target_nodes: usize,
    },
    ScaleDown {
        region: String,
        target_nodes: usize,
    },
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}