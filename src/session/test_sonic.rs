#[cfg(test)]
mod test_unknown {
    #[derive(serde::Deserialize, Debug)]
    struct ClaudeUsage {
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        cache_read_input_tokens: Option<i64>,
        cache_creation_input_tokens: Option<i64>,
        cache_creation: Option<CacheCreationDetail>,
        server_tool_use: Option<ServerToolUse>,
        speed: Option<String>,
    }
    
    #[derive(serde::Deserialize, Debug, Clone)]
    struct CacheCreationDetail {
        ephemeral_5m_input_tokens: Option<i64>,
        ephemeral_1h_input_tokens: Option<i64>,
    }
    
    #[derive(serde::Deserialize, Debug, Clone)]
    struct ServerToolUse {
        web_search_requests: Option<i64>,
    }
    
    #[test]
    fn test_sonic_rejects_unknown() {
        let full = r#"{"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0},"cache_creation_input_tokens":0,"cache_read_input_tokens":34685,"inference_geo":"us","input_tokens":463,"iterations":1,"output_tokens":0,"server_tool_use":{"web_search_requests":0},"service_tier":"standard","speed":"normal"}"#;
        let result: Result<ClaudeUsage, _> = sonic_rs::from_str(full);
        eprintln!("Full: {:?}", result);
        if result.is_err() {
            eprintln!("ERROR: sonic_rs rejects unknown fields! This causes massive undercount!");
        }
    }
}
