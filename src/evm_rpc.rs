//! Adapt the SDK's single EVM endpoint to rgb-lib's chain-ID keyed configuration.
use crate::error::APIError;
use std::{collections::HashMap, time::Duration};

fn chain_id(response: &serde_json::Value) -> Result<u64, APIError> {
    crate::bfa_values::parse_evm_chain_id(
        response.get("result").and_then(|value| value.as_str()),
        response.get("error").is_some(),
    )
    .map_err(|message| APIError::InvalidEthRpcUrl(message.into()))
}

pub(crate) async fn resolve_evm_rpc_urls(
    url: Option<&str>,
) -> Result<HashMap<u64, String>, APIError> {
    let Some(url) = url else {
        return Ok(HashMap::new());
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| APIError::InvalidEthRpcUrl("could not initialize RPC client".into()))?;
    let response: serde_json::Value = client
        .post(url)
        .json(
            &serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": []}),
        )
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|_| APIError::InvalidEthRpcUrl("eth_chainId request failed".into()))?
        .json()
        .await
        .map_err(|_| APIError::InvalidEthRpcUrl("invalid JSON from EVM RPC".into()))?;
    Ok(HashMap::from([(chain_id(&response)?, url.to_string())]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn discovers_the_endpoint_chain_without_assuming_ethereum_mainnet() {
        for (hex, expected) in [("0xa4b1", 42161), ("0x1", 1), ("0xaa36a7", 11155111)] {
            assert_eq!(chain_id(&json!({"result": hex})).unwrap(), expected);
        }
    }

    #[test]
    fn rejects_errors_invalid_quantities_and_overflow() {
        for response in [
            json!({}),
            json!({"result": "42161"}),
            json!({"result": "0x0"}),
            json!({"result": "0x10000000000000000"}),
            json!({"result": 42161}),
            json!({"result": "0x1", "error": {"code": -32601}}),
        ] {
            assert!(chain_id(&response).is_err());
        }
    }
}
