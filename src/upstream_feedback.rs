#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamFeedbackClassification {
    /// HTTP 429 or Retry-After header indicates rate limiting
    RateLimited,
    /// Provider-specific busy signal in response body
    ProviderBusy,
    /// Concurrency limit exceeded (from response, not local config)
    ConcurrencyFull,
    /// Temporary unavailability (5xx, timeout, network error)
    TemporaryUnavailable,
    /// Protocol not supported by upstream
    ProtocolUnsupported,
    /// Unknown or unclassified error
    Unknown,
}

impl UpstreamFeedbackClassification {
    /// Classify upstream response based on HTTP status, headers, and body
    pub fn from_response(
        status: u16,
        headers: &reqwest::header::HeaderMap,
        body: Option<&str>,
    ) -> Self {
        if status == 429 {
            if let Some(body_text) = body {
                let body_lower = body_text.to_lowercase();

                if body_lower.contains("concurrency")
                    || body_lower.contains("concurrent")
                    || body_lower.contains("in-flight")
                {
                    return Self::ConcurrencyFull;
                }

                if body_lower.contains("rate limit")
                    || body_lower.contains("rate_limit")
                    || body_lower.contains("too many requests")
                    || body_lower.contains("token_quota")
                    || body_lower.contains("token quota")
                    || body_lower.contains("quota_failed")
                    || body_lower.contains("quota exceeded")
                    || body_lower.contains("quota_exceeded")
                {
                    return Self::RateLimited;
                }

                if body_lower.contains("busy")
                    || body_lower.contains("overloaded")
                    || body_lower.contains("capacity")
                    || body_lower.contains("throttle")
                {
                    return Self::ProviderBusy;
                }
            }

            // HTTP 429 without stronger hints is treated as rate limiting.
            return Self::RateLimited;
        }

        // Check for Retry-After header (indicates rate limiting or temporary unavailability)
        if headers.contains_key("retry-after") {
            if status == 429 {
                return Self::RateLimited;
            }
            // Retry-After on other status codes indicates temporary unavailability
            return Self::TemporaryUnavailable;
        }

        // Check for rate limit headers
        if headers.contains_key("x-ratelimit-remaining")
            || headers.contains_key("x-rate-limit-remaining")
            || headers.contains_key("ratelimit-remaining")
        {
            if status == 429 {
                return Self::RateLimited;
            }
        }

        // 5xx errors are temporary unavailability
        if status >= 500 && status < 600 {
            return Self::TemporaryUnavailable;
        }

        // 404/405 indicate protocol not supported
        if status == 404 || status == 405 {
            return Self::ProtocolUnsupported;
        }

        // Check response body for busy/rate limit indicators
        if let Some(body_text) = body {
            let body_lower = body_text.to_lowercase();

            // Check for rate limit indicators in body
            if body_lower.contains("rate limit")
                || body_lower.contains("rate_limit")
                || body_lower.contains("too many requests")
                || body_lower.contains("token_quota")
                || body_lower.contains("token quota")
                || body_lower.contains("quota_failed")
                || body_lower.contains("quota exceeded")
                || body_lower.contains("quota_exceeded")
            {
                return Self::RateLimited;
            }

            // Check for busy indicators
            if body_lower.contains("busy")
                || body_lower.contains("overloaded")
                || body_lower.contains("capacity")
                || body_lower.contains("throttle")
            {
                return Self::ProviderBusy;
            }

            // Check for concurrency full indicators
            if body_lower.contains("concurrency")
                || body_lower.contains("concurrent")
                || body_lower.contains("in-flight")
            {
                return Self::ConcurrencyFull;
            }

            // Check for protocol/feature unsupported indicators (be specific to avoid false positives)
            if body_lower.contains("unsupported response format")
                || body_lower.contains("does not support responses")
                || body_lower.contains("protocol not supported")
                || body_lower.contains("endpoint not supported")
                || body_lower.contains("streaming not supported")
                || body_lower.contains("stream not supported")
                || body_lower.contains("model not supported")
                || body_lower.contains("unsupported model")
                || body_lower.contains("model unsupported")
                || body_lower.contains("model not found")
                || body_lower.contains("model_not_found")
                || body_lower.contains("no such model")
                || (body_lower.contains("unsupported") && body_lower.contains("tool"))
                || (body_lower.contains("not supported") && body_lower.contains("feature"))
            {
                return Self::ProtocolUnsupported;
            }
        }

        // 400/422 without specific indicators are unknown
        if status == 400 || status == 422 {
            return Self::Unknown;
        }

        Self::Unknown
    }

    /// Whether this classification indicates the upstream should be cooled down
    pub fn should_cooldown(&self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::ProviderBusy | Self::ConcurrencyFull
        )
    }

    /// Whether this classification indicates a temporary issue (should retry)
    pub fn is_temporary(&self) -> bool {
        matches!(
            self,
            Self::RateLimited
                | Self::ProviderBusy
                | Self::ConcurrencyFull
                | Self::TemporaryUnavailable
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_429_is_rate_limited() {
        let headers = reqwest::header::HeaderMap::new();
        let classification = UpstreamFeedbackClassification::from_response(429, &headers, None);
        assert_eq!(classification, UpstreamFeedbackClassification::RateLimited);
    }

    #[test]
    fn test_429_with_concurrency_body_is_concurrency_full() {
        let headers = reqwest::header::HeaderMap::new();
        let classification = UpstreamFeedbackClassification::from_response(
            429,
            &headers,
            Some(r#"{"error": {"message": "concurrency limit exceeded"}}"#),
        );
        assert_eq!(
            classification,
            UpstreamFeedbackClassification::ConcurrencyFull
        );
    }

    #[test]
    fn test_retry_after_indicates_temporary() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "60".parse().unwrap());
        let classification = UpstreamFeedbackClassification::from_response(503, &headers, None);
        assert_eq!(
            classification,
            UpstreamFeedbackClassification::TemporaryUnavailable
        );
    }

    #[test]
    fn test_5xx_is_temporary() {
        let headers = reqwest::header::HeaderMap::new();
        let classification = UpstreamFeedbackClassification::from_response(503, &headers, None);
        assert_eq!(
            classification,
            UpstreamFeedbackClassification::TemporaryUnavailable
        );
    }

    #[test]
    fn test_404_is_protocol_unsupported() {
        let headers = reqwest::header::HeaderMap::new();
        let classification = UpstreamFeedbackClassification::from_response(404, &headers, None);
        assert_eq!(
            classification,
            UpstreamFeedbackClassification::ProtocolUnsupported
        );
    }

    #[test]
    fn test_model_not_supported_is_protocol_unsupported() {
        let headers = reqwest::header::HeaderMap::new();
        let classification = UpstreamFeedbackClassification::from_response(
            400,
            &headers,
            Some(r#"{"error": {"message": "model not supported"}}"#),
        );
        assert_eq!(
            classification,
            UpstreamFeedbackClassification::ProtocolUnsupported
        );
    }

    #[test]
    fn test_generic_400_is_unknown() {
        let headers = reqwest::header::HeaderMap::new();
        let classification = UpstreamFeedbackClassification::from_response(400, &headers, None);
        assert_eq!(classification, UpstreamFeedbackClassification::Unknown);
    }

    #[test]
    fn test_body_with_rate_limit_text() {
        let headers = reqwest::header::HeaderMap::new();
        let classification = UpstreamFeedbackClassification::from_response(
            400,
            &headers,
            Some(r#"{"error": "rate limit exceeded"}"#),
        );
        assert_eq!(classification, UpstreamFeedbackClassification::RateLimited);
    }

    #[test]
    fn test_body_with_busy_text() {
        let headers = reqwest::header::HeaderMap::new();
        let classification = UpstreamFeedbackClassification::from_response(
            400,
            &headers,
            Some(r#"{"error": "server is busy"}"#),
        );
        assert_eq!(classification, UpstreamFeedbackClassification::ProviderBusy);
    }
}
