use crate::config::Config;
use crate::db::Db;

#[derive(Clone)]
pub struct ShelfState {
    pub cfg: Config,
    pub db: Db,
    pub http: reqwest::Client,
}
