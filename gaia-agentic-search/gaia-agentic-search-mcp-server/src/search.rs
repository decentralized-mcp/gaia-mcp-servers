use crate::AgenticSearchConfig;
use endpoints::{
    chat::{
        ChatCompletionObject, ChatCompletionRequestBuilder, ChatCompletionRequestMessage,
        ChatCompletionUserMessageContent,
    },
    embeddings::{EmbeddingRequest, EmbeddingsResponse, InputText},
};
use gaia_agentic_search_mcp_common::{QdrantSearchHit, SearchRequest, TidbSearchHit};
use mysql::prelude::*;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use rmcp::{
    Error as McpError, ServerHandler,
    handler::server::tool::*,
    model::{
        CallToolResult, Content, ErrorCode, Implementation, ServerCapabilities, ServerInfo, Tool,
    },
    tool,
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
};
use tracing::{debug, error, info, warn};

static SEARCH_TOOL_DESC: OnceLock<String> = OnceLock::new();
static SEARCH_TOOL_PARAM_DESC: OnceLock<String> = OnceLock::new();

pub fn set_search_tool_description(description: String) {
    SEARCH_TOOL_DESC.set(description).unwrap_or_default();
}

pub fn set_search_tool_param_description(description: String) {
    SEARCH_TOOL_PARAM_DESC.set(description).unwrap_or_default();
}

#[derive(Debug, Clone)]
pub struct AgenticSearchServer {
    config: AgenticSearchConfig,
}

impl AgenticSearchServer {
    pub fn new(config: AgenticSearchConfig) -> Self {
        Self { config }
    }

    fn search_tool_attr() -> Tool {
        let tool_description = SEARCH_TOOL_DESC
            .get()
            .cloned()
            .unwrap_or_else(|| "Perform a search for the given query".to_string());

        let query_description = SEARCH_TOOL_PARAM_DESC
            .get()
            .cloned()
            .unwrap_or_else(|| "The query to search for".to_string());

        // build input schema
        let input_schema = json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": query_description
                }
            },
            "required": ["query"],
            "title": "SearchRequest"
        });

        Tool {
            name: "search".into(),
            description: Some(tool_description.into()),
            input_schema: Arc::new(input_schema.as_object().unwrap().clone()),
            annotations: None,
        }
    }

    async fn search_tool_call(
        context: ToolCallContext<'_, Self>,
    ) -> Result<CallToolResult, McpError> {
        let (__rmcp_tool_receiver, context) = <&Self>::from_tool_call_context_part(context)?;
        let (Parameters(SearchRequest { query }), _context) =
            <Parameters<SearchRequest>>::from_tool_call_context_part(context)?;
        Self::search(__rmcp_tool_receiver, query)
            .await
            .into_call_tool_result()
    }

    async fn search(&self, query: String) -> Result<CallToolResult, McpError> {
        match (
            self.config.qdrant_config.is_some(),
            self.config.tidb_config.is_some(),
        ) {
            (true, true) => {
                let sources = self.combined_search(query).await?;
                Ok(CallToolResult::success(vec![Content::text(
                    sources.join("\n"),
                )]))
            }
            (true, false) => {
                let sources = self.vector_search(query).await?;
                Ok(CallToolResult::success(vec![Content::text(
                    sources.join("\n"),
                )]))
            }
            (false, true) => {
                let sources = self.keyword_search(query).await?;
                Ok(CallToolResult::success(vec![Content::text(
                    sources.join("\n"),
                )]))
            }
            (false, false) => {
                let error_message = "No search mode configured";
                error!("{}", error_message);
                Err(McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    error_message,
                    None,
                ))
            }
        }
    }

    fn tool_box() -> &'static ToolBox<AgenticSearchServer> {
        static TOOL_BOX: OnceLock<ToolBox<AgenticSearchServer>> = OnceLock::new();
        TOOL_BOX.get_or_init(|| {
            let mut tool_box = ToolBox::new();
            tool_box.add(ToolBoxItem::new(
                AgenticSearchServer::search_tool_attr(),
                |context| Box::pin(AgenticSearchServer::search_tool_call(context)),
            ));
            tool_box
        })
    }

    // #[tool(description = "Perform a search for the given query")]
    // async fn search(
    //     &self,
    //     #[tool(aggr)] SearchRequest { query }: SearchRequest,
    // ) -> Result<CallToolResult, McpError> {
    //     match (
    //         self.config.qdrant_config.is_some(),
    //         self.config.tidb_config.is_some(),
    //     ) {
    //         (true, true) => self.combined_search(query).await,
    //         (true, false) => {
    //             let content = self.vector_search(query).await?;
    //             Ok(CallToolResult::success(vec![Content::text(content)]))
    //         }
    //         (false, true) => {
    //             let content = self.keyword_search(query).await?;
    //             Ok(CallToolResult::success(vec![Content::text(content)]))
    //         }
    //         (false, false) => {
    //             let error_message = "No search mode configured";
    //             error!("{}", error_message);
    //             Err(McpError::new(
    //                 ErrorCode::INTERNAL_ERROR,
    //                 error_message,
    //                 None,
    //             ))
    //         }
    //     }
    // }

    async fn vector_search(&self, query: impl AsRef<str>) -> Result<Vec<String>, McpError> {
        info!("Starting vector search ...");

        // compute the embedding of the query
        info!("Computing embedding of the query...");
        let embedding = self.compute_embedding(query.as_ref()).await?;

        // search in qdrant
        info!("Searching in Qdrant...");
        let hits = self.search_in_qdrant(embedding).await?;

        if !hits.is_empty() {
            let qdrant_config = self.config.qdrant_config.as_ref().unwrap();
            let payload_source = &qdrant_config.payload_source;
            info!(
                "Extracting the payload ({}) of the vector search results...",
                payload_source
            );
            let mut output = Vec::new();
            for hit in hits {
                let source = hit.payload.get(payload_source).unwrap().as_str().unwrap();
                output.push(source.to_string());
            }

            info!("Vector search done! 🎉");

            debug!("vector search results:\n{:#?}", &output);

            Ok(output)
        } else {
            let error_message = "No vector search results found in Qdrant";
            warn!("{}", error_message);
            Ok(vec![])
        }
    }

    async fn keyword_search(&self, query: impl AsRef<str>) -> Result<Vec<String>, McpError> {
        info!("Starting keyword search ...");

        // extract keywords from the query
        info!("Extracting keywords from the query...");
        let keywords = self.extract_keywords(query.as_ref()).await?;

        // search in tidb
        info!("Searching in TiDB...");
        let hits = self.search_in_tidb(keywords).await?;

        if !hits.is_empty() {
            // format the search results
            info!("Extracting the source of the keyword search results...");
            let mut output = Vec::new();
            for hit in hits {
                output.push(hit.content);
            }

            info!("Keyword search done! 🎉");

            debug!("keyword search results:\n{:#?}", &output);

            Ok(output)
        } else {
            let error_message = "No keyword search results found in TiDB";
            warn!("{}", error_message);
            Ok(vec![])
        }
    }

    async fn combined_search(&self, query: String) -> Result<Vec<String>, McpError> {
        let vector_search_result = self.vector_search(query.as_str()).await?;
        let keyword_search_result = self.keyword_search(query.as_str()).await?;

        info!("Combining vector and keyword search results ...");

        let output = if !vector_search_result.is_empty() && !keyword_search_result.is_empty() {
            let mut output: HashSet<String> = HashSet::from_iter(vector_search_result);
            output.extend(keyword_search_result);

            Vec::from_iter(output)
        } else if !vector_search_result.is_empty() {
            vector_search_result
        } else {
            keyword_search_result
        };

        info!("Combined search done! 🎉");

        debug!("combined search results:\n{:#?}", &output);

        Ok(output)
    }

    async fn compute_embedding(&self, query: impl AsRef<str>) -> Result<Vec<f64>, McpError> {
        match &self.config.embedding_service {
            Some(config) => {
                let embedding_service_url =
                    format!("{}/v1/embeddings", config.url.trim_end_matches('/'));

                // create a embedding request
                let embedding_request = EmbeddingRequest {
                    model: None,
                    input: InputText::String(query.as_ref().to_string()),
                    encoding_format: None,
                    user: None,
                };

                let response = match &config.api_key {
                    Some(api_key) => reqwest::Client::new()
                        .post(&embedding_service_url)
                        .header(CONTENT_TYPE, "application/json")
                        .header(AUTHORIZATION, api_key)
                        .json(&embedding_request)
                        .send()
                        .await
                        .map_err(|e| {
                            let err_msg = format!("Failed to send the embedding request: {e}");
                            error!("{}", err_msg);
                            McpError::new(ErrorCode::INTERNAL_ERROR, err_msg, None)
                        })?,
                    None => reqwest::Client::new()
                        .post(&embedding_service_url)
                        .header(CONTENT_TYPE, "application/json")
                        .json(&embedding_request)
                        .send()
                        .await
                        .map_err(|e| {
                            let err_msg = format!("Failed to send the embedding request: {e}");
                            error!("{}", err_msg);
                            McpError::new(ErrorCode::INTERNAL_ERROR, err_msg, None)
                        })?,
                };

                let bytes = response.bytes().await.map_err(|e| {
                    let err_msg = format!("Failed to parse embeddings response: {e}");
                    error!("{}", err_msg);
                    McpError::new(ErrorCode::INTERNAL_ERROR, err_msg, None)
                })?;

                // parse the response
                let embedding_response = serde_json::from_slice::<EmbeddingsResponse>(&bytes)
                    .map_err(|e| {
                        let err_msg = format!("Failed to parse embeddings response: {e}");
                        error!("{}", err_msg);
                        McpError::new(ErrorCode::INTERNAL_ERROR, err_msg, None)
                    })?;

                let embedding = embedding_response.data.first().ok_or_else(|| {
                    let err_msg = "No embeddings returned";
                    error!("{}", err_msg);
                    McpError::new(ErrorCode::INTERNAL_ERROR, err_msg, None)
                })?;

                Ok(embedding.embedding.to_vec())
            }
            None => {
                let error_message = "Embedding service URL is not configured";
                error!("{}", error_message);
                Err(McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    error_message,
                    None,
                ))
            }
        }
    }

    async fn search_in_qdrant(
        &self,
        vector: impl AsRef<[f64]>,
    ) -> Result<Vec<QdrantSearchHit>, McpError> {
        match &self.config.qdrant_config {
            Some(qdrant_config) => {
                let base_url = qdrant_config.base_url.trim_end_matches('/');
                let url = format!(
                    "{}/collections/{}/points/search",
                    base_url, qdrant_config.collection
                );

                // build params
                let params = json!({
                    "vector": vector.as_ref().to_vec(),
                    "limit": self.config.limit,
                    "with_payload": true,
                    "with_vector": true,
                    "score_threshold": self.config.score_threshold,
                });

                let response = match &qdrant_config.api_key {
                    Some(api_key) => reqwest::Client::new()
                        .post(&url)
                        .header("api-key", api_key)
                        .header("Content-Type", "application/json")
                        .json(&params)
                        .send()
                        .await
                        .map_err(|e| {
                            let err_msg = format!("Failed to search points: {e}");
                            error!("{}", err_msg);
                            McpError::new(ErrorCode::INTERNAL_ERROR, err_msg, None)
                        })?,
                    None => reqwest::Client::new()
                        .post(&url)
                        .header("Content-Type", "application/json")
                        .json(&params)
                        .send()
                        .await
                        .map_err(|e| {
                            let err_msg = format!("Failed to search points: {e}");
                            error!("{}", err_msg);
                            McpError::new(ErrorCode::INTERNAL_ERROR, err_msg, None)
                        })?,
                };

                let status = response.status();
                if !status.is_success() {
                    let error_message =
                        format!("Failed to send search request to Qdrant server. Status: {status}");
                    error!("{}", error_message);
                    return Err(McpError::new(
                        ErrorCode::INTERNAL_ERROR,
                        error_message,
                        None,
                    ));
                }

                match response.json::<Value>().await {
                    Ok(json) => match json.get("result") {
                        Some(result) => {
                            let hits = result
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|v| QdrantSearchHit {
                                    score: v.get("score").unwrap().as_f64().unwrap(),
                                    payload: v
                                        .get("payload")
                                        .unwrap()
                                        .as_object()
                                        .unwrap()
                                        .to_owned()
                                        .into_iter()
                                        .map(|(k, v)| (k.to_string(), v.clone()))
                                        .collect(),
                                    vector: v
                                        .get("vector")
                                        .unwrap()
                                        .as_array()
                                        .unwrap()
                                        .to_owned()
                                        .iter()
                                        .map(|v| v.as_f64().unwrap())
                                        .collect::<Vec<f64>>(),
                                })
                                .collect();

                            Ok(hits)
                        }
                        None => {
                            debug!(
                                "Qdrant search response:\n{}",
                                serde_json::to_string_pretty(&json).unwrap()
                            );

                            match json.get("status") {
                                Some(status) => {
                                    let error_message = format!(
                                        "Failed to search points. {}",
                                        status.get("error").unwrap().as_str().unwrap()
                                    );
                                    error!("{}", error_message);
                                    Err(McpError::new(
                                        ErrorCode::INTERNAL_ERROR,
                                        error_message,
                                        None,
                                    ))
                                }
                                None => {
                                    let error_message = "Failed to search points. ";
                                    error!("{}", error_message);
                                    Err(McpError::new(
                                        ErrorCode::INTERNAL_ERROR,
                                        error_message,
                                        None,
                                    ))
                                }
                            }
                        }
                    },
                    Err(e) => {
                        let error_message = format!("Failed to search points: {e}");
                        error!("{}", error_message);
                        Err(McpError::new(
                            ErrorCode::INTERNAL_ERROR,
                            error_message,
                            None,
                        ))
                    }
                }
            }
            None => {
                let error_message = "Qdrant config is not set";
                error!("{}", error_message);
                Err(McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    error_message,
                    None,
                ))
            }
        }
    }

    /// Extract keywords from the query using the embedding service
    ///
    /// # Arguments
    ///
    /// * `query` - The query to extract keywords from
    ///
    /// # Returns
    ///
    /// A string containing the extracted keywords separated by spaces
    async fn extract_keywords(&self, query: impl AsRef<str>) -> Result<String, McpError> {
        let config = self.config.chat_service.as_ref().unwrap();

        let text = query.as_ref();
        let user_prompt = format!(
            "You are a multilingual keyword extractor. Your task is to extract the most relevant and concise keywords or key phrases from the given user query. The keywords should satisfying the following requirements:\n- Detect the language of the query automatically.\n- Return 3 to 7 keywords or keyphrases that best represent the query's core intent.\n- Keep the extracted keywords in the **original language** (do not translate).\n- Include **multi-word expressions** if they convey meaningful concepts.\n- The keywords should be separated by spaces.\n- Avoid stop words, filler words, or overly generic terms.\n\n### Input Query\n{text:#?}",
        );

        let user_message = ChatCompletionRequestMessage::new_user_message(
            ChatCompletionUserMessageContent::Text(user_prompt),
            None,
        );

        // create a request
        let request = ChatCompletionRequestBuilder::new(&[user_message]).build();

        let chat_service_url = format!("{}/v1/chat/completions", config.url.trim_end_matches('/'));
        debug!(
            "Forward the chat request to {} for extracting keywords",
            chat_service_url,
        );
        let response = match &config.api_key {
            Some(api_key) => reqwest::Client::new()
                .post(&chat_service_url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::AUTHORIZATION, api_key)
                .json(&request)
                .send()
                .await
                .map_err(|e| {
                    let err_msg = format!("Failed to send the chat request: {e}");
                    error!("{}", err_msg);
                    McpError::new(ErrorCode::INTERNAL_ERROR, err_msg, None)
                })?,
            None => reqwest::Client::new()
                .post(&chat_service_url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .json(&request)
                .send()
                .await
                .map_err(|e| {
                    let err_msg = format!("Failed to send the chat request: {e}");
                    error!("{}", err_msg);
                    McpError::new(ErrorCode::INTERNAL_ERROR, err_msg, None)
                })?,
        };

        let chat_completion_object =
            response.json::<ChatCompletionObject>().await.map_err(|e| {
                let err_msg = format!("Failed to parse the chat response: {e}");
                error!("{}", err_msg);
                McpError::new(ErrorCode::INTERNAL_ERROR, err_msg, None)
            })?;

        let content = chat_completion_object.choices[0]
            .message
            .content
            .as_ref()
            .unwrap();

        Ok(content.to_string())
    }

    /// Search in TiDB using the keywords
    ///
    /// # Arguments
    ///
    /// * `keywords` - The keywords to search for. The keywords should be separated by spaces.
    ///
    /// # Returns
    ///
    /// A string containing the search results
    async fn search_in_tidb(
        &self,
        keywords: impl AsRef<str>,
    ) -> Result<Vec<TidbSearchHit>, McpError> {
        match &self.config.tidb_config {
            Some(tidb_config) => {
                // get connection
                debug!("Getting connection to TiDB Cloud...");
                let mut conn = tidb_config.pool.get_conn().map_err(|e| {
                    let error_message = format!("Failed to get connection: {e}");

                    error!(error_message);

                    McpError::new(ErrorCode::INTERNAL_ERROR, error_message, None)
                })?;

                // test connection
                debug!("Testing connection...");
                let version: String = match conn.query_first("SELECT VERSION()").map_err(|e| {
                    let error_message = format!("Failed to query version: {e}");

                    error!(error_message);

                    McpError::new(ErrorCode::INTERNAL_ERROR, error_message, None)
                })? {
                    Some(version) => version,
                    None => {
                        let error_message = "Failed to query version";

                        error!(error_message);

                        return Err(McpError::new(
                            ErrorCode::INTERNAL_ERROR,
                            error_message,
                            None,
                        ));
                    }
                };
                debug!("Connected to TiDB Cloud! Version: {}", version);

                // check if table exists
                debug!("Checking if table exists...");
                let check_table_sql = format!(
                    "SELECT COUNT(*) FROM information_schema.tables
                WHERE table_schema = '{}' AND table_name = '{}'",
                    tidb_config.database, tidb_config.table_name
                );
                let table_exists: i32 = conn
                    .query_first(&check_table_sql)
                    .map_err(|e| {
                        let error_message = format!("Failed to check table: {e}");

                        error!(error_message);

                        McpError::new(ErrorCode::INTERNAL_ERROR, error_message, None)
                    })?
                    .unwrap_or(0);

                if table_exists == 0 {
                    let error_message = format!(
                        "Not found table `{}` in database `{}`",
                        tidb_config.table_name, tidb_config.database
                    );

                    error!(error_message);

                    return Err(McpError::new(
                        ErrorCode::INTERNAL_ERROR,
                        error_message,
                        None,
                    ));
                }

                // execute full-text search
                let query = keywords.as_ref();
                debug!("\nExecuting full-text search for '{}'...", query);
                let search_sql = format!(
                    r"SELECT * FROM {}
                WHERE fts_match_word('{}', content)
                ORDER BY fts_match_word('{}', content)
                DESC LIMIT {}",
                    tidb_config.table_name, query, query, self.config.limit
                );

                conn.query(&search_sql).map_err(|e| {
                    let error_message = format!("Failed to execute search: {e}");

                    error!(error_message);

                    McpError::new(ErrorCode::INTERNAL_ERROR, error_message, None)
                })
            }
            None => {
                let error_message = "TiDB config is not set";
                error!("{}", error_message);
                Err(McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    error_message,
                    None,
                ))
            }
        }
    }
}

#[tool(tool_box)]
impl ServerHandler for AgenticSearchServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some("Gaia Agentic Search MCP server".into()),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: std::env!("CARGO_PKG_NAME").to_string(),
                version: std::env!("CARGO_PKG_VERSION").to_string(),
            },
            ..Default::default()
        }
    }
}
