use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use crate::error::{ProxyError, Result};

/// 3D world coordinates using f64 for precision
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WorldCoordinate {
    pub x: f64,
    pub y: f64, 
    pub z: f64,
}

impl WorldCoordinate {
    pub fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }
    
    /// Calculate 3D distance to another coordinate
    pub fn distance_to(&self, other: &WorldCoordinate) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        let dz = self.z - other.z;
        (dx * dx + dy * dy + dz * dz).sqrt()
    }
    
    /// Calculate 3D vector to another coordinate
    pub fn vector_to(&self, other: &WorldCoordinate) -> WorldCoordinate {
        WorldCoordinate {
            x: other.x - self.x,
            y: other.y - self.y,
            z: other.z - self.z,
        }
    }
    
    /// Calculate magnitude (length) of this coordinate as a vector
    pub fn magnitude(&self) -> f64 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }
    
    /// Normalize this coordinate as a unit vector
    pub fn normalized(&self) -> WorldCoordinate {
        let mag = self.magnitude();
        if mag == 0.0 {
            WorldCoordinate::new(0.0, 0.0, 0.0)
        } else {
            WorldCoordinate {
                x: self.x / mag,
                y: self.y / mag,
                z: self.z / mag,
            }
        }
    }
    
    /// Add another coordinate (vector addition)
    pub fn add(&self, other: &WorldCoordinate) -> WorldCoordinate {
        WorldCoordinate {
            x: self.x + other.x,
            y: self.y + other.y,
            z: self.z + other.z,
        }
    }
    
    /// Scale coordinate by a factor
    pub fn scale(&self, factor: f64) -> WorldCoordinate {
        WorldCoordinate {
            x: self.x * factor,
            y: self.y * factor,
            z: self.z * factor,
        }
    }
}

/// Server region coordinates (i64 for grid-based regions)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RegionCoordinate {
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

impl RegionCoordinate {
    pub fn new(x: i64, y: i64, z: i64) -> Self {
        Self { x, y, z }
    }
    
    /// Center region coordinate (0, 0, 0)
    pub fn center() -> Self {
        Self::new(0, 0, 0)
    }
    
    /// Calculate Manhattan distance to another region
    pub fn manhattan_distance(&self, other: &RegionCoordinate) -> i64 {
        (self.x - other.x).abs() + (self.y - other.y).abs() + (self.z - other.z).abs()
    }
    
    /// Get adjacent region coordinates (6 directions in 3D)
    pub fn adjacent_regions(&self) -> Vec<RegionCoordinate> {
        vec![
            RegionCoordinate::new(self.x + 1, self.y, self.z),
            RegionCoordinate::new(self.x - 1, self.y, self.z),
            RegionCoordinate::new(self.x, self.y + 1, self.z),
            RegionCoordinate::new(self.x, self.y - 1, self.z),
            RegionCoordinate::new(self.x, self.y, self.z + 1),
            RegionCoordinate::new(self.x, self.y, self.z - 1),
        ]
    }
}

/// Server region boundary definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerRegion {
    /// Server identifier
    pub server_id: String,
    /// Region coordinate in the world grid
    pub region_coord: RegionCoordinate,
    /// Center point of this region in world coordinates
    pub center: WorldCoordinate,
    /// Region boundary size (half-extents in each dimension)
    pub bounds: f64,
    /// Current server load (0.0 to 1.0)
    pub load: f32,
    /// Server health status
    pub healthy: bool,
}

impl ServerRegion {
    pub fn new(server_id: String, region_coord: RegionCoordinate, center: WorldCoordinate, bounds: f64) -> Self {
        Self {
            server_id,
            region_coord,
            center,
            bounds,
            load: 0.0,
            healthy: true,
        }
    }
    
    /// Check if a world coordinate is within this region's boundaries
    pub fn contains_point(&self, coord: &WorldCoordinate) -> bool {
        (coord.x >= self.center.x - self.bounds && coord.x <= self.center.x + self.bounds) &&
        (coord.y >= self.center.y - self.bounds && coord.y <= self.center.y + self.bounds) &&
        (coord.z >= self.center.z - self.bounds && coord.z <= self.center.z + self.bounds)
    }
    
    /// Calculate distance from a point to this region's nearest boundary
    pub fn distance_to_boundary(&self, coord: &WorldCoordinate) -> f64 {
        // Calculate distance to each face of the cubic region
        let dx = (coord.x - self.center.x).abs() - self.bounds;
        let dy = (coord.y - self.center.y).abs() - self.bounds;
        let dz = (coord.z - self.center.z).abs() - self.bounds;
        
        // If inside the region, return the minimum distance to any boundary
        if dx <= 0.0 && dy <= 0.0 && dz <= 0.0 {
            -[dx, dy, dz].iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b))
        } else {
            // If outside, calculate distance to the region
            dx.max(0.0).powi(2) + dy.max(0.0).powi(2) + dz.max(0.0).powi(2)
        }.sqrt()
    }
    
    /// Check if a point is approaching this region's boundary within a threshold
    pub fn is_approaching_boundary(&self, coord: &WorldCoordinate, threshold: f64) -> bool {
        self.distance_to_boundary(coord) <= threshold
    }
}

/// Player movement tracking for predictive transfers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovementData {
    /// Current position
    pub position: WorldCoordinate,
    /// Previous position
    pub previous_position: WorldCoordinate,
    /// Current velocity (units per second)
    pub velocity: WorldCoordinate,
    /// Timestamp of last update
    pub timestamp: u64,
    /// Movement history for trajectory prediction
    pub position_history: Vec<(WorldCoordinate, u64)>,
}

impl MovementData {
    pub fn new(initial_position: WorldCoordinate) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
            
        Self {
            position: initial_position,
            previous_position: initial_position,
            velocity: WorldCoordinate::new(0.0, 0.0, 0.0),
            timestamp,
            position_history: vec![(initial_position, timestamp)],
        }
    }
    
    /// Update position and calculate velocity
    pub fn update_position(&mut self, new_position: WorldCoordinate) {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
            
        let delta_time = (current_time - self.timestamp) as f64 / 1000.0; // Convert to seconds
        
        if delta_time > 0.0 {
            self.previous_position = self.position;
            let movement = self.position.vector_to(&new_position);
            self.velocity = movement.scale(1.0 / delta_time);
        }
        
        self.position = new_position;
        self.timestamp = current_time;
        
        // Add to history and limit size
        self.position_history.push((new_position, current_time));
        if self.position_history.len() > 10 {
            self.position_history.remove(0);
        }
    }
    
    /// Predict future position based on current velocity
    pub fn predict_position(&self, seconds_ahead: f64) -> WorldCoordinate {
        let predicted_movement = self.velocity.scale(seconds_ahead);
        self.position.add(&predicted_movement)
    }
    
    /// Calculate time until reaching a specific coordinate at current velocity
    pub fn time_to_reach(&self, target: &WorldCoordinate) -> Option<f64> {
        let distance_vector = self.position.vector_to(target);
        let speed = self.velocity.magnitude();
        
        if speed == 0.0 {
            return None;
        }
        
        // Project distance onto velocity vector to check if moving towards target
        let velocity_normalized = self.velocity.normalized();
        let distance_normalized = distance_vector.normalized();
        
        // Dot product to check direction alignment
        let dot_product = velocity_normalized.x * distance_normalized.x + 
                         velocity_normalized.y * distance_normalized.y + 
                         velocity_normalized.z * distance_normalized.z;
        
        if dot_product <= 0.0 {
            // Moving away from or perpendicular to target
            return None;
        }
        
        Some(distance_vector.magnitude() / speed)
    }
    
    /// Get average velocity over recent history
    pub fn get_average_velocity(&self, seconds_back: f64) -> WorldCoordinate {
        let cutoff_time = self.timestamp - (seconds_back * 1000.0) as u64;
        
        let recent_positions: Vec<(WorldCoordinate, u64)> = self.position_history
            .iter()
            .filter(|(_, timestamp)| *timestamp >= cutoff_time)
            .cloned()
            .collect();
            
        if recent_positions.len() < 2 {
            return self.velocity;
        }
        
        let first = &recent_positions[0];
        let last = &recent_positions[recent_positions.len() - 1];
        
        let total_movement = first.0.vector_to(&last.0);
        let total_time = (last.1 - first.1) as f64 / 1000.0;
        
        if total_time > 0.0 {
            total_movement.scale(1.0 / total_time)
        } else {
            self.velocity
        }
    }
}