use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DagEngine {
    completed_workers: HashSet<String>,
    waiting_dependencies: HashMap<String, HashSet<String>>, // worker_id -> set of required worker_ids
}

impl DagEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_task(&mut self, worker_id: &str, depends_on: &[String]) -> bool {
        let unsatisfied: HashSet<String> = depends_on
            .iter()
            .filter(|dep| !self.completed_workers.contains(*dep))
            .cloned()
            .collect();

        if unsatisfied.is_empty() {
            true // all dependencies satisfied immediately
        } else {
            self.waiting_dependencies.insert(worker_id.to_string(), unsatisfied);
            false // must wait
        }
    }

    pub fn mark_completed(&mut self, worker_id: &str) -> Vec<String> {
        self.completed_workers.insert(worker_id.to_string());

        let mut ready_workers = Vec::new();

        // Check which waiting workers now have all dependencies satisfied
        let mut newly_ready = Vec::new();
        for (w_id, deps) in self.waiting_dependencies.iter_mut() {
            deps.remove(worker_id);
            if deps.is_empty() {
                newly_ready.push(w_id.clone());
            }
        }

        for ready in newly_ready {
            self.waiting_dependencies.remove(&ready);
            ready_workers.push(ready);
        }

        ready_workers
    }

    pub fn is_satisfied(&self, depends_on: &[String]) -> bool {
        depends_on.iter().all(|dep| self.completed_workers.contains(dep))
    }

    pub fn reset(&mut self) {
        self.completed_workers.clear();
        self.waiting_dependencies.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dag_flow() {
        let mut dag = DagEngine::new();
        assert!(dag.register_task("w1", &[]));
        assert!(dag.register_task("w2", &[]));

        // w3 depends on w1 and w2
        assert!(!dag.register_task("w3", &["w1".into(), "w2".into()]));

        // w1 completes -> w3 still waits for w2
        let ready1 = dag.mark_completed("w1");
        assert!(ready1.is_empty());

        // w2 completes -> w3 is now ready!
        let ready2 = dag.mark_completed("w2");
        assert_eq!(ready2, vec!["w3"]);
    }
}
