use scribe_shelf::run;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run().await
}
