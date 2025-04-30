use clap::{Parser, ValueEnum};
use rmcp::{
    model::{CallToolRequestParam, ClientCapabilities, ClientInfo, Implementation},
    service::ServiceExt,
    transport::{SseTransport, TokioChildProcess},
};
use tokio::process::Command;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const SOCKET_ADDR: &str = "127.0.0.1:8006";

#[derive(Debug, Clone, ValueEnum)]
enum TransportType {
    Tcp,
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Gaia Weather MCP client")]
struct Args {
    /// Transport type to use (tcp or stdio)
    #[arg(short, long, value_enum, default_value = "tcp")]
    transport: TransportType,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| format!("info,{}=debug", env!("CARGO_CRATE_NAME")).into()),
        )
        .with(tracing_subscriber::fmt::layer().with_line_number(true))
        .init();

    let cli = Args::parse();

    match cli.transport {
        TransportType::Tcp => {
            tracing::info!("Connecting to Gaia TravelPlanner MCP server via tcp");

            // connect to mcp server
            let stream = tokio::net::TcpSocket::new_v4()?
                .connect(SOCKET_ADDR.parse()?)
                .await?;

            // create a mcp client
            let mcp_client = ().serve(stream).await?;

            // List available tools
            let tools = mcp_client.peer().list_tools(Default::default()).await?;
            tracing::info!(
                "Available tools:\n{}",
                serde_json::to_string_pretty(&tools)?
            );

            // request param
            let request_param = CallToolRequestParam {
                name: "get_drive_routes".into(),
                arguments: Some(serde_json::Map::from_iter([
                    (
                        "from".to_string(),
                        serde_json::Value::String("北京市海淀区丹棱街5号".to_string()),
                    ),
                    (
                        "to".to_string(),
                        serde_json::Value::String("北京市海淀区颐和园路5号".to_string()),
                    ),
                    (
                        "api_key".to_string(),
                        serde_json::Value::String(
                            std::env::var("AMAP_API_KEY").unwrap_or_else(|_| "".to_string()),
                        )
                        .into(),
                    ),
                ])),
            };

            // Call the sum tool
            let travel_result = mcp_client.peer().call_tool(request_param).await?;

            tracing::info!(
                "Travel result:\n{}",
                serde_json::to_string_pretty(&travel_result)?
            );
        }
    }

    Ok(())
}
