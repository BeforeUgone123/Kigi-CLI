use std::io::Read as _;

use anyhow::Context as _;
use kigi_models::WireModel;
use kigi_sampling_types::devin;

use super::models_fetch::BackendError;

fn devin_blocking_client() -> Result<reqwest::blocking::Client, BackendError> {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .context("failed to build devin catalog client")
        .map_err(|e| BackendError::Auth(e.to_string()))
}

pub(crate) fn fetch_devin_models(
    platform: kigi_models::PlatformId,
    bearer: &str,
) -> Result<Vec<WireModel>, BackendError> {
    let base = platform.base_url();
    let url = format!(
        "{}{}",
        base.trim_end_matches('/'),
        devin::GET_CLI_MODEL_CONFIGS_PATH
    );
    let api_key = devin::normalize_devin_session_token(bearer);
    let body = devin::build_get_cli_model_configs_request(
        &api_key,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default(),
        &uuid::Uuid::new_v4().to_string(),
    );

    let resp = devin_blocking_client()?
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, devin::PROTO_CONTENT_TYPE)
        .header(
            devin::CONNECT_PROTOCOL_VERSION_HEADER,
            devin::CONNECT_PROTOCOL_VERSION,
        )
        .header(reqwest::header::ACCEPT, devin::PROTO_CONTENT_TYPE)
        .body(body)
        .send()
        .map_err(|e| {
            if let Some(status) = e.status() {
                BackendError::RequestFailed {
                    status: status.as_u16(),
                    body: "devin catalog request failed".to_string(),
                }
            } else {
                BackendError::Network(e)
            }
        })?;

    let status = resp.status();
    if !status.is_success() {
        drop(resp);
        return Err(BackendError::RequestFailed {
            status: status.as_u16(),
            body: "devin catalog request failed".to_string(),
        });
    }

    let mut bytes = Vec::new();
    resp.take((devin::MAX_DEVIN_UNARY_PAYLOAD + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| BackendError::Auth(format!("devin catalog read failed: {e}")))?;
    if bytes.len() > devin::MAX_DEVIN_UNARY_PAYLOAD {
        return Err(BackendError::RequestFailed {
            status: 200,
            body: "devin catalog response oversized".to_string(),
        });
    }
    let decoded: devin::GetCliModelConfigsResponse =
        devin::decode_unary(&bytes).map_err(|e| BackendError::RequestFailed {
            status: 200,
            body: format!("devin catalog decode failed: {e}"),
        })?;
    devin_configs_to_wire_models(decoded)
}

pub(crate) fn devin_configs_to_wire_models(
    response: devin::GetCliModelConfigsResponse,
) -> Result<Vec<WireModel>, BackendError> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cfg in response.client_model_configs {
        if cfg.disabled {
            continue;
        }
        let uid = cfg.model_uid.trim();
        if uid.is_empty() {
            continue;
        }
        let info = cfg.model_info.as_ref();
        if info.is_some_and(|i| i.is_model_router) {
            continue;
        }
        let features = info.and_then(|i| i.model_features.as_ref());
        if features.is_some_and(|f| !f.supports_tool_calls) {
            continue;
        }
        if !seen.insert(uid.to_string()) {
            continue;
        }
        let context = info
            .map(|i| i.max_tokens)
            .filter(|v| *v > 0)
            .or_else(|| (cfg.max_tokens > 0).then_some(cfg.max_tokens))
            .map(|v| v as u64)
            .unwrap_or(crate::agent::models_fetch::DEFAULT_CONTEXT_WINDOW);
        let max_output = info
            .map(|i| i.max_output_tokens)
            .filter(|v| *v > 0)
            .map(|v| v as u64)
            .unwrap_or_else(|| context.min(64_000));
        out.push(WireModel {
            id: uid.to_string(),
            context_length: context,
            supports_reasoning: features.is_some_and(|f| f.supports_thinking),
            supports_image_in: features
                .map(|f| f.supports_images)
                .unwrap_or(cfg.supports_images),
            supports_video_in: false,
            display_name: (!cfg.label.is_empty()).then(|| cfg.label.clone()),
            max_output_tokens: max_output,
            supports_thinking_type: None,
            think_efforts: None,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message as _;

    fn config(uid: &str) -> devin::ClientModelConfig {
        devin::ClientModelConfig {
            model_uid: uid.to_string(),
            ..Default::default()
        }
    }

    fn featured(
        uid: &str,
        context: i32,
        max_out: i32,
        tool_calls: bool,
        thinking: bool,
        images: bool,
    ) -> devin::ClientModelConfig {
        devin::ClientModelConfig {
            model_uid: uid.to_string(),
            model_info: Some(devin::ModelInfo {
                max_tokens: context,
                max_output_tokens: max_out,
                model_features: Some(devin::ModelFeatures {
                    supports_images: images,
                    supports_tool_calls: tool_calls,
                    supports_thinking: thinking,
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn catalog_filters_disabled_blank_router_and_no_tool_models() {
        let router = devin::ClientModelConfig {
            model_uid: "MODEL_ROUTER".into(),
            model_info: Some(devin::ModelInfo {
                is_model_router: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let resp = devin::GetCliModelConfigsResponse {
            client_model_configs: vec![
                featured("MODEL_GOOD", 200_000, 8_000, true, true, true),
                devin::ClientModelConfig {
                    disabled: true,
                    ..featured("MODEL_DISABLED", 1, 1, true, false, false)
                },
                config("   "),
                config(""),
                router,
                featured("MODEL_NO_TOOLS", 1, 1, false, false, false),
                featured("MODEL_DUP", 1, 1, true, false, false),
                devin::ClientModelConfig {
                    label: "dup-2".into(),
                    ..featured("MODEL_DUP", 9, 9, true, false, false)
                },
            ],
        };
        let models = devin_configs_to_wire_models(resp).expect("project");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["MODEL_GOOD", "MODEL_DUP"], "only kept rows");
        let good = &models[0];
        assert_eq!(good.context_length, 200_000);
        assert_eq!(good.max_output_tokens, 8_000);
        assert!(good.supports_reasoning);
        assert!(good.supports_image_in);
    }

    #[test]
    fn catalog_nested_metadata_wins_and_absent_features_invent_nothing() {
        let nested = devin::ClientModelConfig {
            model_uid: "MODEL_NESTED".into(),
            max_tokens: 999,
            model_info: Some(devin::ModelInfo {
                max_tokens: 131_072,
                max_output_tokens: -1,
                ..Default::default()
            }),
            ..Default::default()
        };
        let plain = devin::ClientModelConfig {
            model_uid: "MODEL_PLAIN".into(),
            supports_images: true,
            max_tokens: 65_000,
            ..Default::default()
        };
        let resp = devin::GetCliModelConfigsResponse {
            client_model_configs: vec![nested, plain],
        };
        let models = devin_configs_to_wire_models(resp).expect("project");
        assert_eq!(models[0].context_length, 131_072, "nested context wins");
        assert_eq!(models[0].max_output_tokens, 64_000);
        assert!(!models[0].supports_reasoning, "no invented thinking");
        assert!(!models[0].supports_image_in, "features say nothing → false");
        assert_eq!(models[1].context_length, 65_000);
        assert!(models[1].supports_image_in, "config flag carries images");
        assert!(!models[1].supports_reasoning);
    }

    #[test]
    fn catalog_gzipped_unary_response_decodes() {
        let resp = devin::GetCliModelConfigsResponse {
            client_model_configs: vec![featured("MODEL_GZIP", 1000, 100, true, false, false)],
        };
        let wire = devin::gunzip_bounded(&resp.encode_to_vec(), devin::MAX_DEVIN_UNARY_PAYLOAD);
        assert!(wire.is_err(), "bare bytes are not gzip");
        let gz = {
            use std::io::Write as _;
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&resp.encode_to_vec()).unwrap();
            e.finish().unwrap()
        };
        let decoded: devin::GetCliModelConfigsResponse = devin::decode_unary(&gz).expect("gunzip");
        let models = devin_configs_to_wire_models(decoded).expect("project");
        assert_eq!(models[0].id, "MODEL_GZIP");
    }
}
