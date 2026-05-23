#[tokio::main]
async fn main() -> anyhow::Result<()> {
    scribe_backend::run_server().await
}
