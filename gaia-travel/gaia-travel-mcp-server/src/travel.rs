use rmcp::{
    Error as McpError, ServerHandler,
    model::{CallToolResult, Content, ErrorCode, ServerCapabilities, ServerInfo},
    schemars, tool,
};
use std::collections::HashMap;
use tracing::{debug, info};

#[derive(Debug, Clone)]
pub struct TravelPlanner;

#[tool(tool_box)]
impl TravelPlanner {
    #[tool(description = "Get the drive routes between two addresses")]
    async fn get_drive_routes(
        &self,
        #[tool(aggr)] GetDriveRoutesRequest { from, to, api_key }: GetDriveRoutesRequest,
    ) -> Result<CallToolResult, McpError> {
        let api_key = match api_key {
            Some(api_key) => api_key,
            None => match std::env::var("AMAP_API_KEY") {
                Ok(api_key) => api_key,
                Err(_) => {
                    return Err(McpError::new(
                        ErrorCode::INVALID_PARAMS,
                        "No API key provided".to_string(),
                        None,
                    ));
                }
            },
        };

        // get location from the starting address
        let loc_from = {
            let tool_result = self
                .get_geocode(GetGeocodeRequest {
                    address: from,
                    api_key: Some(api_key.clone()),
                })
                .await?;
            let tool_result_content = tool_result.content[0].as_text().unwrap().text.as_ref();
            let geocode_response =
                serde_json::from_str::<GetGeocodeResponse>(tool_result_content).unwrap();
            geocode_response.geocodes[0].location.clone().unwrap()
        };

        // get location from the destination address
        let loc_to = {
            let tool_result = self
                .get_geocode(GetGeocodeRequest {
                    address: to,
                    api_key: Some(api_key.clone()),
                })
                .await?;
            let tool_result_content = tool_result.content[0].as_text().unwrap().text.as_ref();
            let geocode_response =
                serde_json::from_str::<GetGeocodeResponse>(tool_result_content).unwrap();
            geocode_response.geocodes[0].location.clone().unwrap()
        };

        self.get_drive_routes_by_coord(GetDriveRoutesByCoordRequest {
            from: loc_from,
            to: loc_to,
            api_key: Some(api_key),
        })
        .await
    }

    #[tool(description = "Get the drive routes between two coordinates")]
    async fn get_drive_routes_by_coord(
        &self,
        #[tool(aggr)]
        GetDriveRoutesByCoordRequest { from, to, api_key }: GetDriveRoutesByCoordRequest,
    ) -> Result<CallToolResult, McpError> {
        let api_key = match api_key {
            Some(api_key) => api_key,
            None => match std::env::var("AMAP_API_KEY") {
                Ok(api_key) => api_key,
                Err(_) => {
                    return Err(McpError::new(
                        ErrorCode::INVALID_PARAMS,
                        "No API key provided".to_string(),
                        None,
                    ));
                }
            },
        };

        let mut params = HashMap::new();
        params.insert("origin", from);
        params.insert("destination", to);
        params.insert("key", api_key);
        // params.insert("extensions", "all");

        // send the request to get the directions
        let response = reqwest::Client::new()
            .get("https://restapi.amap.com/v3/direction/driving")
            .query(&params)
            .send()
            .await
            .map_err(|e| McpError::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

        if response.status().is_success() {
            let json = response
                .json::<serde_json::Value>()
                .await
                .map_err(|e| McpError::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

            info!("json: {}", serde_json::to_string_pretty(&json).unwrap());

            let map = json.as_object().unwrap();

            let status = map.get("status").unwrap().as_str().unwrap();
            debug!("status: {status}");
            if status == "0" {
                let info = map.get("info").unwrap().as_str().unwrap();

                return Err(McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to get drive routes: {info}"),
                    None,
                ));
            }

            let content = Content::json(GetDriveRoutesResponse { count: 0 })?;

            Ok(CallToolResult::success(vec![content]))
        } else {
            Err(McpError::new(
                ErrorCode::INTERNAL_ERROR,
                "Failed to get directions".to_string(),
                None,
            ))
        }
    }

    #[tool(description = "Get the geocode for an address")]
    async fn get_geocode(
        &self,
        #[tool(aggr)] GetGeocodeRequest { address, api_key }: GetGeocodeRequest,
    ) -> Result<CallToolResult, McpError> {
        let api_key = match api_key {
            Some(api_key) => api_key,
            None => match std::env::var("AMAP_API_KEY") {
                Ok(api_key) => api_key,
                Err(_) => {
                    return Err(McpError::new(
                        ErrorCode::INVALID_PARAMS,
                        "No API key provided".to_string(),
                        None,
                    ));
                }
            },
        };

        let mut params = HashMap::new();
        params.insert("address", address);
        params.insert("key", api_key);

        // send the request to get the geocode
        let response = reqwest::Client::new()
            .get("https://restapi.amap.com/v3/geocode/geo")
            .query(&params)
            .send()
            .await
            .map_err(|e| McpError::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

        if response.status().is_success() {
            let json = response
                .json::<serde_json::Value>()
                .await
                .map_err(|e| McpError::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

            info!("json: {json:#?}");

            let map = json.as_object().unwrap();

            let status = map.get("status").unwrap().as_str().unwrap();
            debug!("status: {status}");
            if status == "0" {
                let info = map.get("info").unwrap().as_str().unwrap();

                return Err(McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to get drive routes: {info}"),
                    None,
                ));
            }

            let count = map
                .get("count")
                .unwrap()
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap();
            let json_geocodes = map.get("geocodes").unwrap().as_array().unwrap();
            let mut geocodes = Vec::new();
            if !json_geocodes.is_empty() {
                for json_geocode in json_geocodes {
                    let geocode = json_geocode.as_object().unwrap();

                    let formatted_address = match geocode.get("formatted_address") {
                        Some(addr) if addr.is_string() => Some(addr.as_str().unwrap().to_string()),
                        _ => None,
                    };

                    let country = match geocode.get("country") {
                        Some(country) if country.is_string() => {
                            Some(country.as_str().unwrap().to_string())
                        }
                        _ => None,
                    };

                    let province = match geocode.get("province") {
                        Some(prov) if prov.is_string() => Some(prov.as_str().unwrap().to_string()),
                        _ => None,
                    };

                    let city = match geocode.get("city") {
                        Some(city) if city.is_string() => Some(city.as_str().unwrap().to_string()),
                        _ => None,
                    };

                    let city_code = match geocode.get("citycode") {
                        Some(city_code) if city_code.is_string() => {
                            Some(city_code.as_str().unwrap().to_string())
                        }
                        _ => None,
                    };

                    let district = match geocode.get("district") {
                        Some(district) if district.is_string() => {
                            Some(district.as_str().unwrap().to_string())
                        }
                        _ => None,
                    };

                    let street = match geocode.get("street") {
                        Some(street) if street.is_string() => {
                            Some(street.as_str().unwrap().to_string())
                        }
                        _ => None,
                    };

                    let number = match geocode.get("number") {
                        Some(number) if number.is_string() => {
                            Some(number.as_str().unwrap().to_string())
                        }
                        _ => None,
                    };

                    let adcode = match geocode.get("adcode") {
                        Some(adcode) if adcode.is_string() => {
                            Some(adcode.as_str().unwrap().to_string())
                        }
                        _ => None,
                    };

                    let location = match geocode.get("location") {
                        Some(location) if location.is_string() => {
                            Some(location.as_str().unwrap().to_string())
                        }
                        _ => None,
                    };

                    let level = match geocode.get("level") {
                        Some(level) if level.is_string() => {
                            Some(level.as_str().unwrap().to_string())
                        }
                        _ => None,
                    };

                    geocodes.push(Geocode {
                        formatted_address,
                        country,
                        province,
                        city,
                        city_code,
                        district,
                        street,
                        number,
                        adcode,
                        location,
                        level,
                    });
                }
            }

            let content = Content::json(GetGeocodeResponse { count, geocodes })?;

            Ok(CallToolResult::success(vec![content]))
        } else {
            Err(McpError::new(
                ErrorCode::INTERNAL_ERROR,
                "Failed to get directions".to_string(),
                None,
            ))
        }
    }
}

#[tool(tool_box)]
impl ServerHandler for TravelPlanner {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some("A travel planner".into()),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

/// Get the drive routes between two addresses
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct GetDriveRoutesRequest {
    #[schemars(description = "the starting address, e.g., '北京市海淀区丹棱街5号'")]
    pub from: String,
    #[schemars(description = "the destination address, e.g., '北京市海淀区颐和园路5号'")]
    pub to: String,
    #[schemars(description = "the API key for the maps service")]
    pub api_key: Option<String>,
}

/// Get the drive routes between two coordinates
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct GetDriveRoutesByCoordRequest {
    #[schemars(description = "the starting coordinates, e.g., '116.310003,39.992892'")]
    pub from: String,
    #[schemars(description = "the destination coordinates, e.g., '116.310003,39.992892'")]
    pub to: String,
    #[schemars(description = "the API key for the maps service")]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct GetDriveRoutesResponse {
    #[schemars(description = "the number of directions")]
    pub count: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GetGeocodeRequest {
    #[schemars(description = "the address to get the geocode for, e.g., '北京市海淀区丹棱街5号'")]
    pub address: String,
    #[schemars(description = "the API key for the maps service")]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GetGeocodeResponse {
    #[schemars(description = "the count of geocode results")]
    pub count: u64,
    #[schemars(description = "the geocode results")]
    pub geocodes: Vec<Geocode>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Geocode {
    #[schemars(description = "the formatted address")]
    #[serde(default)]
    pub formatted_address: Option<String>,
    #[schemars(description = "the country")]
    #[serde(default)]
    pub country: Option<String>,
    #[schemars(description = "the province")]
    #[serde(default)]
    pub province: Option<String>,
    #[schemars(description = "the city")]
    #[serde(default)]
    pub city: Option<String>,
    #[schemars(description = "the city code")]
    #[serde(default)]
    pub city_code: Option<String>,
    #[schemars(description = "the district")]
    #[serde(default)]
    pub district: Option<String>,
    #[schemars(description = "the street")]
    #[serde(default)]
    pub street: Option<String>,
    #[schemars(description = "the street number")]
    #[serde(default)]
    pub number: Option<String>,
    #[schemars(description = "the city code (adcode)")]
    #[serde(default)]
    pub adcode: Option<String>,
    #[schemars(description = "the point")]
    #[serde(default)]
    pub location: Option<String>,
    #[schemars(description = "the geocoding matching level of the address")]
    #[serde(default)]
    pub level: Option<String>,
}
