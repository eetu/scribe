use std::sync::Arc;

use crate::config::Config;
use crate::jobs::JobMap;

#[derive(Clone)]
pub struct PressState {
    pub cfg: Arc<Config>,
    pub jobs: JobMap,
}
