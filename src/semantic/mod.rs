//! Semantic routing ? embed prompt, match against route examples, dispatch to best model.
//!
//! Refer to aisix semantic.rs: embed prompt with embedding model,
//! compute cosine similarity with route example embeddings, pick best match.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Single semantic route rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticRoute {
    /// Route name (unique identifier)
    pub name: String,
    /// Target model (redirect to this model on match)
    pub target_model: String,
    /// Example prompts for embedding matching
    pub examples: Vec<String>,
    /// Match threshold (0.0~1.0, cosine similarity must exceed this)
    #[serde(default = "default_threshold")]
    pub threshold: f64,
}

fn default_threshold() -> f64 {
    0.75
}

/// Semantic routing store
pub struct SemanticRouter {
    routes: RwLock<Vec<SemanticRoute>>,
}

impl SemanticRouter {
    pub fn new() -> Self {
        Self {
            routes: RwLock::new(Vec::new()),
        }
    }

    /// Update route rules
    pub fn set_routes(&self, routes: Vec<SemanticRoute>) {
        *self.routes.write() = routes;
    }

    /// List current routes
    pub fn list_routes(&self) -> Vec<SemanticRoute> {
        self.routes.read().clone()
    }

    /// Match prompt against routes, returning best target model if any
    pub async fn match_route<F, Fut>(
        &self,
        prompt: &str,
        embed_fn: F,
    ) -> Option<String>
    where
        F: Fn(&str) -> Fut,
        Fut: std::future::Future<Output = Option<Vec<f32>>>,
    {
        let prompt_vec = embed_fn(prompt).await?;
        if prompt_vec.is_empty() {
            return None;
        }

        let routes = self.routes.read().clone();
        if routes.is_empty() {
            return None;
        }

        let mut best_match: Option<(String, f64)> = None;

        for route in &routes {
            for example in &route.examples {
                let example_vec = embed_fn(example).await?;
                if example_vec.is_empty() {
                    continue;
                }
                let sim = cosine_similarity(&prompt_vec, &example_vec);
                if sim >= route.threshold
                    && (best_match.is_none() || sim > best_match.as_ref().unwrap().1) {
                        best_match = Some((route.target_model.clone(), sim));
                    }
            }
        }

        best_match.map(|(model, _)| model)
    }
}

/// Cosine similarity between two vectors
fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    (dot / (norm_a * norm_b)) as f64
}

impl Default for SemanticRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);

        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &c) - 0.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn test_match_route() {
        let router = SemanticRouter::new();
        router.set_routes(vec![SemanticRoute {
            name: "code".into(),
            target_model: "deepseek-coder-6.7b".into(),
            examples: vec!["write a function".into(), "debug this code".into()],
            threshold: 0.5,
        }]);

        let result = router
            .match_route("write a function", |text: &str| {
                let text = text.to_string();
                async move {
                    let words: Vec<&str> = text.split_whitespace().collect();
                    let mut vec = vec![0.0f32; 100];
                    for w in &words {
                        let h = w.chars().next().map(|c| c as usize).unwrap_or(0) % 100;
                        vec[h] = 1.0;
                    }
                    Some(vec)
                }
            })
            .await;
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "deepseek-coder-6.7b");
    }
}
